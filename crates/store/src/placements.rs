//! Placements (ADR 0023): where a Source Set keeps each of its Games, in its own folder or in the Library Folder that
//! holds its Placed Games. A Placed Game stays its Source Set's: its Audits read its recorded files where they are
//! placed, a Repair renames them or keeps them aside there, and while any Game is placed neither the Library Folder
//! nor the Source Set can be unregistered. Builds don't place Games yet; [`Store::record_placement`] records one.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use title_select_api::build::{FileStat, PlacedFile};
use title_select_api::paths::relative_path;
use title_select_api::storage::is_reserved_name;
use title_select_engine::meter::Tally;

use crate::builds::sha256_of;
use crate::config::Config;
use crate::folder_dir::{FolderDir, file_stat};
use crate::folders::{folder_by_id, open_folder};
use crate::sets::{Resolved, resolve, set_by_id, set_by_name};
use crate::{Error, Result, Store, now};

/// The Library Folder holding Source Set `set`'s Placed Games: the one it names, while that is registered and bound
/// to it, or else the only registered Library Folder bound to it. None when it names none and several, or none, are
/// bound to it.
pub(crate) fn holding_folder(c: &Connection, set: i64) -> Result<Option<i64>> {
    let named: Option<i64> = c
        .query_row(
            "SELECT f.id FROM source_sets s JOIN library_folders f ON f.id = s.holding_folder_id
             WHERE s.id = ?1 AND f.source_set_id = s.id AND f.unregistered_at IS NULL",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    if named.is_some() {
        return Ok(named);
    }
    let bound: Vec<i64> = c
        .prepare(
            "SELECT id FROM library_folders WHERE source_set_id = ?1 AND unregistered_at IS NULL
             ORDER BY id LIMIT 2",
        )?
        .query_map([set], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(match bound.as_slice() {
        [only] => Some(*only),
        _ => None,
    })
}

const PLACED_QUERY: &str = "SELECT game, folder_id, path, size, mtime_ns, sha256 FROM placed_files";

fn placed_from_row(r: &rusqlite::Row) -> rusqlite::Result<PlacedFile> {
    Ok(PlacedFile {
        game: r.get(0)?,
        library_folder: r.get(1)?,
        path: r.get(2)?,
        file: FileStat {
            size: r.get::<_, i64>(3)? as u64,
            mtime_ns: r.get::<_, i64>(4)? as i128,
        },
        sha256: r.get(5)?,
    })
}

/// The files of Source Set `set`'s Placed Games, by Library Folder and path.
pub(crate) fn placed_files_of(c: &Connection, set: i64) -> Result<Vec<PlacedFile>> {
    let mut stmt = c.prepare(&format!(
        "{PLACED_QUERY} WHERE source_set_id = ?1 ORDER BY folder_id, path"
    ))?;
    let rows = stmt.query_map([set], placed_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The files of the Placed Games that Library Folder `folder` holds, by path.
pub(crate) fn placed_files_in(c: &Connection, folder: i64) -> Result<Vec<PlacedFile>> {
    let mut stmt = c.prepare(&format!(
        "{PLACED_QUERY} WHERE folder_id = ?1 ORDER BY path"
    ))?;
    let rows = stmt.query_map([folder], placed_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Why Source Set `set` can't be unregistered while Games of it are placed, saying how many; none while none is.
pub(crate) fn set_hold(c: &Connection, set: i64) -> Result<Option<String>> {
    let placed: i64 = c.query_row(
        "SELECT count(*) FROM placed_games WHERE source_set_id = ?1",
        [set],
        |r| r.get(0),
    )?;
    Ok((placed > 0).then(|| {
        format!(
            "{} placed in a play library folder; they must be back in the source set's folder first",
            games(placed)
        )
    }))
}

/// Why Library Folder `folder` can't be unregistered, or bound to another Source Set, while it holds Placed Games,
/// saying how many; none while it holds none.
pub(crate) fn folder_hold(c: &Connection, folder: i64) -> Result<Option<String>> {
    let held: Option<(i64, String)> = c
        .query_row(
            "SELECT count(*), s.name FROM placed_games p JOIN source_sets s ON s.id = p.source_set_id
             WHERE p.folder_id = ?1 GROUP BY s.id",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(held.map(|(placed, set)| {
        format!(
            "it holds {} of source set {set}, placed there as their only copy; they must be back in the source \
             set's folder first",
            games(placed)
        )
    }))
}

fn games(n: i64) -> String {
    match n {
        1 => "1 game".to_owned(),
        n => format!("{n} games"),
    }
}

/// Records that the Placed Game's file `from` in Library Folder `folder` is now `to` there, a file of `game` as it
/// now is: as a Repair renamed it. The Game it was a file of is no longer placed once it has no file left.
pub(crate) fn move_placed(
    tx: &Transaction,
    folder: i64,
    from: &str,
    to: &str,
    game: &str,
    file: &FileStat,
) -> Result<()> {
    let (set, was): (i64, String) = tx.query_row(
        "SELECT source_set_id, game FROM placed_files WHERE folder_id = ?1 AND path = ?2",
        params![folder, from],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO placed_games (source_set_id, game, folder_id, placed_at) VALUES (?1, ?2, ?3, ?4)",
        params![set, game, folder, now()],
    )?;
    tx.execute(
        "UPDATE placed_files SET path = ?3, game = ?4, size = ?5, mtime_ns = ?6 WHERE folder_id = ?1 AND path = ?2",
        params![
            folder,
            from,
            to,
            game,
            file.size as i64,
            file.mtime_ns as i64
        ],
    )?;
    drop_unplaced(tx, set, &was)
}

/// Forgets the Placed Game's file `path` in Library Folder `folder`, which a Repair moved into its Source Set's kept
/// files. The Game is no longer placed once it has no file left.
pub(crate) fn forget_placed(tx: &Transaction, folder: i64, path: &str) -> Result<()> {
    let found: Option<(i64, String)> = tx
        .query_row(
            "SELECT source_set_id, game FROM placed_files WHERE folder_id = ?1 AND path = ?2",
            params![folder, path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((set, game)) = found else {
        return Ok(());
    };
    tx.execute(
        "DELETE FROM placed_files WHERE folder_id = ?1 AND path = ?2",
        params![folder, path],
    )?;
    drop_unplaced(tx, set, &game)
}

/// Forgets that Source Set `set`'s Game `game` is placed once no file of it is.
fn drop_unplaced(tx: &Transaction, set: i64, game: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM placed_games WHERE source_set_id = ?1 AND game = ?2
         AND NOT EXISTS (SELECT 1 FROM placed_files f WHERE f.source_set_id = ?1 AND f.game = ?2)",
        params![set, game],
    )?;
    Ok(())
}

impl Store {
    /// The Library Folders holding Source Set `set`'s Placed Games, each with its folder resolved in its Root and the
    /// paths of the Placed Games' files in it.
    pub(crate) fn placed_folders(
        &self,
        config: &Config,
        set: i64,
    ) -> Result<Vec<(i64, Resolved, Vec<String>)>> {
        let files = self.read(|c| placed_files_of(c, set))?;
        let mut folders: Vec<(i64, Resolved, Vec<String>)> = Vec::new();
        for file in files {
            match folders.last_mut() {
                Some((folder, _, paths)) if *folder == file.library_folder => paths.push(file.path),
                _ => {
                    let row = self.read(|c| folder_by_id(c, file.library_folder))?;
                    let resolved = resolve(config, &row.root, &row.path)?;
                    folders.push((file.library_folder, resolved, vec![file.path]));
                }
            }
        }
        Ok(folders)
    }

    /// The Library Folder holding the Placed Games of Source Set `name`, if any: see [`holding_folder`].
    pub fn holding_folder(&self, name: &str) -> Result<Option<i64>> {
        self.read(|c| holding_folder(c, set_by_name(c, name)?.id))
    }

    /// Names Library Folder `folder`, which must be registered and bound to Source Set `name`, as the one holding its
    /// Placed Games; `None` names none. Refused while Games of the Source Set are placed in another folder. Changes no
    /// file.
    pub fn set_holding_folder(&self, name: &str, folder: Option<i64>) -> Result<()> {
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            if let Some(folder) = folder {
                let row = folder_by_id(tx, folder)?;
                row.check_registered()?;
                if row.source_set_id != set.id {
                    return Err(Error::Refused(format!(
                        "play library folder {}/{} is bound to source set {}, not {name}; only a folder bound to \
                         it can hold its games",
                        row.root, row.path, row.source_set
                    )));
                }
            }
            let elsewhere: Option<(i64, String, String)> = tx
                .query_row(
                    "SELECT count(*), f.root, f.path FROM placed_games p JOIN library_folders f ON f.id = p.folder_id
                     WHERE p.source_set_id = ?1 AND p.folder_id IS NOT ?2 GROUP BY f.id LIMIT 1",
                    params![set.id, folder],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((placed, root, path)) = elsewhere {
                return Err(Error::Conflict(format!(
                    "{} of source set {name} placed in play library folder {root}/{path}; they must be back in the \
                     source set's folder before another folder holds its games",
                    games(placed)
                )));
            }
            tx.execute(
                "UPDATE source_sets SET holding_folder_id = ?2 WHERE id = ?1",
                params![set.id, folder],
            )?;
            Ok(())
        })
    }

    /// The files of the Placed Games of Source Set `name`, by Library Folder and path.
    pub fn placed_files(&self, name: &str) -> Result<Vec<PlacedFile>> {
        self.read(|c| placed_files_of(c, set_by_name(c, name)?.id))
    }

    /// Records Source Set `name`'s Game `game` as placed in Library Folder `folder`, the one holding its Placed Games,
    /// as the files at `paths` there, each relative to it; each is read whole to record its SHA-256. Moves no file:
    /// the Game's files must already be there. What a Build that places a Game records once it has moved it; for
    /// tests until one does.
    #[doc(hidden)]
    pub fn record_placement(
        &self,
        config: &Config,
        name: &str,
        game: &str,
        folder: i64,
        paths: &[&str],
    ) -> Result<()> {
        let (set, row) = self.read(|c| Ok((set_by_name(c, name)?, folder_by_id(c, folder)?)))?;
        row.check_registered()?;
        if paths.is_empty() {
            return Err(Error::Refused(format!(
                "{game}: a placed game has at least one file"
            )));
        }
        let dir = open_folder(config, &row.root, &row.path).map_err(Error::Refused)?;
        let mut files = Vec::new();
        for path in paths {
            let rel = relative_path(path).map_err(|e| Error::Refused(format!("{path}: {e}")))?;
            if rel.split('/').any(is_reserved_name) {
                return Err(Error::Refused(format!(
                    "{path} is under a name Title Select keeps its own work under"
                )));
            }
            files.push((rel.clone(), read(&dir, &rel)?));
        }
        self.write(|tx| {
            if holding_folder(tx, set.id)? != Some(folder) {
                return Err(Error::Refused(format!(
                    "play library folder {}/{} does not hold source set {name}'s placed games",
                    row.root, row.path
                )));
            }
            set_by_id(tx, set.id)?;
            tx.execute(
                "INSERT INTO placed_games (source_set_id, game, folder_id, placed_at) VALUES (?1, ?2, ?3, ?4)",
                params![set.id, game, folder, now()],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                    Error::Conflict(format!("{game} of source set {name} is already placed"))
                }
                e => e.into(),
            })?;
            for (path, (file, sha256)) in &files {
                tx.execute(
                    "INSERT INTO placed_files (folder_id, path, source_set_id, game, size, mtime_ns, sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        folder,
                        path,
                        set.id,
                        game,
                        file.size as i64,
                        file.mtime_ns as i64,
                        sha256
                    ],
                )
                .map_err(|e| match e {
                    rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                        Error::Conflict(format!("{path} is already a file of a placed game"))
                    }
                    e => e.into(),
                })?;
            }
            Ok(())
        })
    }
}

