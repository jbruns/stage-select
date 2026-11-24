//! Embedded, forward-only migrations. Each runs in a transaction, after a `VACUUM INTO` backup of a database
//! that already has a schema. The first is the baseline (ADR 0028): later ones are appended, never flattened.

use std::path::Path;

use rusqlite::Connection;

use super::{BACKUPS, Error, Result, io_error};

/// The `PRAGMA application_id` the baseline stamps every store with: "SSel" in ASCII. A store with a schema and
/// another id was made by an earlier release, before the baseline or before the rename (ADR 0030), and is refused.
pub(crate) const APPLICATION_ID: i32 = 0x5353_656C;

/// Applied in order; `PRAGMA user_version` counts those already applied.
pub(super) const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_baseline.sql"),
    include_str!("migrations/0002_ingest_kept_deletions.sql"),
    include_str!("migrations/0003_removed_games.sql"),
    include_str!("migrations/0004_created_folders.sql"),
];

/// What a store made by an earlier release is refused with.
pub(crate) const PRE_BASELINE: &str =
    "This store was made by an earlier release and can't be opened. Start a new store.";

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
