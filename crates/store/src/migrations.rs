//! Embedded, forward-only migrations. Each runs in a transaction, after a `VACUUM INTO` backup of a database
//! that already has a schema. The first is the baseline (ADR 0028): later ones are appended, never flattened.

use std::io::Read;
use std::path::Path;

use rusqlite::Connection;

use super::{BACKUPS, Error, Result, io_error};

/// The `PRAGMA application_id` the baseline stamps every store with: "SSel" in ASCII. A store with a schema and
/// another id was made by an earlier release, before the baseline or before the rename (ADR 0030), and is refused.
pub(crate) const APPLICATION_ID: i32 = 0x5353_656C;

/// Applied in order; `PRAGMA user_version` counts those already applied.
pub(super) const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_baseline.sql")];

/// What a store made by an earlier release is refused with.
pub(crate) const PRE_BASELINE: &str =
    "This store was made by an earlier release and can't be opened. Start a new store.";

/// Refuses the database at `path` if its header says it has a schema but not the baseline's stamp. It reads the
/// header's bytes rather than opening the database, as even a read-only SQLite connection may create a WAL's sidecars,
/// so a refused store is left exactly as it was. The header is the newest committed page one in the database's WAL,
/// if it has one, as SQLite would read it, else the database file's. A missing or empty file passes, as does one that
/// isn't a SQLite database: opening it reports that.
pub(super) fn check_header(path: &Path) -> Result<()> {
    let mut wal_path = path.as_os_str().to_owned();
    wal_path.push("-wal");
    let wal_path = std::path::PathBuf::from(wal_path);
    let header = match committed_page_one(&wal_path)? {
        Some(header) => header,
        None => {
            let mut header = [0; 100];
            let read = match std::fs::File::open(path) {
                Ok(file) => file.take(100).read(&mut header),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(io_error(path, e)),
            };
            // A short read leaves the rest zero, so a header cut short fails the magic or reads unstamped and
            // unversioned.
            read.map_err(|e| io_error(path, e))?;
            header
        }
    };
    if &header[..16] != b"SQLite format 3\0" {
        return Ok(());
    }
    let field = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
    check_stamp(i64::from(field(60)), field(68) as i32)
}

/// The first 100 bytes of page one as the last commit in the WAL at `path` left it, or `None` if there is no WAL or
/// no valid commit in it wrote page one. Frames are read as SQLite recovers a WAL: in order while their salts match
/// the header's and their running checksum holds, and only up to the last commit frame.
fn committed_page_one(path: &Path) -> Result<Option<[u8; 100]>> {
    const HEADER: usize = 32;
    const FRAME_HEADER: usize = 24;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path, e)),
    };
    let mut wal = std::io::BufReader::new(file);
    let mut header = [0; HEADER];
    if !read_all(&mut wal, &mut header).map_err(|e| io_error(path, e))? {
        return Ok(None);
    }
    let word = |bytes: &[u8], at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    let big_endian = match word(&header, 0) {
        0x377f_0682 => false,
        0x377f_0683 => true,
        _ => return Ok(None),
    };
    let page_size = word(&header, 8) as usize;
    if !(512..=65536).contains(&page_size) || !page_size.is_power_of_two() {
        return Ok(None);
    }
    let checksum = |seed: (u32, u32), bytes: &[u8]| {
        let (mut s0, mut s1) = seed;
        for pair in bytes.as_chunks::<8>().0 {
            let read = |at: usize| {
                let w: [u8; 4] = pair[at..at + 4].try_into().unwrap();
                if big_endian {
                    u32::from_be_bytes(w)
                } else {
                    u32::from_le_bytes(w)
                }
            };
            s0 = s0.wrapping_add(read(0)).wrapping_add(s1);
            s1 = s1.wrapping_add(read(4)).wrapping_add(s0);
        }
        (s0, s1)
    };
    let mut sum = checksum((0, 0), &header[..24]);
    if sum != (word(&header, 24), word(&header, 28)) {
        return Ok(None);
    }
    let salts = &header[16..24];
    let mut frame = vec![0; FRAME_HEADER + page_size];
    let (mut pending, mut committed) = (None, None);
    while read_all(&mut wal, &mut frame).map_err(|e| io_error(path, e))? {
        if &frame[8..16] != salts {
            break;
        }
        sum = checksum(checksum(sum, &frame[..8]), &frame[FRAME_HEADER..]);
        if sum != (word(&frame, 16), word(&frame, 20)) {
            break;
        }
        if word(&frame, 0) == 1 {
            pending = Some(frame[FRAME_HEADER..FRAME_HEADER + 100].try_into().unwrap());
        }
        if word(&frame, 4) != 0 {
            committed = pending.or(committed);
            pending = None;
        }
    }
    Ok(committed)
}

/// Fills `buf` from `from`, or returns `false` if `from` ends first.
fn read_all(from: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    match from.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// Refuses a database at schema `version` that isn't stamped `id` with the baseline's. The baseline sets both in one
/// transaction, so a store can't be seen with one and not the other.
pub(super) fn check_stamp(version: i64, id: i32) -> Result<()> {
    if version > 0 && id != APPLICATION_ID {
        return Err(Error::Refused(PRE_BASELINE.into()));
    }
    Ok(())
}

pub(super) fn migrate(conn: &mut Connection, dir: &Path, migrations: &[&str]) -> Result<()> {
    let applied: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let applied = usize::try_from(applied).unwrap_or(usize::MAX);
    if applied > migrations.len() {
        return Err(Error::Refused(format!(
            "{}: the store has schema version {applied}, newer than this Stage Select's {}",
            dir.display(),
            migrations.len()
        )));
    }
    if applied == migrations.len() {
        return Ok(());
    }
    // A migration may rebuild a table others refer to, which SQLite allows only with foreign keys off; each checks
    // them before it commits instead. The pragma is a no-op inside a transaction, so it is set around them.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let migrated = apply(conn, dir, migrations, applied);
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrated
}

fn apply(conn: &mut Connection, dir: &Path, migrations: &[&str], applied: usize) -> Result<()> {
    for (version, sql) in migrations.iter().enumerate().skip(applied) {
        // A store opened with no schema has nothing to protect.
        if applied > 0 {
            backup(conn, dir, version)?;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if violations > 0 {
            return Err(Error::Io(format!(
                "{}: migration {} would leave {violations} broken references",
                dir.display(),
                version + 1
            )));
        }
        tx.pragma_update(None, "user_version", version as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Writes a copy of the database at schema `version` to `backups/`.
fn backup(conn: &Connection, dir: &Path, version: usize) -> Result<()> {
    let backups = dir.join(BACKUPS);
    std::fs::create_dir_all(&backups).map_err(|e| io_error(&backups, e))?;
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%S%.fZ");
    let path = backups.join(format!("schema-{version}-{stamp}.sqlite"));
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
    Ok(())
}