/// The regular file `rel` in `dir`, as it is, with the SHA-256 of all of it.
fn read(dir: &FolderDir, rel: &str) -> Result<(FileStat, String)> {
    let io = |e: std::io::Error| Error::Io(format!("{}: {e}", dir.path().join(rel).display()));
    let file = dir.open_file(rel).map_err(io)?;
    let st = rustix::fs::fstat(&file).map_err(|e| io(e.into()))?;
    // Recorded for you, by no Job: no Job's meter counts it.
    let (sha256, _) = sha256_of(&file, &Tally::default()).map_err(io)?;
    Ok((file_stat(&st), sha256))
}

/// The Library Folders an Audit of Source Set `set` reads besides its own folder: each that holds one of its Placed
/// Games. Until a Build places Games (#214), a Build of a holding folder that holds none changes nothing it reads.
fn read_folders(c: &Connection, set: i64) -> Result<Vec<i64>> {
    Ok(
        c.prepare("SELECT DISTINCT folder_id FROM placed_games WHERE source_set_id = ?1")?
            .query_map([set], |r| r.get(0))?
            .collect::<Result<_, _>>()?,
    )
}

/// Refuses to start an Audit of Source Set `set_name` (`set`) while a Build or cleanup of a Library Folder holding
/// its Placed Games is queued or running: it would change files the Audit reads. Called in the transaction that
/// queues or starts the Audit.
pub(crate) fn check_no_holding_build(c: &Connection, set: i64, set_name: &str) -> Result<()> {
    for folder in read_folders(c, set)? {
        let job: Option<i64> = c
            .query_row(
                "SELECT id FROM jobs WHERE library_folder_id = ?1 AND kind = 'build'
                 AND state IN ('queued', 'running')",
                [folder],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(job) = job {
            return Err(Error::Conflict(format!(
                "build job {job} of the play library folder holding source set {set_name}'s placed games is queued \
                 or running; audit the source set once it ends"
            )));
        }
    }
    Ok(())
}

/// Refuses to queue a Build or cleanup of Library Folder `folder` while it holds Placed Games of a Source Set an
/// Audit is queued for or reading: it would change files the Audit reads. Called in the transaction that queues the
/// Build.
pub(crate) fn check_no_audit_reading(c: &Connection, folder: i64) -> Result<()> {
    let set: i64 = c.query_row(
        "SELECT source_set_id FROM library_folders WHERE id = ?1",
        [folder],
        |r| r.get(0),
    )?;
    if !read_folders(c, set)?.contains(&folder) {
        return Ok(());
    }
    let audit: Option<String> = c
        .query_row(
            "SELECT 'audit job ' || id || ' of source set ' || (SELECT name FROM source_sets WHERE id = ?1)
             FROM jobs WHERE source_set_id = ?1 AND kind = 'audit' AND state IN ('queued', 'running')
             UNION ALL
             SELECT 'audit ' || id || ' of source set ' || (SELECT name FROM source_sets WHERE id = ?1)
             FROM audits WHERE source_set_id = ?1 AND status = 'running'
             LIMIT 1",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    match audit {
        Some(audit) => Err(Error::Conflict(format!(
            "{audit}, whose placed games this folder holds, is queued or running; build once it ends"
        ))),
        None => Ok(()),
    }
}
