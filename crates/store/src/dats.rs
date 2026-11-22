//! DAT import and DAT blob retention.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::blobs::{remove_blob, write_blob};
use super::{DATS, Error, Result, Store, io_error, now};
use title_select_api::dat::{known_provider, provider_part};
use title_select_engine::dat::Dat;

pub use title_select_api::{DatFormat, DatRevisionRecord, ImportOutcome};

pub(super) const REVISION_COLUMNS: &str = "r.id, r.sha256, d.provider, d.name, r.version, r.file_name, \
     r.imported_at, r.source_url, r.blob_dropped_at IS NULL, r.format, r.description, r.author, r.homepage, \
     r.header_url, r.header_date";

pub(super) fn revision_from_row(row: &Row) -> rusqlite::Result<(i64, DatRevisionRecord)> {
    let provider: String = row.get(2)?;
    let name: String = row.get(3)?;
    let format: String = row.get(9)?;
    let format = DatFormat::parse(&format).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            9,
            rusqlite::types::Type::Text,
            format!("{format:?} is not a DAT format").into(),
        )
    })?;
    let date: String = row.get(14)?;
    Ok((
        row.get(0)?,
        DatRevisionRecord {
            sha256: row.get(1)?,
            dat: format!("{provider}/{name}"),
            format,
            provider: known_provider(&provider).map(str::to_owned),
            name,
            description: row.get(10)?,
            author: row.get(11)?,
            homepage: row.get(12)?,
            header_url: row.get(13)?,
            version: row.get(4)?,
            date: (!date.is_empty()).then_some(date),
            file: row.get(5)?,
            imported_at: row.get(6)?,
            source_url: row.get(7)?,
            blob_present: row.get(8)?,
            blockers: None,
        },
    ))
}

pub(super) fn revision_by_id(c: &Connection, id: i64) -> Result<(i64, DatRevisionRecord)> {
    let sql = format!(
        "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id WHERE r.id = ?1"
    );
    c.query_row(&sql, [id], revision_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no DAT revision {id}")))
}

/// The catalogued DAT Revision with this SHA-256; a deleted one is not catalogued.
fn revision_by_sha(c: &Connection, sha256: &str) -> Result<Option<(i64, DatRevisionRecord)>> {
    let sql = format!(
        "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id
         WHERE r.sha256 = ?1 AND r.deleted_at IS NULL"
    );
    Ok(c.query_row(&sql, [sha256], revision_from_row).optional()?)
}

/// Whether a deleted DAT Revision with this SHA-256 keeps its row for history.
fn deleted_by_sha(c: &Connection, sha256: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM dat_revisions WHERE sha256 = ?1 AND deleted_at IS NOT NULL)",
        [sha256],
        |r| r.get(0),
    )?)
}

/// A run of digits, compared by the number it writes, however long: its digits without leading zeros, by count and
/// then one by one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Number(usize, String);

/// What orders a DAT's revisions by their header's version, or date: its runs of digits, as numbers. It reads the
/// versions providers write the same way: No-Intro's `20240101-123456`, Redump's `2024-01-01 12-34-56`, MAME's
/// `0.265 (mame0265)` and its tags. Text with no digits tells nothing.
fn version_key(version: &str) -> Vec<Number> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .map(|run| {
            let digits = run.trim_start_matches('0');
            Number(digits.len(), digits.to_owned())
        })
        .collect()
}

