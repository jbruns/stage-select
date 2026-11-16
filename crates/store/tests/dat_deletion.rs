//! Deleting a DAT Revision nothing depends on (ADR 0003, amendment of 2026-11-15).

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::*;
use title_select_api::settings::{DEFAULT_SELECTIONS_KEPT, Retention, SettingsUpdate};
use title_select_api::{DatRevisionRecord, DeletionBlocker, DeletionBlockerKind as K};
use title_select_engine::AuditEvidence;
use title_select_store::config::Config;
use title_select_store::{Error, ImportOutcome, Store};

const SET: &str = "No-Intro/Test - System";

struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("roots/sets");
        std::fs::create_dir_all(root.join(SET)).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            root.display().to_string()
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store
            .update_settings(
                &config,
                &SettingsUpdate {
                    retention: Some(Retention {
                        audits_keep: 1,
                        selections_keep: DEFAULT_SELECTIONS_KEPT,
                    }),
                    ..SettingsUpdate::default()
                },
                "Operator",
            )
            .unwrap();
        Env { tmp, store, config }
    }

    /// Revision 1 lists Pong; each later one adds a game.
    fn dat(&self, revision: u32) -> PathBuf {
        let mut games = vec![("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])];
        if revision >= 2 {
            games.push(("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]));
        }
        if revision >= 3 {
            games.push(("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]));
        }
        write_dat_version(
            &self.tmp.path().join("in"),
            &format!("Test - System ({revision}).dat"),
            &format!("2026010{revision}-000000"),
            &games,
        )
    }

    fn import(&self, revision: u32) -> String {
        self.store
            .import_dat(&self.dat(revision), None)
            .unwrap()
            .0
            .sha256
    }

    fn add_set(&self) {
        self.store
            .add_source_set(&self.config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
    }

    fn unregister_set(&self) {
        self.store
            .unregister_source_set(&self.config, "Test - System")
            .unwrap();
    }

    fn audit(&self, revision: &str) {
        self.store
            .audit_source_set(
                &self.config,
                "Test - System",
                Some(revision),
                AuditEvidence::Incremental,
            )
            .unwrap();
    }

    fn listed(&self) -> Vec<DatRevisionRecord> {
        self.store.dat_revisions().unwrap()
    }

    fn record(&self, sha256: &str) -> Option<DatRevisionRecord> {
        self.listed().into_iter().find(|r| r.sha256 == sha256)
    }

    fn blob(&self, sha256: &str) -> bool {
        self.store.blob_path(sha256).is_file()
    }

    /// The store's database, with foreign keys off, for a fixture row the public interface can't make on its own.
    fn db(&self) -> rusqlite::Connection {
        let db = rusqlite::Connection::open(self.store.dir().join("title-select.sqlite")).unwrap();
        db.pragma_update(None, "foreign_keys", "OFF").unwrap();
        db
    }

    fn refused(&self, sha256: &str) -> Vec<DeletionBlocker> {
        match self.store.delete_dat_revision(sha256) {
            Err(Error::RevisionInUse { message, blockers }) => {
                assert!(message.contains("Nothing was changed"), "{message}");
                blockers
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

fn kinds(blockers: &[DeletionBlocker]) -> Vec<K> {
    blockers.iter().map(|b| b.kind).collect()
}

#[test]
fn a_registered_source_set_blocks_every_revision_of_its_dat_even_one_no_audit_used() {
    let env = Env::new();
    let old = env.import(1);
    let new = env.import(2);
    env.add_set();
    let before = env.listed();

    for sha in [&old, &new] {
        let blockers = env.refused(sha);
        assert_eq!(
            blockers,
            vec![DeletionBlocker {
                kind: K::SourceSet,
                count: 1,
                names: vec!["Test - System".into()],
            }]
        );
        assert_eq!(env.record(sha).unwrap().blockers.unwrap(), blockers);
    }

    assert_eq!(env.listed(), before);
    assert!(env.blob(&new));
}

#[test]
fn an_unreferenced_older_revision_is_deleted_and_the_newest_keeps_its_place() {
    let env = Env::new();
    let old = env.import(1);
    let new = env.import(2);
    assert_eq!(env.record(&old).unwrap().blockers, Some(vec![]));

    let deleted = env.store.delete_dat_revision(&old).unwrap();

    assert_eq!(deleted.dat, "No-Intro/Test - System");
    assert!(!deleted.dat_removed);
    assert!(env.record(&old).is_none());
    assert!(!env.blob(&old));
    assert_eq!(env.listed().len(), 1);
    assert!(env.blob(&new));
}

#[test]
fn the_newest_revision_is_deleted_with_its_blob_and_the_older_one_becomes_the_newest() {
    let env = Env::new();
    let old = env.import(1);
    let new = env.import(2);
    assert!(env.blob(&new));

    let deleted = env.store.delete_dat_revision(&new).unwrap();

    assert!(deleted.blob_deleted);
    assert!(!env.blob(&new));
    assert_eq!(env.listed().len(), 1);
    // The older revision now holds the newest's place, so its bytes are kept when it is imported again.
    assert_eq!(
        env.store.import_dat(&env.dat(1), None).unwrap().1,
        ImportOutcome::Restored
    );
    env.store.retain_blobs().unwrap();
    assert!(env.blob(&old));
}

#[test]
fn a_restored_revision_is_deleted_with_its_blob() {
    let env = Env::new();
    let old = env.import(1);
    env.import(2);
    assert!(!env.blob(&old));
    assert_eq!(
        env.store.import_dat(&env.dat(1), None).unwrap().1,
        ImportOutcome::Restored
    );
    assert!(env.blob(&old));

    let deleted = env.store.delete_dat_revision(&old).unwrap();

    assert!(deleted.blob_deleted);
    assert!(!env.blob(&old));
    assert!(env.record(&old).is_none());
}

#[test]
fn deleting_the_last_revision_removes_the_dat_and_importing_it_again_adds_it_afresh() {
    let env = Env::new();
    let only = env.import(1);

    let deleted = env.store.delete_dat_revision(&only).unwrap();

    assert!(deleted.dat_removed);
    assert!(deleted.blob_deleted);
    assert!(env.listed().is_empty());
    assert!(!env.blob(&only));
    assert!(matches!(
        env.store.delete_dat_revision(&only),
        Err(Error::NotFound(_))
    ));
    // With no revision catalogued, no source set can be registered for the DAT.
    assert!(
        env.store
            .add_source_set(&env.config, "sets", SET, "No-Intro/Test - System")
            .is_err()
    );

    let (again, outcome) = env.store.import_dat(&env.dat(1), None).unwrap();

    assert_eq!(outcome, ImportOutcome::Added);
    assert_eq!(again.sha256, only);
    assert!(again.blob_present);
    assert!(env.blob(&only));
    assert_eq!(env.listed().len(), 1);
}

#[test]
fn a_revision_history_names_is_hidden_and_keeps_its_identity_in_that_history() {
    let env = Env::new();
    let old = env.import(1);
    env.add_set();
    env.audit(&old);
    let new = env.import(2);
    env.audit(&new);
    env.unregister_set();
    // The latest audit stays unpruned, so it keeps the newer revision; the older one's audit is pruned history.
    assert_eq!(
        kinds(&env.record(&new).unwrap().blockers.unwrap()),
        vec![K::Audit]
    );
    assert_eq!(env.record(&old).unwrap().blockers, Some(vec![]));

    env.store.delete_dat_revision(&old).unwrap();

    assert!(env.record(&old).is_none());
    assert!(!env.blob(&old));
    let set: i64 = env
        .db()
        .query_row("SELECT id FROM source_sets", [], |r| r.get(0))
        .unwrap();
    let history = env.store.source_set_history_by_id(set).unwrap();
    assert!(
        history
            .iter()
            .any(|a| a.dat_revision_sha256 == old && a.dat_revision_version == "20260101-000000"),
        "{history:?}"
    );

    // Its bytes come back as a fresh import, in the same row.
    let (again, outcome) = env.store.import_dat(&env.dat(1), None).unwrap();
    assert_eq!(outcome, ImportOutcome::Added);
    assert_eq!(again.sha256, old);
    assert_eq!(env.listed().len(), 2);
}

#[test]
fn a_deleted_newer_revision_no_longer_holds_the_newest_place() {
    let env = Env::new();
    let first = env.import(1);
    env.import(2);
    let third = env.import(3);
    env.add_set();
    env.audit(&third);
    env.store.import_dat(&env.dat(1), None).unwrap();
    env.audit(&first);
    env.unregister_set();
    // The pruned audit of the third keeps its row hidden once deleted.
    env.store.delete_dat_revision(&third).unwrap();

    let names: Vec<String> = env.listed().into_iter().map(|r| r.version).collect();
    assert_eq!(names, vec!["20260101-000000", "20260102-000000"]);
    // The second, now the newest, is what a new source set is audited against, and what retention keeps.
    assert_eq!(
        env.store.import_dat(&env.dat(2), None).unwrap().1,
        ImportOutcome::Restored
    );
    env.add_set();
    env.store
        .audit_source_set(
            &env.config,
            "Test - System",
            None,
            AuditEvidence::Incremental,
        )
        .unwrap();
    let history = env.store.history("Test - System").unwrap();
    assert_eq!(history[0].dat_revision_version, "20260102-000000");
}

/// A DAT with one revision and an Unregistered Source Set, so nothing but `fixture` blocks it, run with `:rev`,
/// `:dat`, `:set` and `:sha` bound.
fn blocked_by(kind: K, fixture: &str) {
    let env = Env::new();
    let sha = env.import(1);
    env.add_set();
    env.unregister_set();
    assert_eq!(env.record(&sha).unwrap().blockers, Some(vec![]));
    let db = env.db();
    let (rev, dat, set): (i64, i64, i64) = db
        .query_row(
            "SELECT r.id, r.dat_id, s.id FROM dat_revisions r JOIN source_sets s ON s.dat_id = r.dat_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let sql = fixture
        .replace(":rev", &rev.to_string())
        .replace(":dat", &dat.to_string())
        .replace(":set", &set.to_string())
        .replace(":sha", &format!("'{sha}'"));
    db.execute_batch(&sql).unwrap();
    drop(db);
    let before = env.listed();

    let blockers = env.refused(&sha);

    assert_eq!(kinds(&blockers), vec![kind], "{blockers:?}");
    assert!(blockers[0].count >= 1);
    assert_eq!(env.listed(), before);
    assert_eq!(env.record(&sha).unwrap().blockers.unwrap(), blockers);
    assert!(env.blob(&sha));
}

const REGISTERED_FOLDER: &str =
    "INSERT INTO library_folders (id, root, path, source_set_id, profile_id, generation,
                                                              created_at)
                                 VALUES (50, 'library', 'Test', :set, 1, 3, 'now');";

#[test]
fn an_unpruned_audit_alone_blocks() {
    let env = Env::new();
    let sha = env.import(1);
    env.add_set();
    env.audit(&sha);
    env.unregister_set();

    let blockers = env.refused(&sha);

    assert_eq!(
        blockers,
        vec![DeletionBlocker {
            kind: K::Audit,
            count: 1,
            names: vec!["Test - System".into()],
        }]
    );
    assert!(env.blob(&sha));
}

#[test]
fn a_registered_additions_set_alone_blocks() {
    blocked_by(
        K::AdditionsSet,
        "INSERT INTO additions_sets (dat_id, root, path, created_at) VALUES (:dat, 'adds', 'Test', 'now');",
    );
}

#[test]
fn an_unpruned_selection_alone_blocks() {
    blocked_by(
        K::Selection,
        "INSERT INTO selections (source_set_id, dat_revision_id, profile_version_id, system, system_settings, pins,
                                 summary, coverage_drift, stale_choices, created_at)
         VALUES (:set, :rev, 1, 'x', 0, '{}', '{}', '{}', '[]', 'now');",
    );
}

#[test]
fn an_unfinished_repair_plan_alone_blocks() {
    blocked_by(
        K::ChangePlan,
        "INSERT INTO audits (id, source_set_id, dat_revision_id, status, source_set_path, started_at, pruned_at)
         VALUES (70, :set, :rev, 'finished', 'x', 'now', 'now');
         INSERT INTO repair_plans (source_set_id, audit_id, content, digest, created_at, readers, hashers)
         VALUES (:set, 70, '{}', 'd', 'now', 1, 1);",
    );
}

#[test]
fn an_unbuilt_build_plan_for_a_registered_folder_alone_blocks() {
    blocked_by(
        K::ChangePlan,
        &format!(
            "{REGISTERED_FOLDER}
             INSERT INTO selections (id, source_set_id, dat_revision_id, profile_version_id, system,
                                     system_settings, pins, summary, coverage_drift, stale_choices, created_at,
                                     pruned_at)
             VALUES (60, :set, :rev, 1, 'x', 0, '{{}}', '{{}}', '{{}}', '[]', 'now', 'now');
             INSERT INTO plans (folder_id, selection_id, generation, content, digest, changes, created_at)
             VALUES (50, 60, 3, '{{}}', 'd', '{{}}', 'now');"
        ),
    );
}

#[test]
fn a_build_not_cleaned_up_alone_blocks() {
    blocked_by(
        K::Build,
        "INSERT INTO builds (folder_id, plan_id, job_id, selection_id, audit_id, dat_revision_id, state, generation,
                             started_at)
         VALUES (50, 1, 1, 1, 1, :rev, 'failed', 1, 'now');",
    );
}

#[test]
fn a_built_file_in_a_registered_folder_alone_blocks() {
    blocked_by(
        K::BuiltFile,
        &format!(
            "{REGISTERED_FOLDER}
             INSERT INTO built_files (folder_id, target, game, kind, build_id, selection_id, audit_id,
                                      dat_revision_id, roms, sources, size, mtime_ns)
             VALUES (50, 'Pong.zip', 'Pong', 'copy', 1, 1, 1, :rev, '[]', '[]', 1, 1);"
        ),
    );
}

#[test]
fn a_kept_file_still_kept_alone_blocks() {
    blocked_by(
        K::KeptFile,
        &format!(
            "{REGISTERED_FOLDER}
             INSERT INTO kept_files (folder_id, build_id, seq, generation, name, kept_name, reason,
                                     dat_revision_id, sha256, size, mtime_ns, dev, ino, state, created_at)
             VALUES (50, 1, 1, 1, 'Pong.zip', 'Pong.zip', 'removal', :rev, 'x', 1, 1, 1, 1, 'kept', 'now');"
        ),
    );
}

#[test]
fn a_queued_job_alone_blocks() {
    blocked_by(
        K::Job,
        "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at)
         VALUES ('queued', :set, :rev, 'Operator', 1, 1, 'now');",
    );
}

#[test]
fn an_unfulfilled_ingest_reaudit_alone_blocks() {
    blocked_by(
        K::IngestReaudit,
        "INSERT INTO ingest_reaudits (plan_id, item_id, source_set_id, sha256, actor, readers, hashers, created_at)
         VALUES (1, 1, :set, :sha, 'Operator', 1, 1, 'now');",
    );
}

#[test]
fn work_queued_after_the_listing_said_deletable_is_seen_by_the_delete() {
    let env = Env::new();
    let sha = env.import(1);
    env.add_set();
    assert_eq!(
        kinds(&env.record(&sha).unwrap().blockers.unwrap()),
        vec![K::SourceSet]
    );
    env.unregister_set();
    assert_eq!(env.record(&sha).unwrap().blockers, Some(vec![]));
    // An audit job is queued between the listing and the delete.
    env.db()
        .execute(
            "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at)
             SELECT 'queued', s.id, r.id, 'Operator', 1, 1, 'now' FROM source_sets s, dat_revisions r",
            [],
        )
        .unwrap();

    assert_eq!(kinds(&env.refused(&sha)), vec![K::Job]);
    assert!(env.blob(&sha));
}

#[test]
fn finished_or_pruned_history_does_not_block() {
    blocked_free(
        "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at)
         VALUES ('succeeded', :set, :rev, 'Operator', 1, 1, 'now');
         INSERT INTO selections (source_set_id, dat_revision_id, profile_version_id, system, system_settings, pins,
                                 summary, coverage_drift, stale_choices, created_at, pruned_at)
         VALUES (:set, :rev, 1, 'x', 0, '{}', '{}', '{}', '[]', 'now', 'now');
         INSERT INTO ingest_reaudits (plan_id, item_id, source_set_id, sha256, actor, readers, hashers, created_at,
                                      fulfilled_at)
         VALUES (1, 1, :set, :sha, 'Operator', 1, 1, 'now', 'now');
         INSERT INTO kept_files (folder_id, build_id, seq, generation, name, kept_name, reason, dat_revision_id,
                                 sha256, size, mtime_ns, dev, ino, state, created_at)
         VALUES (50, 1, 1, 1, 'Pong.zip', 'Pong.zip', 'removal', :rev, 'x', 1, 1, 1, 1, 'deleted', 'now');",
    );
}

/// As [`blocked_by`], for history that blocks nothing: the revision is deleted, its row kept hidden.
fn blocked_free(fixture: &str) {
    let env = Env::new();
    let sha = env.import(1);
    env.add_set();
    env.unregister_set();
    let db = env.db();
    let (rev, set): (i64, i64) = db
        .query_row(
            "SELECT r.id, s.id FROM dat_revisions r JOIN source_sets s ON s.dat_id = r.dat_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    db.execute_batch(
        &fixture
            .replace(":rev", &rev.to_string())
            .replace(":set", &set.to_string())
            .replace(":sha", &format!("'{sha}'")),
    )
    .unwrap();
    drop(db);
    assert_eq!(env.record(&sha).unwrap().blockers, Some(vec![]));

    let deleted = env.store.delete_dat_revision(&sha).unwrap();

    assert!(deleted.dat_removed);
    assert!(env.listed().is_empty());
    assert!(!env.blob(&sha));
    let hidden: i64 = env
        .db()
        .query_row(
            "SELECT count(*) FROM dat_revisions WHERE sha256 = ?1 AND deleted_at IS NOT NULL",
            [&sha],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hidden, 1);
}