/// The date and time `text` writes, as `[year, month, day, hour, minute, second]`, if it starts with one: a date as
/// `20240101`, `2024-01-01` or `2024 01 01`, then optionally a time as `123456` or `12-34-56`, as No-Intro and Redump
/// write their versions and DATs their dates. A missing time is midnight.
fn timestamp(text: &str) -> Option<[u32; 6]> {
    let runs: Vec<&str> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .collect();
    let split = |run: &str, widths: &[usize]| -> Option<Vec<u32>> {
        (run.len() == widths.iter().sum::<usize>()).then(|| {
            let mut at = 0;
            widths
                .iter()
                .map(|w| {
                    at += w;
                    run[at - w..at].parse().unwrap_or(0)
                })
                .collect()
        })
    };
    let (date, rest) = match runs.as_slice() {
        [run, rest @ ..] if run.len() == 8 => (split(run, &[4, 2, 2])?, rest),
        [y, m, d, rest @ ..] if y.len() == 4 && m.len() <= 2 && d.len() <= 2 => (
            vec![y.parse().ok()?, m.parse().ok()?, d.parse().ok()?],
            rest,
        ),
        _ => return None,
    };
    let time = match rest {
        [run, ..] if run.len() == 6 => split(run, &[2, 2, 2])?,
        [h, m, s, ..] if [h, m, s].iter().all(|r| r.len() <= 2) => {
            vec![h.parse().ok()?, m.parse().ok()?, s.parse().ok()?]
        }
        _ => vec![0, 0, 0],
    };
    let at = [date[0], date[1], date[2], time[0], time[1], time[2]];
    let valid = (1..=12).contains(&at[1])
        && (1..=31).contains(&at[2])
        && at[3] < 24
        && at[4] < 60
        && at[5] < 61;
    valid.then_some(at)
}

/// Where a revision's header places it among its DAT's revisions, one key per revision so that the order is total
/// and transitive: by when it was made, then by its version's numbers. Its time is the one its version writes, as
/// No-Intro's and Redump's do, else its header's date. A part the header lacks ranks below any it gives: an undated
/// revision is older than a dated one, and a version without digits older than one with. Revisions whose keys are
/// equal keep their import order, the later the newer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OrderKey {
    at: Option<[u32; 6]>,
    version: Vec<Number>,
}

impl OrderKey {
    fn new(version: &str, date: &str) -> OrderKey {
        OrderKey {
            at: timestamp(version).or_else(|| timestamp(date)),
            version: version_key(version),
        }
    }
}

/// The DAT Format of the DAT `dat_id`'s newest revision, or `None` before it has one.
pub(crate) fn dat_format(c: &Connection, dat_id: i64) -> Result<Option<DatFormat>> {
    let format: Option<String> = c
        .query_row(
            "SELECT format FROM dat_revisions WHERE dat_id = ?1 AND deleted_at IS NULL
             ORDER BY import_seq DESC LIMIT 1",
            [dat_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(format.as_deref().and_then(DatFormat::parse))
}

/// Refuses a Selection, Library Folder or Build of the DAT `dat_id` when it is a MAME software list (ADR 0027's
/// amendment).
pub(crate) fn check_selectable(c: &Connection, dat_id: i64) -> Result<()> {
    match dat_format(c, dat_id)? {
        Some(DatFormat::SoftwareList) => Err(Error::Refused(
            title_select_api::selection::SOFTWARE_LIST_REFUSAL.to_owned(),
        )),
        _ => Ok(()),
    }
}

/// Refuses Coverage for the DAT `dat_id` when it is a MAME ListXML (ADR 0027).
pub(crate) fn check_coverable(c: &Connection, dat_id: i64, coverage: Option<&str>) -> Result<()> {
    match (coverage, dat_format(c, dat_id)?) {
        (Some(_), Some(DatFormat::Listxml)) => Err(Error::Refused(
            title_select_api::selection::ARCADE_COVERAGE_REFUSAL.to_owned(),
        )),
        _ => Ok(()),
    }
}

/// The DAT `<provider>/<name>`, spelled exactly so.
pub(crate) fn find_dat(c: &Connection, provider: &str, name: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM dats WHERE provider = ?1 AND name = ?2",
        params![provider, name],
        |r| r.get(0),
    )
    .optional()?)
}

/// The DAT `<provider>/<name>`, spelled exactly so, if any revision of it is catalogued: one whose every revision was
/// deleted has left the catalogue, though history may still name it.
pub(crate) fn catalogued_dat(c: &Connection, provider: &str, name: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT d.id FROM dats d WHERE d.provider = ?1 AND d.name = ?2
           AND EXISTS (SELECT 1 FROM dat_revisions r WHERE r.dat_id = d.id AND r.deleted_at IS NULL)",
        params![provider, name],
        |r| r.get(0),
    )
    .optional()?)
}

/// The one spelling the store keeps of `provider`: that of the DATs already imported from a provider of that name
/// whatever its case, else `provider` as given. Every provider that comes in, from an import, a registration, a fetch
/// or a request, is spelled so before it is stored or looked up, so that every other match is exact.
pub(crate) fn canonical_provider(c: &Connection, provider: &str) -> Result<String> {
    Ok(c.query_row(
        "SELECT provider FROM dats WHERE provider = ?1 COLLATE NOCASE ORDER BY id LIMIT 1",
        [provider],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or_else(|| provider.to_owned()))
}

/// `dat`, as `<provider>/<name>`, with its provider spelled as [`canonical_provider`] keeps it; as given if it names
/// no provider.
pub(crate) fn canonical_dat(c: &Connection, dat: &str) -> Result<String> {
    Ok(match dat.split_once('/') {
        Some((provider, name)) => format!("{}/{name}", canonical_provider(c, provider)?),
        None => dat.to_owned(),
    })
}

/// Renumbers `header_order` within the DAT `dat_id`'s catalogued revisions by [`OrderKey`], then import order
/// (`import_seq`, which a revision imported again after deletion takes afresh), from 1; a deleted revision keeps 0,
/// below them all.
pub(crate) fn reorder_revisions(c: &Connection, dat_id: i64) -> Result<()> {
    reorder(
        c,
        "SELECT id, version, header_date, import_seq FROM dat_revisions
         WHERE dat_id = ?1 AND deleted_at IS NULL",
        dat_id,
    )
}

fn reorder(c: &Connection, select: &str, dat_id: i64) -> Result<()> {
    let mut stmt = c.prepare(select)?;
    let revisions = stmt
        .query_map([dat_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ordered: Vec<(OrderKey, i64, i64)> = revisions
        .into_iter()
        .map(|(id, version, date, seq)| (OrderKey::new(&version, &date), seq, id))
        .collect();
    ordered.sort();
    for (order, (_, _, id)) in ordered.iter().enumerate() {
        c.execute(
            "UPDATE dat_revisions SET header_order = ?2 WHERE id = ?1",
            params![id, order as i64 + 1],
        )?;
    }
    Ok(())
}

/// Whether a revision of the DAT `<provider>/<name>`, its provider spelled as the store keeps it, at `version` and
/// `date`, not yet imported, would be its newest, as [`reorder_revisions`] places it: true when the DAT has no
/// revision yet.
pub(crate) fn would_be_newest(
    c: &Connection,
    provider: &str,
    name: &str,
    version: &str,
    date: &str,
) -> Result<bool> {
    let key = OrderKey::new(version, date);
    let mut stmt = c.prepare(
        "SELECT r.version, r.header_date FROM dats d JOIN dat_revisions r ON r.dat_id = d.id
         WHERE d.provider = ?1 AND d.name = ?2 AND r.deleted_at IS NULL",
    )?;
    let headers = stmt
        .query_map(params![provider, name], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Imported after every one, it is newer than any whose key is equal.
    Ok(headers.iter().all(|(v, d)| OrderKey::new(v, d) <= key))
}

/// The provider part of the key `dat` is a revision of, its provider spelled as the store keeps it: `fetched`, the
/// provider a fetch took it from, else the one its format and header signal, else the part reserved for its format.
pub(crate) fn dat_provider_part(
    c: &Connection,
    dat: &Dat,
    fetched: Option<&str>,
) -> Result<String> {
    let part = provider_part(fetched.or_else(|| dat.provider()), dat.format);
    canonical_provider(c, &part)
}

/// Splits `<provider>/<name>`. A provider never contains `/`; a DAT's name may.
pub(super) fn split_dat(dat: &str) -> Result<(&str, &str)> {
    dat.split_once('/')
        .filter(|(p, n)| !p.is_empty() && !n.is_empty())
        .ok_or_else(|| Error::Refused(format!("{dat}: a DAT is named as <provider>/<name>")))
}

impl Store {
    /// Imports the DAT file at `path` as a DAT Revision, reading its DAT Format, native name, provider and header
    /// claims from the file itself. `source_url` records where it was downloaded from. Importing bytes already
    /// imported returns the existing DAT Revision, restoring its blob if it was dropped.
    pub fn import_dat(
        &self,
        path: &Path,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
        let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.import_dat_bytes(&bytes, &file, source_url)
    }

    /// Imports the unmodified bytes of a DAT file whose original filename is `file`, like [`Store::import_dat`].
    pub fn import_dat_bytes(
        &self,
        bytes: &[u8],
        file: &str,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
        let dat = Dat::parse(bytes, Path::new(file))?;
        self.import_parsed_dat(&dat, bytes, file, None, source_url)
    }

    /// As [`Store::import_dat_bytes`], for `bytes` already parsed as `dat`. A revision fetched from a DAT Provider
    /// (ADR 0012) passes it as `fetched`, and takes that provider rather than its header's.
    pub(crate) fn import_parsed_dat(
        &self,
        dat: &Dat,
        bytes: &[u8],
        file: &str,
        fetched: Option<&str>,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
        dat.check_name(Path::new(file))?;
        let dats = self.dir.join(DATS);
        // Holding the writer keeps the blob and its row consistent against retention and garbage collection.
        let (record, outcome) = self.with_writer(|conn| {
            if let Some((id, existing)) = revision_by_sha(conn, &dat.sha256)? {
                // A fetched revision must be of the DAT it was fetched for; bytes already kept as another DAT's
                // revision are never silently moved to it.
                if fetched.is_some() {
                    let expected = format!("{}/{}", dat_provider_part(conn, dat, fetched)?, dat.name);
                    if existing.dat != expected {
                        return Err(Error::Conflict(format!(
                            "{file} is already imported as a revision of {}, so it isn't added to {expected}",
                            existing.dat
                        )));
                    }
                }
                let blob = self.blob_path(&existing.sha256);
                if existing.blob_present && blob.is_file() {
                    return Ok((existing, ImportOutcome::AlreadyImported));
                }
                write_blob(&dats, &dat.sha256, bytes)?;
                conn.execute(
                    "UPDATE dat_revisions SET blob_dropped_at = NULL, restored_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
                let restored = DatRevisionRecord {
                    blob_present: true,
                    ..existing
                };
                return Ok((restored, ImportOutcome::Restored));
            }
            write_blob(&dats, &dat.sha256, bytes)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Bytes whose revision was deleted come back as a fresh import, in the row history still names.
            let deleted = deleted_by_sha(&tx, &dat.sha256)?;
            let watch = crate::observe::StaleWatch::start(&tx)?;
            let provider = dat_provider_part(&tx, dat, fetched)?;
            let dat_id = match find_dat(&tx, &provider, &dat.name)? {
                Some(id) => id,
                None => {
                    tx.execute(
                        "INSERT INTO dats (provider, name) VALUES (?1, ?2)",
                        params![provider, dat.name],
                    )?;
                    let id = tx.last_insert_rowid();
                    // The provider's first DAT fixes its spelling for overrides saved before it.
                    crate::storage::respell_overrides(&tx)?;
                    id
                }
            };
            // A newer revision releases any restored revision of the same DAT.
            tx.execute(
                "UPDATE dat_revisions SET restored_at = NULL WHERE dat_id = ?1",
                [dat_id],
            )?;
            let sql = if deleted {
                    "UPDATE dat_revisions
                     SET dat_id = ?1, version = ?3, header_date = ?4, file_name = ?5, imported_at = ?6,
                         source_url = ?7, format = ?8, description = ?9, author = ?10, homepage = ?11,
                         header_url = ?12, blob_dropped_at = NULL, restored_at = NULL, deleted_at = NULL,
                         import_seq = (SELECT max(import_seq) + 1 FROM dat_revisions)
                     WHERE sha256 = ?2"
            } else {
                    "INSERT INTO dat_revisions (dat_id, sha256, version, header_date, file_name, imported_at,
                                                source_url, format, description, author, homepage, header_url,
                                                import_seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                             (SELECT coalesce(max(import_seq), 0) + 1 FROM dat_revisions))"
            };
            tx.execute(
                sql,
                params![
                    dat_id,
                    dat.sha256,
                    dat.version,
                    dat.date,
                    file,
                    now(),
                    source_url,
                    dat.format.as_str(),
                    dat.description,
                    dat.author,
                    dat.homepage,
                    dat.url
                ],
            )?;
            reorder_revisions(&tx, dat_id)?;
            let record = revision_by_sha(&tx, &dat.sha256)?.expect("just inserted").1;
            crate::stale::note_followed_revisions(&tx)?;
            crate::observe::note(crate::observe::Observed::DatRevision {
                dat: record.dat.clone(),
                version: record.version.clone(),
            });
            watch.finish(&tx)?;
            tx.commit()?;
            Ok((record, ImportOutcome::Added))
        })?;
        if outcome == ImportOutcome::Added {
            self.retain_blobs()?;
        }
        Ok((record, outcome))
    }

    /// The DAT Revision whose SHA-256 is `sha256`, and its kept blob opened for reading, or `None` when the blob was
    /// dropped (ADR 0003). The blob is opened before anything reads it, so retention dropping it meanwhile can't cut
    /// the read short; one dropped before it is opened is not kept. A `sha256` that is not 64 lowercase hex digits is
    /// refused before any file is touched.
    pub fn open_dat_file(
        &self,
        sha256: &str,
    ) -> Result<(DatRevisionRecord, Option<std::fs::File>)> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(Error::Refused(
                "a DAT revision is named by its SHA-256, 64 lowercase hex digits".into(),
            ));
        }
        let (_, record) = self
            .read(|c| revision_by_sha(c, sha256))?
            .ok_or_else(|| Error::NotFound("no such DAT revision".into()))?;
        if !record.blob_present {
            return Ok((record, None));
        }
        let path = self.blob_path(sha256);
        match std::fs::File::open(&path) {
            Ok(file) => Ok((record, Some(file))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((record, None)),
            Err(e) => Err(io_error(&path, e)),
        }
    }

    /// Every catalogued DAT Revision, oldest first by header version within each DAT, the DATs in import order,
    /// each with what stops you deleting it.
    pub fn dat_revisions(&self) -> Result<Vec<DatRevisionRecord>> {
        self.read(|c| {
            let sql = format!(
                "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id
                 WHERE r.deleted_at IS NULL
                 ORDER BY d.id, r.header_order, r.id"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt
                .query_map([], revision_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            let mut blockers = crate::dat_deletion::blockers_of(c, None)?;
            Ok(rows
                .into_iter()
                .map(|(id, mut rev)| {
                    rev.blockers = Some(blockers.remove(&id).unwrap_or_default());
                    rev
                })
                .collect())
        })
    }

    /// Drops the blob of every DAT Revision that no retained or running Audit, no Queued or Running Job, and no
    /// Audit a DAT import from an Ingest Folder still owes refers to, that is not the newest revision of its DAT (ADR 0003), and that is not held as restored. The row
    /// stays. Cue packs are retained alike ([`Store::retain_cue_packs`]). Returns the SHA-256s whose DAT blobs were dropped.
    pub fn retain_blobs(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // An Audit row that is not pruned, including one still running, holds its DAT Revision; so does a
            // Selection that is not pruned.
            let shas = tx
                .prepare(
                    "SELECT r.sha256 FROM dat_revisions r
                     WHERE r.blob_dropped_at IS NULL AND r.restored_at IS NULL
                       AND r.id <> (SELECT n.id FROM dat_revisions n
                                    WHERE n.dat_id = r.dat_id AND n.deleted_at IS NULL
                                    ORDER BY n.header_order DESC LIMIT 1)
                       AND NOT EXISTS (
                           SELECT 1 FROM audits a WHERE a.dat_revision_id = r.id AND a.pruned_at IS NULL)
                       AND NOT EXISTS (
                           SELECT 1 FROM jobs j WHERE j.dat_revision_id = r.id AND j.state IN ('queued', 'running'))
                       AND NOT EXISTS (
                           SELECT 1 FROM selections s WHERE s.dat_revision_id = r.id AND s.pruned_at IS NULL)
                       AND NOT EXISTS (
                           SELECT 1 FROM ingest_reaudits o WHERE o.sha256 = r.sha256 AND o.fulfilled_at IS NULL)
                     ORDER BY r.id",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let at = now();
            for sha in &shas {
                tx.execute(
                    "UPDATE dat_revisions SET blob_dropped_at = ?1 WHERE sha256 = ?2",
                    params![at, sha],
                )?;
            }
            tx.commit()?;
            // The rows no longer hold these blobs, so a crash before this point leaves only garbage. The
            // writer is still held, so no restore can clear a marker in between; check it anyway before deleting.
            let mut dropped = Vec::new();
            for sha in shas {
                let still_dropped: bool = conn.query_row(
                    "SELECT blob_dropped_at IS NOT NULL FROM dat_revisions WHERE sha256 = ?1",
                    [&sha],
                    |r| r.get(0),
                )?;
                if still_dropped {
                    remove_blob(&self.blob_path(&sha))?;
                    dropped.push(sha);
                }
            }
            Ok(dropped)
        })
        .and_then(|dropped| {
            self.retain_cue_packs()?;
            self.retain_sbi_packs()?;
            Ok(dropped)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{OrderKey, version_key};

    #[test]
    fn versions_order_as_each_provider_writes_them() {
        let ordered = |older: &str, newer: &str| version_key(older) < version_key(newer);
        assert!(ordered("20231231-235959", "20240101-000000"), "No-Intro");
        assert!(
            ordered("2023-12-31 23-59-59", "2024-01-01 00-00-00"),
            "Redump"
        );
        assert!(
            ordered("0.99 (mame099)", "0.265 (mame0265)"),
            "MAME ListXML"
        );
        assert!(ordered("mame0099", "mame0265"), "a MAME tag");
        assert!(ordered("1.9", "1.10"));
        assert!(version_key("").is_empty());
        assert_eq!(version_key("v1.02"), version_key("1.2"));
    }

    #[test]
    fn a_version_that_writes_a_date_orders_as_that_date_before_any_number() {
        let key = |version: &str, date: &str| OrderKey::new(version, date);
        assert!(key("20231231-235959", "") < key("2024-01-01 00-00-00", ""));
        assert!(key("20240101-000000", "") < key("2024-01-01 00-00-01", ""));
        assert!(
            key("9", "2024-01-01") < key("1", "2024-01-02"),
            "the date first"
        );
        assert!(
            key("1", "2024-01-01") < key("2", "2024-01-01"),
            "then the version"
        );
        assert!(
            key("99", "") < key("1", "2024-01-01"),
            "an undated revision is older"
        );
        assert!(
            key("", "") < key("1", ""),
            "a version without digits is older"
        );
        assert_eq!(key("2024-01-01", "").at, key("", "2024-01-01").at);
        assert!(key("0.265 (mame0265)", "") > key("0.99 (mame099)", ""));
        assert_eq!(key("2024-13-45", "").at, None, "not a date");
    }

    #[test]
    fn the_order_of_revisions_is_a_total_order() {
        let parts = [
            "",
            "1",
            "2",
            "10",
            "20240101-000000",
            "2024-02-01",
            "2024-03-01 10-00-00",
            "mame0265",
        ];
        let keys: Vec<OrderKey> = parts
            .iter()
            .flat_map(|v| parts.iter().map(move |d| OrderKey::new(v, d)))
            .collect();
        for a in &keys {
            for b in &keys {
                assert_eq!(a.cmp(b), b.cmp(a).reverse(), "antisymmetric");
                for c in &keys {
                    if a <= b && b <= c {
                        assert!(a <= c, "transitive: {a:?} {b:?} {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn numbers_too_long_for_any_integer_still_order_by_value() {
        let huge = "1".repeat(60);
        let ordered = |older: &str, newer: &str| version_key(older) < version_key(newer);
        assert!(ordered(&format!("{huge}8"), &format!("{huge}9")));
        assert!(ordered(&huge, &format!("{huge}0")));
        assert!(ordered(&format!("v{huge}"), &format!("2{huge}")));
        assert_eq!(version_key(&format!("000{huge}")), version_key(&huge));
    }
}
