//! Placements (ADR 0023): a Source Set's Games placed in the Library Folder that holds them, and the Audits, checks,
//! Repairs and unregistering that follow them there. Each test records its Placements directly, from synthetic DATs,
//! Source Sets and files; `single_copy` places them by Builds.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::common::*;
use title_select_api::build::{AddLibraryFolder, FileOutcome, FolderCheck, LibraryFolder};
use title_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan, RepairStep};
use title_select_api::selection::{
    CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, Winner,
};
use title_select_api::storage::placed_path;
use title_select_api::{AuditEvidence, Finding, GameStatus, JobKind, JobState, Report};
use title_select_engine::torrentzip::Writer;
use title_select_store::config::Config;
use title_select_store::{Error, Fault, Store, Workers};

const SET: &str = "Alpha";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]),
        (
            "Joust (USA)",
            vec![
                rom("Joust (USA) (Side A).a26", b"joust-a"),
                rom("Joust (USA) (Side B).a26", b"joust-b"),
            ],
        ),
    ]
}

/// A store with the Source Set Alpha in the Root `sets`, which lists it among its `writable_paths`, and the Library
/// Folder `atari` bound to it in the writable Root `play`: the only one bound to it, so the one holding its Placed
/// Games.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    folder: LibraryFolder,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/Alpha", "roots/play", "in", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"Alpha\"]\n\n[roots.play]\npath = {:?}\nwritable = true\n\n\
             [roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
            tmp.path().join("roots/sets").display().to_string(),
            tmp.path().join("roots/play").display().to_string(),
            tmp.path().join("in").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store
            .update_settings(
                &config,
                &title_select_api::settings::SettingsUpdate {
                    ingest_folders: Some(
                        [(
                            "in".to_owned(),
                            title_select_api::settings::IngestCadence {
                                settle_secs: 0,
                                scan_every_minutes: None,
                            },
                        )]
                        .into(),
                    ),
                    ..Default::default()
                },
                "Operator",
            )
            .unwrap();
        store.register_ingest_folder(&config, "in").unwrap();
        let dat = write_dat_from(
            &tmp.path().join("dats"),
            "alpha.dat",
            "1",
            "No-Intro",
            &games(),
        );
        store.import_dat(&dat, None).unwrap();
        store
            .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
        store
            .save_profile(
                PROFILE,
                &SelectionProfile::default(),
                None,
                "Operator",
                &[],
                WORKERS,
            )
            .unwrap();
        let folder = store
            .add_library_folder(
                &config,
                &AddLibraryFolder {
                    root: "play".into(),
                    path: Some("atari".into()),
                    template: None,
                    source_set: SET.into(),
                    profile: PROFILE.into(),
                },
            )
            .unwrap();
        Env {
            tmp,
            store,
            config,
            folder,
        }
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("roots/sets/Alpha")
    }

    fn library(&self) -> PathBuf {
        self.tmp.path().join("roots/play/atari")
    }

    /// Writes `game` as the TorrentZip `<game>.zip` holding `members` in the Library Folder, and records it placed
    /// there.
    fn place(&self, game: &str, members: &[(&str, &[u8])]) {
        let name = format!("{game}.zip");
        torrentzip(&self.library().join(&name), members);
        self.store
            .record_placement(&self.config, SET, game, self.folder.id, &[&name])
            .unwrap();
    }

    fn audit(&self, evidence: AuditEvidence) -> Report {
        self.store
            .audit_source_set(&self.config, SET, None, evidence)
            .unwrap()
            .report
    }

    fn check(&self) -> FolderCheck {
        let job = self
            .store
            .enqueue_check(self.folder.id, "Operator")
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        let done = self
            .store
            .run_check_job(&self.config, job.id, None)
            .unwrap();
        assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
        self.store.latest_check(self.folder.id).unwrap().unwrap()
    }

    /// Writes `name` in the Ingest Folder as `bytes` and scans it twice, so it settles and is assessed.
    fn ingest(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.tmp.path().join("in").join(name), bytes).unwrap();
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self
                .store
                .run_ingest_scan_job(&self.config, job.id)
                .unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
    }

    /// The names in `dir`, relative to it, of every file under it.
    fn files_under(dir: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut walk = vec![dir.to_path_buf()];
        while let Some(at) = walk.pop() {
            let Ok(entries) = std::fs::read_dir(&at) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                match path.is_dir() {
                    true => walk.push(path),
                    false => found.push(path.strip_prefix(dir).unwrap().display().to_string()),
                }
            }
        }
        found.sort();
        found
    }

    fn plan(&self, choices: &[(&str, RepairChoice)]) -> RepairPlan {
        let request = PlanRepair {
            choices: choices
                .iter()
                .map(|(key, choice)| (key.to_string(), *choice))
                .collect(),
            additions: Default::default(),
        };
        self.store.plan_repair(&self.config, SET, &request).unwrap()
    }

    /// Approves `plan` and runs its Repair Job, then the incremental Audit it owes.
    fn repair(&self, plan: &RepairPlan) -> RepairPlan {
        let approved = self
            .store
            .approve_repair(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let job = approved.job.expect("an approved repair has a job");
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job);
        let ended = self.store.run_repair_job(&self.config, job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.store.fulfil_repair_audits().unwrap();
        let done = self.store.repair_plan(plan.id).unwrap();
        let audit = done.audit_job.expect("a repair queues an audit");
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, audit);
        let audited = self.store.run_audit_job(&self.config, audit, None).unwrap();
        assert_eq!(audited.state, JobState::Succeeded, "{audited:?}");
        done
    }

    /// The Game Status of each Game in `report`.
    fn statuses(report: &Report) -> BTreeMap<String, GameStatus> {
        report
            .games
            .iter()
            .map(|g| (g.name.clone(), g.status))
            .collect()
    }
}

fn torrentzip(path: &std::path::Path, members: &[(&str, &[u8])]) {
    let mut writer = Writer::new(std::fs::File::create(path).unwrap()).unwrap();
    for (name, payload) in members {
        writer
            .add(name, payload.len() as u64, &mut &payload[..])
            .unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn an_audit_verifies_a_placed_game_where_it_is_placed_and_reports_it_missing_once_its_file_is_gone()
{
    let env = Env::new();
    torrentzip(
        &env.set().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    // A file of the Library Folder that is no Placed Game's is never read as the Source Set's.
    torrentzip(
        &env.library().join("Yars (USA).zip"),
        &[("Yars (USA).a26", b"yars")],
    );

    let report = env.audit(AuditEvidence::Full);

    let placed = placed_path(env.folder.id, "Pong (USA).zip");
    let pong: Vec<_> = report.entries.iter().filter(|e| e.path == placed).collect();
    assert_eq!(pong.len(), 1, "{:#?}", report.entries);
    assert_eq!(pong[0].finding, Finding::Matched);
    assert_eq!(
        pong[0].evidence.as_ref().map(|e| e.level),
        Some(title_select_api::EvidenceLevel::Payload)
    );
    assert_eq!(
        Env::statuses(&report),
        BTreeMap::from([
            ("Pong (USA)".to_owned(), GameStatus::Complete),
            ("Combat (USA)".to_owned(), GameStatus::Complete),
            ("Yars (USA)".to_owned(), GameStatus::Missing),
            ("Joust (USA)".to_owned(), GameStatus::Missing),
        ]),
        "a placed game is never missing for being placed, and only placed games' files are read"
    );

    std::fs::remove_file(env.library().join("Pong (USA).zip")).unwrap();
    let report = env.audit(AuditEvidence::Incremental);

    assert!(
        report.entries.iter().all(|e| e.path != placed),
        "{:#?}",
        report.entries
    );
    assert_eq!(Env::statuses(&report)["Pong (USA)"], GameStatus::Missing);
}

#[test]
fn an_audit_never_follows_a_symbolic_link_put_in_place_of_a_placed_file_or_its_folder() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    std::fs::create_dir(env.library().join("c")).unwrap();
    torrentzip(
        &env.library().join("c/Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    env.store
        .record_placement(
            &env.config,
            SET,
            "Combat (USA)",
            env.folder.id,
            &["c/Combat (USA).zip"],
        )
        .unwrap();
    let report = env.audit(AuditEvidence::Full);
    assert_eq!(Env::statuses(&report)["Pong (USA)"], GameStatus::Complete);
    assert_eq!(Env::statuses(&report)["Combat (USA)"], GameStatus::Complete);
    // Exact copies elsewhere in the same Root, linked to from where each was placed.
    let elsewhere = env.tmp.path().join("roots/play/elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::rename(
        env.library().join("Pong (USA).zip"),
        elsewhere.join("Pong (USA).zip"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        elsewhere.join("Pong (USA).zip"),
        env.library().join("Pong (USA).zip"),
    )
    .unwrap();
    std::fs::rename(env.library().join("c"), elsewhere.join("c")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("c"), env.library().join("c")).unwrap();

    for evidence in [AuditEvidence::Incremental, AuditEvidence::Full] {
        let report = env.audit(evidence);

        let pong = placed_path(env.folder.id, "Pong (USA).zip");
        let combat = placed_path(env.folder.id, "c/Combat (USA).zip");
        assert!(
            report
                .entries
                .iter()
                .any(|e| e.path == pong && e.finding == Finding::Unreadable),
            "{:#?}",
            report.entries
        );
        assert!(
            report
                .entries
                .iter()
                .all(|e| e.path != combat || e.finding == Finding::Unreadable),
            "{:#?}",
            report.entries
        );
        let statuses = Env::statuses(&report);
        assert_ne!(statuses["Pong (USA)"], GameStatus::Complete);
        assert_ne!(statuses["Combat (USA)"], GameStatus::Complete);
    }
}

#[test]
fn a_changed_placed_game_gets_its_finding_and_an_incremental_audit_reuses_reads_of_unchanged_ones()
{
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    env.place("Combat (USA)", &[("Combat (USA).a26", b"combat")]);
    env.audit(AuditEvidence::Incremental);
    // Combat's file is rewritten with other bytes under its ROM's name: not the ROM the DAT gives.
    std::thread::sleep(std::time::Duration::from_millis(20));
    torrentzip(
        &env.library().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"c0mbat")],
    );

    let report = env.audit(AuditEvidence::Incremental);

    let entry = |game: &str| {
        let path = placed_path(env.folder.id, &format!("{game}.zip"));
        report
            .entries
            .iter()
            .find(|e| e.path == path)
            .unwrap()
            .clone()
    };
    let pong = entry("Pong (USA)");
    assert_eq!(pong.finding, Finding::Matched);
    assert!(
        pong.evidence.unwrap().reused.is_some(),
        "an unchanged placed game's earlier read is reused"
    );
    let combat = entry("Combat (USA)");
    assert_eq!(combat.finding, Finding::Mismatched);
    assert!(
        combat.evidence.unwrap().reused.is_none(),
        "a changed one is read again"
    );
    let statuses = Env::statuses(&report);
    assert_eq!(statuses["Pong (USA)"], GameStatus::Complete);
    assert_eq!(statuses["Combat (USA)"], GameStatus::Missing);
}

#[test]
fn a_check_of_the_holding_folder_reports_a_changed_or_missing_placed_game_and_never_as_drift() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    env.place("Combat (USA)", &[("Combat (USA).a26", b"combat")]);
    let outcome = |check: &FolderCheck, target: &str| {
        let file = check.files.iter().find(|f| f.target == target).unwrap();
        assert!(file.placed, "{file:?}");
        file.outcome
    };

    let check = env.check();
    assert_eq!(outcome(&check, "Pong (USA).zip"), FileOutcome::Unchanged);
    assert!(check.drift.is_empty(), "{:?}", check.drift);

    std::thread::sleep(std::time::Duration::from_millis(20));
    torrentzip(
        &env.library().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"c0mbat")],
    );
    std::fs::remove_file(env.library().join("Pong (USA).zip")).unwrap();
    let check = env.check();

    assert_eq!(outcome(&check, "Combat (USA).zip"), FileOutcome::Conflict);
    assert_eq!(outcome(&check, "Pong (USA).zip"), FileOutcome::Missing);
    assert!(
        check.drift.is_empty(),
        "a placed game's file is never drift a cleanup could acknowledge or keep aside: {:?}",
        check.drift
    );
    // The Audit is the authority, whatever the check found.
    let statuses = Env::statuses(&env.audit(AuditEvidence::Incremental));
    assert_eq!(statuses["Combat (USA)"], GameStatus::Missing);
    assert_eq!(statuses["Pong (USA)"], GameStatus::Missing);
}

#[test]
fn a_check_reads_again_a_placed_file_put_in_place_with_its_size_and_modification_time_kept() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    let path = env.library().join("Pong (USA).zip");
    let outcome = |check: &FolderCheck| {
        let file = check
            .files
            .iter()
            .find(|f| f.target == "Pong (USA).zip")
            .unwrap();
        (file.outcome, file.hashed)
    };
    // Puts `bytes` in its place by a rename, as the file it replaces was: the same modification time.
    let replace = |bytes: &[u8]| {
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let staged = env.library().join("staged.tmp");
        std::fs::write(&staged, bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&staged)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        std::fs::rename(&staged, &path).unwrap();
    };
    assert_eq!(outcome(&env.check()), (FileOutcome::Unchanged, false));

    // An exact copy: read again, found the same, and trusted as it now is.
    replace(&std::fs::read(&path).unwrap());
    assert_eq!(outcome(&env.check()), (FileOutcome::Refreshed, true));
    assert_eq!(outcome(&env.check()), (FileOutcome::Unchanged, false));

    // As long, and holding other bytes.
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    replace(&bytes);
    let check = env.check();

    assert_eq!(outcome(&check), (FileOutcome::Conflict, true));
    assert!(check.drift.is_empty(), "{:?}", check.drift);
}

#[test]
fn unregistering_the_folder_or_the_source_set_is_refused_while_games_are_placed() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    env.place("Combat (USA)", &[("Combat (USA).a26", b"combat")]);

    let folder = env
        .store
        .unregister_library_folder(&env.config, env.folder.id)
        .unwrap_err();
    let set = env
        .store
        .unregister_source_set(&env.config, SET)
        .unwrap_err();

    match folder {
        Error::Conflict(why) => assert!(
            why.contains("it holds 2 games of source set Alpha"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    match set {
        Error::Conflict(why) => assert!(
            why.contains("2 games placed in a play library folder"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(env.store.library_folder(&env.config, env.folder.id).is_ok());
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 2);
}

#[test]
fn only_a_folder_bound_to_the_source_set_holds_its_placed_games() {
    let env = Env::new();
    assert_eq!(env.store.holding_folder(SET).unwrap(), Some(env.folder.id));
    let second = env
        .store
        .add_library_folder(
            &env.config,
            &AddLibraryFolder {
                root: "play".into(),
                path: Some("atari-2".into()),
                template: None,
                source_set: SET.into(),
                profile: PROFILE.into(),
            },
        )
        .unwrap();
    assert_eq!(
        env.store.holding_folder(SET).unwrap(),
        None,
        "with two bound and none named, none holds them"
    );
    torrentzip(
        &env.library().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    assert!(matches!(
        env.store.record_placement(
            &env.config,
            SET,
            "Pong (USA)",
            env.folder.id,
            &["Pong (USA).zip"]
        ),
        Err(Error::Refused(_))
    ));

    env.store
        .set_holding_folder(SET, Some(env.folder.id))
        .unwrap();
    env.place("Combat (USA)", &[("Combat (USA).a26", b"combat")]);

    assert!(matches!(
        env.store.set_holding_folder(SET, Some(second.id)),
        Err(Error::Conflict(_))
    ));
    assert_eq!(env.store.holding_folder(SET).unwrap(), Some(env.folder.id));
}

/// Records `title` alone as a Selection Job's result, made with the latest Profile version.
fn select(env: &Env, title: &str, sha256: &str) -> i64 {
    env.store
        .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
        .unwrap();
    let job = env.store.claim_job().unwrap().unwrap();
    assert_eq!(job.kind, JobKind::Selection);
    let rule = Rule {
        stage: "Parent selection".into(),
        reference: Some("REF0001".into()),
        message: "m".into(),
    };
    let selection = Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SET.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups: vec![SelectionGroup {
            group: title.to_lowercase(),
            unresolved: false,
            winners: vec![Winner {
                title: title.into(),
                rule,
                unresolved: false,
            }],
            alternatives: vec![],
            exclusions: vec![],
            choice: None,
            drift: false,
        }],
        stale_choices: BTreeMap::new(),
    };
    env.store
        .record_selection(job.id, &selection)
        .unwrap()
        .selection
        .unwrap()
}

#[test]
fn an_audit_and_a_build_of_the_holding_folder_exclude_each_other_before_anything_is_placed() {
    let env = Env::new();
    torrentzip(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    let sha256 = env.audit(AuditEvidence::Full).dat_revision.sha256;
    let selection = select(&env, "Pong (USA)", &sha256);
    let plan = env
        .store
        .plan_build(&env.config, env.folder.id, selection)
        .unwrap();

    let audit = env
        .store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental)
        .unwrap();
    assert!(matches!(
        env.store
            .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS),
        Err(Error::Conflict(_))
    ));
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, audit.id);
    let audited = env
        .store
        .run_audit_job(&env.config, audit.id, None)
        .unwrap();
    assert_eq!(audited.state, JobState::Succeeded);

    let plan = env
        .store
        .plan_build(&env.config, env.folder.id, selection)
        .unwrap();
    env.store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    assert!(matches!(
        env.store
            .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .audit_source_set(&env.config, SET, None, AuditEvidence::Incremental),
        Err(Error::Conflict(_))
    ));

    // A second folder bound to the Source Set leaves it no holding folder by default: nothing an Audit reads waits.
    env.store
        .add_library_folder(
            &env.config,
            &AddLibraryFolder {
                root: "play".into(),
                path: Some("atari-copy".into()),
                template: None,
                source_set: SET.into(),
                profile: PROFILE.into(),
            },
        )
        .unwrap();
    env.store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental)
        .unwrap();
}

#[test]
fn a_cleanup_of_the_holding_folder_is_refused_and_never_keeps_a_placed_game_aside() {
    let env = Env::new();
    torrentzip(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    let sha256 = env.audit(AuditEvidence::Full).dat_revision.sha256;
    let selection = select(&env, "Pong (USA)", &sha256);
    let plan = env
        .store
        .plan_build(&env.config, env.folder.id, selection)
        .unwrap();
    let build = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, build.id);
    let built = env
        .store
        .run_build_job(&env.config, build.id, None)
        .unwrap();
    assert_eq!(built.state, JobState::Succeeded, "{built:?}");
    // The built file, now Pong's only copy: placed there.
    std::fs::remove_file(env.set().join("Pong (USA).zip")).unwrap();
    env.store
        .record_placement(
            &env.config,
            SET,
            "Pong (USA)",
            env.folder.id,
            &["Pong (USA).zip"],
        )
        .unwrap();

    let refused = env.store.plan_library_removal(&env.config, env.folder.id);

    assert!(
        matches!(&refused, Err(Error::Refused(why)) if why.contains("only copy")),
        "{refused:?}"
    );
    assert!(env.library().join("Pong (USA).zip").is_file());
    assert!(!env.library().join(".title-select/retained").exists());
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
    assert_eq!(
        Env::statuses(&env.audit(AuditEvidence::Full))["Pong (USA)"],
        GameStatus::Complete
    );
}

#[test]
fn a_repair_renames_a_placed_game_where_it_is_placed() {
    let env = Env::new();
    // Pong, placed under a name that isn't its own; Combat, placed as it should be.
    torrentzip(
        &env.library().join("pong.zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    env.store
        .record_placement(&env.config, SET, "Pong (USA)", env.folder.id, &["pong.zip"])
        .unwrap();
    env.place("Combat (USA)", &[("Combat (USA).a26", b"combat")]);
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    let from = placed_path(env.folder.id, "pong.zip");
    let to = placed_path(env.folder.id, "Pong (USA).zip");
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "game:Pong (USA)")
        .unwrap();
    assert_eq!(
        (item.kind, item.chosen),
        (RepairKind::Rename, RepairChoice::Apply)
    );
    assert!(
        matches!(
            plan.content.steps.as_slice(),
            [step] if matches!(&step.step, RepairStep::Rename { from: f, to: t, .. } if *f == from && *t == to)
        ),
        "{:#?}",
        plan.content.steps
    );

    env.repair(&plan);

    assert!(env.library().join("Pong (USA).zip").is_file());
    assert!(!env.library().join("pong.zip").exists());
    assert_eq!(
        std::fs::read_dir(env.set()).unwrap().count(),
        1,
        "nothing but the source set's marker is in its folder"
    );
    let placed: Vec<(String, String)> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .map(|p| (p.game, p.path))
        .collect();
    assert_eq!(
        placed,
        [
            ("Combat (USA)".to_owned(), "Combat (USA).zip".to_owned()),
            ("Pong (USA)".to_owned(), "Pong (USA).zip".to_owned()),
        ],
        "the renamed file stays placed under its new name"
    );
    let latest = env.store.history(SET).unwrap()[0].id;
    let report = env.store.report(latest).unwrap();
    assert!(
        report
            .entries
            .iter()
            .any(|e| e.path == to && e.finding == Finding::Matched)
    );
    assert_eq!(Env::statuses(&report)["Pong (USA)"], GameStatus::Complete);
    assert!(
        env.plan(&[]).content.items.is_empty(),
        "nothing more to repair"
    );
}

#[test]
fn a_repair_sets_a_placed_file_aside_into_the_source_sets_kept_files() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    // Combat's placed file holds nothing any game needs.
    env.place("Combat (USA)", &[("junk.bin", b"junk")]);
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    let combat = placed_path(env.folder.id, "Combat (USA).zip");
    let key = format!("file:{combat}");
    let item = plan.content.items.iter().find(|i| i.key == key).unwrap();
    assert_eq!(
        (item.kind, item.chosen),
        (RepairKind::KeepAside, RepairChoice::KeepAside)
    );

    env.repair(&plan);

    assert!(!env.library().join("Combat (USA).zip").exists());
    let kept = env.store.source_kept_files(SET).unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        kept[0].name, combat,
        "a kept file of the source set, from where it was placed"
    );
    assert!(
        env.set()
            .join(".title-select/retained")
            .join(kept[0].generation.to_string())
            .join(&kept[0].kept_name)
            .is_file()
    );
    assert!(
        !env.library().join(".title-select/retained").exists(),
        "nothing is kept in the library folder's kept files"
    );
    let placed: Vec<String> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .map(|p| p.game)
        .collect();
    assert_eq!(placed, ["Pong (USA)"], "combat is no longer placed");
    let latest = env.store.history(SET).unwrap()[0].id;
    let report = env.store.report(latest).unwrap();
    assert!(report.entries.iter().all(|e| e.path != combat));
    assert_eq!(Env::statuses(&report)["Pong (USA)"], GameStatus::Complete);
}

#[test]
fn a_repair_repacks_a_placed_game_where_it_is_placed_by_a_rename_or_across_filesystems_by_a_copy() {
    for across in [false, true] {
        let env = Env::new();
        env.store.copy_across_filesystems(across);
        // Pong's placed ZIP holds its ROM under another name: a repack repairs it.
        env.place("Pong (USA)", &[("pong.a26", b"pong")]);
        let was = env.store.placed_files(SET).unwrap()[0].clone();
        env.audit(AuditEvidence::Full);

        let plan = env.plan(&[]);

        let target = placed_path(env.folder.id, "Pong (USA).zip");
        let item = plan
            .content
            .items
            .iter()
            .find(|i| i.key == "game:Pong (USA)")
            .unwrap();
        assert_eq!(item.chosen, RepairChoice::Apply, "{item:?}");
        assert!(
            plan.content
                .steps
                .iter()
                .any(|s| matches!(&s.step, RepairStep::Build { target: t, .. } if *t == target)),
            "{:#?}",
            plan.content.steps
        );

        env.repair(&plan);

        let placed = env.store.placed_files(SET).unwrap();
        assert_eq!(placed.len(), 1, "{placed:?}");
        assert_eq!(
            (placed[0].game.as_str(), placed[0].path.as_str()),
            ("Pong (USA)", "Pong (USA).zip")
        );
        assert_ne!(
            placed[0].sha256, was.sha256,
            "the repack is recorded placed"
        );
        let kept = env.store.source_kept_files(SET).unwrap();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(
            kept[0].name, target,
            "what it replaced is the source set's kept file"
        );
        assert_eq!(
            Env::files_under(&env.library())
                .into_iter()
                .filter(|f| f != ".title-select/marker")
                .collect::<Vec<_>>(),
            ["Pong (USA).zip"],
            "nothing is staged, kept or left behind in the library folder (across: {across})"
        );
        assert_eq!(
            Env::files_under(&env.set()),
            [
                ".title-select/marker".to_owned(),
                format!(".title-select/retained/1/{}", kept[0].kept_name),
            ],
            "nothing staged is left in the source set's folder (across: {across})"
        );
        // Copied across, the kept file is a new one, and both files copied were deleted through a .deleting area.
        let kept_ino = std::os::unix::fs::MetadataExt::ino(
            &std::fs::metadata(
                env.set()
                    .join(".title-select/retained/1")
                    .join(&kept[0].kept_name),
            )
            .unwrap(),
        );
        assert_eq!(kept_ino != was.ino, across);
        assert_eq!(
            env.set().join(".title-select/retained/.deleting").exists(),
            across
        );
        assert_eq!(
            env.library()
                .join(".title-select/retained/.deleting")
                .exists(),
            across
        );
        let latest = env.store.history(SET).unwrap()[0].id;
        let report = env.store.report(latest).unwrap();
        assert!(
            report
                .entries
                .iter()
                .any(|e| e.path == target && e.finding == Finding::Matched)
        );
        assert_eq!(Env::statuses(&report)["Pong (USA)"], GameStatus::Complete);
        assert!(
            env.plan(&[]).content.items.is_empty(),
            "nothing more to repair"
        );
    }
}

#[test]
fn a_repair_replaces_a_placed_games_file_from_its_own_files_and_never_from_another_games() {
    let env = Env::new();
    // Combat's placed file holds its ROM beside a stray file; Pong's holds Yars's ROM too, which Yars, in no
    // folder, never takes from it.
    torrentzip(
        &env.library().join("c.zip"),
        &[("Combat (USA).a26", b"combat"), ("readme.txt", b"stray")],
    );
    env.store
        .record_placement(&env.config, SET, "Combat (USA)", env.folder.id, &["c.zip"])
        .unwrap();
    env.place(
        "Pong (USA)",
        &[("Pong (USA).a26", b"pong"), ("Yars (USA).a26", b"yars")],
    );
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    let combat = placed_path(env.folder.id, "Combat (USA).zip");
    let pong = placed_path(env.folder.id, "Pong (USA).zip");
    assert!(
        plan.content.steps.iter().any(|s| matches!(
            &s.step,
            RepairStep::Build { target, sources, .. }
                if *target == combat && sources.keys().all(|p| *p == placed_path(env.folder.id, "c.zip"))
        )),
        "{:#?}",
        plan.content.steps
    );
    assert!(
        plan.content.steps.iter().all(|s| match &s.step {
            RepairStep::Build {
                target, sources, ..
            } => {
                !target.contains("Yars") && (*target == pong || !sources.contains_key(&pong))
            }
            RepairStep::Rename { to, .. } => !to.contains("Yars"),
            _ => true,
        }),
        "{:#?}",
        plan.content.steps
    );

    env.repair(&plan);

    let placed: Vec<(String, String)> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .map(|p| (p.game, p.path))
        .collect();
    assert!(
        placed.contains(&("Combat (USA)".to_owned(), "Combat (USA).zip".to_owned())),
        "{placed:?}"
    );
    assert!(
        placed.iter().all(|(game, _)| game != "Yars (USA)"),
        "{placed:?}"
    );
    let latest = env.store.history(SET).unwrap()[0].id;
    let statuses = Env::statuses(&env.store.report(latest).unwrap());
    assert_eq!(statuses["Combat (USA)"], GameStatus::Complete);
    assert_eq!(statuses["Pong (USA)"], GameStatus::Complete);
}

#[test]
fn a_fill_of_a_placed_game_whose_files_are_gone_writes_it_in_the_source_sets_folder() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    std::fs::remove_file(env.library().join("Pong (USA).zip")).unwrap();
    env.ingest("pong.a26", b"pong");
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    assert!(
        plan.content.steps.iter().any(
            |s| matches!(&s.step, RepairStep::Fill { target, .. } if target == "Pong (USA).zip")
        ),
        "{:#?}",
        plan.content.steps
    );

    env.repair(&plan);

    assert!(env.set().join("Pong (USA).zip").is_file());
    assert!(
        env.store.placed_files(SET).unwrap().is_empty(),
        "a placed game filled in its source set's folder is no longer placed"
    );
    let latest = env.store.history(SET).unwrap()[0].id;
    let statuses = Env::statuses(&env.store.report(latest).unwrap());
    assert_eq!(statuses["Pong (USA)"], GameStatus::Complete);
}

#[test]
fn a_repair_copies_a_placed_file_aside_into_the_source_sets_kept_files_across_filesystems() {
    let env = Env::new();
    env.store.copy_across_filesystems(true);
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    env.place("Combat (USA)", &[("junk.bin", b"junk")]);
    let bytes = std::fs::read(env.library().join("Combat (USA).zip")).unwrap();
    let ino = std::os::unix::fs::MetadataExt::ino(
        &std::fs::metadata(env.library().join("Combat (USA).zip")).unwrap(),
    );
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);
    env.repair(&plan);

    let combat = placed_path(env.folder.id, "Combat (USA).zip");
    assert!(!env.library().join("Combat (USA).zip").exists());
    let kept = env.store.source_kept_files(SET).unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].name, combat);
    let copy = env
        .set()
        .join(".title-select/retained")
        .join(kept[0].generation.to_string())
        .join(&kept[0].kept_name);
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        bytes,
        "the kept file is an exact copy"
    );
    assert_ne!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&copy).unwrap()),
        ino,
        "a copy, not the original renamed"
    );
    assert_eq!(
        Env::files_under(&env.library().join(".title-select")),
        ["marker"],
        "the original was deleted, and nothing is left in the library folder's private folders"
    );
    let placed: Vec<String> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .map(|p| p.game)
        .collect();
    assert_eq!(placed, ["Pong (USA)"]);
}

#[test]
fn a_placed_file_changed_after_it_was_copied_aside_stays_placed_and_its_copy_stays_kept() {
    let env = Env::new();
    env.store.copy_across_filesystems(true);
    env.place("Combat (USA)", &[("junk.bin", b"junk")]);
    env.audit(AuditEvidence::Full);
    let plan = env.plan(&[]);
    let seq = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Keep { .. }))
        .unwrap() as u32;
    // Once it is copied, it is written over in place, as long and with its modification time set back.
    let original = env.library().join("Combat (USA).zip");
    let spoiled = original.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
            if at == Fault::RepairMoved(seq) {
                let mtime = std::fs::metadata(&spoiled).unwrap().modified().unwrap();
                let mut bytes = std::fs::read(&spoiled).unwrap();
                let last = bytes.len() - 1;
                bytes[last] ^= 1;
                let mut file = std::fs::File::options().write(true).open(&spoiled).unwrap();
                std::io::Write::write_all(&mut file, &bytes).unwrap();
                file.set_modified(mtime).unwrap();
            }
        })));

    let done = env.repair(&plan);
    env.store.hook_faults(None);

    let combat = placed_path(env.folder.id, "Combat (USA).zip");
    assert!(
        original.is_file(),
        "the changed original stays where it was placed"
    );
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1, "{placed:?}");
    assert_eq!(placed[0].path, "Combat (USA).zip");
    let kept = env.store.source_kept_files(SET).unwrap();
    assert_eq!(kept.len(), 1, "its copy stays kept: {kept:?}");
    assert_eq!(kept[0].name, combat);
    let outcome = done
        .outcomes
        .iter()
        .find(|o| o.item == format!("file:{combat}"))
        .unwrap();
    assert!(
        outcome
            .error
            .as_deref()
            .is_some_and(|e| e.contains("it stays placed")),
        "{outcome:?}"
    );
    assert!(
        Env::files_under(&env.library().join(".title-select/retained/.deleting")).is_empty(),
        "it was put back, not left to be deleted"
    );
}

#[test]
fn a_placed_file_is_never_deleted_once_its_copy_aside_changed_even_across_a_stop() {
    let env = Env::new();
    env.store.copy_across_filesystems(true);
    env.place("Combat (USA)", &[("junk.bin", b"junk")]);
    env.audit(AuditEvidence::Full);
    let plan = env.plan(&[]);
    let (seq, kept) = plan
        .content
        .steps
        .iter()
        .enumerate()
        .find_map(|(n, s)| match &s.step {
            RepairStep::Keep { kept_name, .. } => Some((n as u32, kept_name.clone())),
            _ => None,
        })
        .unwrap();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    // Stopped once its copy is published in the kept files, before the original is deleted.
    env.store.inject_fault(Some(Fault::RepairMoved(seq)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_repair_job(&env.config, job, None)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);
    // Its copy, written over in place: the same file, holding something else.
    let copy = env
        .set()
        .join(".title-select/retained")
        .join(plan.content.generation.to_string())
        .join(&kept);
    std::fs::write(&copy, b"spoiled").unwrap();

    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, requeued.id);
    env.store
        .run_repair_job(&env.config, requeued.id, None)
        .unwrap();

    let combat = placed_path(env.folder.id, "Combat (USA).zip");
    assert!(
        env.library().join("Combat (USA).zip").is_file(),
        "the original stays where it was placed"
    );
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1, "it stays placed: {placed:?}");
    let done = env.store.repair_plan(plan.id).unwrap();
    let outcome = done
        .outcomes
        .iter()
        .find(|o| o.item == format!("file:{combat}"))
        .unwrap();
    assert!(
        !outcome.done
            && outcome
                .error
                .as_deref()
                .is_some_and(|e| e.contains("its copy changed since it was published")),
        "{outcome:?}"
    );
    assert!(
        Env::files_under(&env.library().join(".title-select/retained/.deleting")).is_empty(),
        "it was put back, not left to be deleted"
    );
}

#[test]
fn a_placed_file_is_never_deleted_once_its_copy_aside_changes_after_it_was_first_checked() {
    // Its copy written over in place, or replaced by another file, once the original is set aside to delete and read
    // back: in the run that copied it, and in one resumed after a stop there, with the original's slot recorded.
    for (replace, resumed) in [(false, false), (true, false), (false, true), (true, true)] {
        let case = format!("replace={replace} resumed={resumed}");
        let env = Env::new();
        env.store.copy_across_filesystems(true);
        env.place("Combat (USA)", &[("junk.bin", b"junk")]);
        env.audit(AuditEvidence::Full);
        let plan = env.plan(&[]);
        let (seq, kept) = plan
            .content
            .steps
            .iter()
            .enumerate()
            .find_map(|(n, s)| match &s.step {
                RepairStep::Keep { kept_name, .. } => Some((n as u32, kept_name.clone())),
                _ => None,
            })
            .unwrap();
        let copy = env
            .set()
            .join(".title-select/retained")
            .join(plan.content.generation.to_string())
            .join(&kept);
        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let mut job = approved.job.unwrap();
        assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
        if resumed {
            env.store
                .inject_fault(Some(Fault::RepairCarryChecking(seq)));
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                env.store.run_repair_job(&env.config, job, None)
            }));
            assert!(ran.is_err(), "{case}: {ran:?}");
            env.store.inject_fault(None);
            assert!(
                !Env::files_under(&env.library().join(".title-select/retained/.deleting"))
                    .is_empty(),
                "{case}: stopped with the original set aside to delete"
            );
            job = env.store.recover_jobs(WORKERS).unwrap()[0]
                .requeued
                .clone()
                .unwrap()
                .id;
            assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
        }
        let spoiled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (at, did) = (copy.clone(), spoiled.clone());
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if fault == Fault::RepairCarryChecking(seq) {
                    if replace {
                        std::fs::remove_file(&at).unwrap();
                    }
                    std::fs::write(&at, b"spoiled").unwrap();
                    did.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            })));
        env.store.run_repair_job(&env.config, job, None).unwrap();
        env.store.hook_faults(None);

        assert!(spoiled.load(std::sync::atomic::Ordering::SeqCst), "{case}");
        assert!(
            env.library().join("Combat (USA).zip").is_file(),
            "{case}: the original stays where it was placed"
        );
        assert_eq!(env.store.placed_files(SET).unwrap().len(), 1, "{case}");
        let combat = placed_path(env.folder.id, "Combat (USA).zip");
        let done = env.store.repair_plan(plan.id).unwrap();
        let outcome = done
            .outcomes
            .iter()
            .find(|o| o.item == format!("file:{combat}"))
            .unwrap();
        assert!(
            !outcome.done
                && outcome
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("was not deleted, since")),
            "{case}: {outcome:?}"
        );
        assert!(
            Env::files_under(&env.library().join(".title-select/retained/.deleting")).is_empty(),
            "{case}: it was put back, not left to be deleted"
        );
    }
}

#[test]
fn a_repacked_placed_file_whose_copy_in_place_changed_is_never_recorded_placed() {
    let env = Env::new();
    env.store.copy_across_filesystems(true);
    // Placed under another name than its own, so its repack is published before it is kept aside.
    torrentzip(&env.library().join("pong.zip"), &[("pong.a26", b"pong")]);
    env.store
        .record_placement(&env.config, SET, "Pong (USA)", env.folder.id, &["pong.zip"])
        .unwrap();
    env.audit(AuditEvidence::Full);
    let plan = env.plan(&[]);
    let seq = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Publish { .. }))
        .unwrap() as u32;
    let target = env.library().join("Pong (USA).zip");
    let spoiled = target.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |at: Fault| {
            if at == Fault::RepairMoved(seq) {
                std::fs::write(&spoiled, b"spoiled").unwrap();
            }
        })));

    let done = env.repair(&plan);
    env.store.hook_faults(None);

    let placed: Vec<(String, String)> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .map(|p| (p.game, p.path))
        .collect();
    assert_eq!(
        placed,
        [("Pong (USA)".to_owned(), "pong.zip".to_owned())],
        "the spoiled copy is never recorded placed, and the original stays"
    );
    assert!(env.library().join("pong.zip").is_file());
    let outcome = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Pong (USA)")
        .unwrap();
    assert!(
        !outcome.done
            && outcome
                .error
                .as_deref()
                .is_some_and(|e| e.contains("its copy changed since it was published")),
        "{outcome:?}"
    );
    let kept = env.store.source_kept_files(SET).unwrap();
    assert!(
        kept.iter()
            .any(|k| k.kept_name.starts_with("Pong (USA).zip")),
        "the repack it staged is kept, not deleted: {kept:?}"
    );
}

#[test]
fn an_audit_never_follows_its_library_folder_itself_replaced_by_a_symbolic_link() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    assert_eq!(
        Env::statuses(&env.audit(AuditEvidence::Full))["Pong (USA)"],
        GameStatus::Complete
    );
    // The folder, moved to a sibling in the same Root, and linked to from where it was.
    let sibling = env.tmp.path().join("roots/play/elsewhere");
    std::fs::rename(env.library(), &sibling).unwrap();
    std::os::unix::fs::symlink(&sibling, env.library()).unwrap();

    for evidence in [AuditEvidence::Incremental, AuditEvidence::Full] {
        let report = env.audit(evidence);

        let pong = placed_path(env.folder.id, "Pong (USA).zip");
        assert!(
            report
                .entries
                .iter()
                .all(|e| e.path != pong || e.finding == Finding::Unreadable),
            "{:#?}",
            report.entries
        );
        assert!(
            report
                .entries
                .iter()
                .any(|e| e.path == pong && e.finding == Finding::Unreadable),
            "{:#?}",
            report.entries
        );
        assert_ne!(Env::statuses(&report)["Pong (USA)"], GameStatus::Complete);
    }
}

#[test]
fn a_check_reads_again_a_file_both_built_and_placed_put_in_place_with_its_size_and_modification_time_kept()
 {
    let env = Env::new();
    torrentzip(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    let sha256 = env.audit(AuditEvidence::Full).dat_revision.sha256;
    let selection = select(&env, "Pong (USA)", &sha256);
    let plan = env
        .store
        .plan_build(&env.config, env.folder.id, selection)
        .unwrap();
    let build = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, build.id);
    let built = env
        .store
        .run_build_job(&env.config, build.id, None)
        .unwrap();
    assert_eq!(built.state, JobState::Succeeded, "{built:?}");
    std::fs::remove_file(env.set().join("Pong (USA).zip")).unwrap();
    env.store
        .record_placement(
            &env.config,
            SET,
            "Pong (USA)",
            env.folder.id,
            &["Pong (USA).zip"],
        )
        .unwrap();
    let outcome = |check: &FolderCheck| {
        let file = check
            .files
            .iter()
            .find(|f| f.target == "Pong (USA).zip")
            .unwrap();
        assert!(file.placed, "{file:?}");
        file.outcome
    };
    assert_eq!(outcome(&env.check()), FileOutcome::Unchanged);

    // As long, holding other bytes, put in its place by a rename with its modification time set back.
    let path = env.library().join("Pong (USA).zip");
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let staged = env.library().join("staged.tmp");
    std::fs::write(&staged, &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&staged)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    std::fs::rename(&staged, &path).unwrap();

    assert_eq!(outcome(&env.check()), FileOutcome::Conflict);
}

#[test]
fn a_stop_once_a_fill_of_a_vanished_placed_game_is_done_leaves_it_no_longer_placed() {
    let env = Env::new();
    env.place("Pong (USA)", &[("Pong (USA).a26", b"pong")]);
    std::fs::remove_file(env.library().join("Pong (USA).zip")).unwrap();
    env.ingest("pong.a26", b"pong");
    env.audit(AuditEvidence::Full);
    let plan = env.plan(&[]);
    let seq = plan
        .content
        .steps
        .iter()
        .position(|s| matches!(s.step, RepairStep::Fill { .. }))
        .unwrap() as u32;
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    env.store.inject_fault(Some(Fault::RepairStepDone(seq)));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_repair_job(&env.config, job, None)
    }));
    assert!(ran.is_err(), "{ran:?}");
    env.store.inject_fault(None);

    assert!(env.set().join("Pong (USA).zip").is_file());
    assert!(
        env.store.placed_files(SET).unwrap().is_empty(),
        "the fill and the placement it ends are recorded together"
    );
    let requeued = env.store.recover_jobs(WORKERS).unwrap()[0]
        .requeued
        .clone()
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, requeued.id);
    env.store
        .run_repair_job(&env.config, requeued.id, None)
        .unwrap();
    assert!(env.store.placed_files(SET).unwrap().is_empty());
}

/// The path and Game of each of Alpha's placed files.
fn placed(env: &Env) -> Vec<(String, String)> {
    let mut placed: Vec<(String, String)> = env
        .store
        .placed_files(SET)
        .unwrap()
        .into_iter()
        .map(|p| (p.path, p.game))
        .collect();
    placed.sort();
    placed
}

#[test]
fn a_fill_of_a_partly_vanished_loose_placed_game_adds_the_missing_file_beside_the_survivors() {
    let env = Env::new();
    let (a, b) = (
        "Joust (USA)/Joust (USA) (Side A).a26",
        "Joust (USA)/Joust (USA) (Side B).a26",
    );
    std::fs::create_dir_all(env.library().join("Joust (USA)")).unwrap();
    std::fs::write(env.library().join(a), b"joust-a").unwrap();
    std::fs::write(env.library().join(b), b"joust-b").unwrap();
    env.store
        .record_placement(&env.config, SET, "Joust (USA)", env.folder.id, &[a, b])
        .unwrap();
    std::fs::remove_file(env.library().join(b)).unwrap();
    env.ingest("side-b.a26", b"joust-b");
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    let target = placed_path(env.folder.id, b);
    assert!(
        plan.content
            .steps
            .iter()
            .any(|s| matches!(&s.step, RepairStep::Fill { target: t, .. } if *t == target)),
        "{:#?}",
        plan.content.steps
    );

    env.repair(&plan);

    assert_eq!(std::fs::read(env.library().join(b)).unwrap(), b"joust-b");
    assert_eq!(std::fs::read(env.library().join(a)).unwrap(), b"joust-a");
    assert!(
        !env.set().join("Joust (USA)").exists(),
        "a game is never split between two folders"
    );
    assert_eq!(
        placed(&env),
        [
            (a.to_owned(), "Joust (USA)".to_owned()),
            (b.to_owned(), "Joust (USA)".to_owned())
        ]
    );
    let latest = env.store.history(SET).unwrap()[0].id;
    let statuses = Env::statuses(&env.store.report(latest).unwrap());
    assert_eq!(statuses["Joust (USA)"], GameStatus::Complete);
    let check = env.check();
    assert!(
        check
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged),
        "{check:#?}"
    );
}

#[test]
fn a_fill_of_a_partly_vanished_zipped_placed_game_rebuilds_it_in_place() {
    let env = Env::new();
    env.place(
        "Joust (USA)",
        &[
            ("Joust (USA) (Side A).a26", b"joust-a"),
            ("Joust (USA) (Side B).a26", b"joust-b"),
        ],
    );
    // Its zip loses a member: rewritten without it, as a tool outside Title Select might.
    torrentzip(
        &env.library().join("Joust (USA).zip"),
        &[("Joust (USA) (Side A).a26", b"joust-a")],
    );
    // A fill takes a Game whole from one ingest file, as it does into the source set's folder.
    let whole = env.tmp.path().join("joust.zip");
    torrentzip(
        &whole,
        &[
            ("Joust (USA) (Side A).a26", b"joust-a"),
            ("Joust (USA) (Side B).a26", b"joust-b"),
        ],
    );
    env.ingest("joust.zip", &std::fs::read(&whole).unwrap());
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    let target = placed_path(env.folder.id, "Joust (USA).zip");
    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.game.as_deref() == Some("Joust (USA)"))
        .unwrap_or_else(|| panic!("{:#?}", plan.content.notes));
    assert_eq!(
        item.chosen,
        RepairChoice::Apply,
        "{item:#?}\n{:#?}",
        plan.content.steps
    );
    assert!(
        plan.content.steps.iter().any(|s| s.item == item.key
            && matches!(&s.step, RepairStep::Fill { target: t, .. } | RepairStep::Publish { target: t, .. }
                if *t == target)),
        "{:#?}",
        plan.content.steps
    );

    env.repair(&plan);

    assert!(
        !env.set().join("Joust (USA).zip").exists(),
        "a game is never split between two folders"
    );
    assert_eq!(
        placed(&env),
        [("Joust (USA).zip".to_owned(), "Joust (USA)".to_owned())]
    );
    let latest = env.store.history(SET).unwrap()[0].id;
    let statuses = Env::statuses(&env.store.report(latest).unwrap());
    assert_eq!(statuses["Joust (USA)"], GameStatus::Complete);
    assert!(
        env.check()
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged)
    );
}

#[test]
fn a_fill_where_a_game_is_placed_writes_only_through_the_folder_it_checked_and_records_only_what_is_there()
 {
    // The Library Folder moved away and an empty folder put at its path: before the fill begins, or while its file is
    // being written.
    for during in [false, true] {
        let env = Env::new();
        env.place("Joust (USA)", &[("Joust (USA) (Side A).a26", b"joust-a")]);
        let whole = env.tmp.path().join("joust.zip");
        torrentzip(
            &whole,
            &[
                ("Joust (USA) (Side A).a26", b"joust-a"),
                ("Joust (USA) (Side B).a26", b"joust-b"),
            ],
        );
        env.ingest("joust.zip", &std::fs::read(&whole).unwrap());
        env.audit(AuditEvidence::Full);
        let plan = env.plan(&[]);
        let keep = plan
            .content
            .steps
            .iter()
            .position(|s| matches!(s.step, RepairStep::Keep { .. }))
            .unwrap() as u32;
        let (library, moved) = (env.library(), env.tmp.path().join("roots/play/moved"));
        let swapped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let did = swapped.clone();
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                let now = match during {
                    false => fault == Fault::RepairStepDone(keep),
                    true => matches!(fault, Fault::IngestRenaming | Fault::IngestStaged),
                };
                if now && !did.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    std::fs::rename(&library, &moved).unwrap();
                    std::fs::create_dir(&library).unwrap();
                }
            })));
        let approved = env
            .store
            .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let job = approved.job.unwrap();
        assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
        env.store.run_repair_job(&env.config, job, None).unwrap();
        env.store.hook_faults(None);

        assert!(
            swapped.load(std::sync::atomic::Ordering::SeqCst),
            "during={during}"
        );
        assert!(
            Env::files_under(&env.library()).is_empty(),
            "during={during}: nothing is written in what was put in the folder's place"
        );
        let done = env.store.repair_plan(plan.id).unwrap();
        let fill = done
            .outcomes
            .iter()
            .find(|o| o.item == "game:Joust (USA)")
            .unwrap();
        assert!(
            !fill.done
                && fill
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("was moved or replaced since this repair began")),
            "during={during}: {:#?}",
            done.outcomes
        );
        // Nothing is recorded placed where no file of it is.
        for (path, _) in placed(&env) {
            assert!(
                env.library().join(&path).is_file(),
                "during={during}: {path}"
            );
        }
    }
}

#[test]
fn a_check_of_a_loose_placed_game_reports_a_file_beside_its_files_as_drift() {
    let env = Env::new();
    let (a, b) = (
        "Joust (USA)/Joust (USA) (Side A).a26",
        "Joust (USA)/Joust (USA) (Side B).a26",
    );
    std::fs::create_dir_all(env.library().join("Joust (USA)")).unwrap();
    std::fs::write(env.library().join(a), b"joust-a").unwrap();
    std::fs::write(env.library().join(b), b"joust-b").unwrap();
    env.store
        .record_placement(&env.config, SET, "Joust (USA)", env.folder.id, &[a, b])
        .unwrap();
    std::fs::write(env.library().join("Joust (USA)/extra.a26"), b"unrecorded").unwrap();

    let check = env.check();

    assert!(
        check
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged),
        "{check:#?}"
    );
    let drift: Vec<&str> = check.drift.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(drift, ["Joust (USA)/extra.a26"], "{check:#?}");
}

#[test]
fn a_fill_where_a_game_is_placed_copies_across_filesystems() {
    let env = Env::new();
    env.store.copy_across_filesystems(true);
    let (a, b) = (
        "Joust (USA)/Joust (USA) (Side A).a26",
        "Joust (USA)/Joust (USA) (Side B).a26",
    );
    std::fs::create_dir_all(env.library().join("Joust (USA)")).unwrap();
    std::fs::write(env.library().join(a), b"joust-a").unwrap();
    env.store
        .record_placement(&env.config, SET, "Joust (USA)", env.folder.id, &[a])
        .unwrap();
    env.ingest("side-b.a26", b"joust-b");
    env.audit(AuditEvidence::Full);
    let ingested = std::fs::metadata(env.tmp.path().join("in/side-b.a26")).unwrap();
    let copied = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let did = copied.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::IngestStaged {
                did.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        })));

    let plan = env.plan(&[]);
    env.repair(&plan);
    env.store.hook_faults(None);

    assert!(
        copied.load(std::sync::atomic::Ordering::SeqCst),
        "it was copied, not renamed"
    );
    let written = std::fs::metadata(env.library().join(b)).unwrap();
    assert_ne!(
        std::os::unix::fs::MetadataExt::ino(&written),
        std::os::unix::fs::MetadataExt::ino(&ingested),
        "a copy is a new file"
    );
    assert_eq!(std::fs::read(env.library().join(b)).unwrap(), b"joust-b");
    assert!(!env.tmp.path().join("in/side-b.a26").exists());
    assert_eq!(
        placed(&env),
        [
            (a.to_owned(), "Joust (USA)".to_owned()),
            (b.to_owned(), "Joust (USA)".to_owned())
        ]
    );
    let latest = env.store.history(SET).unwrap()[0].id;
    let statuses = Env::statuses(&env.store.report(latest).unwrap());
    assert_eq!(statuses["Joust (USA)"], GameStatus::Complete);
    assert!(
        env.check()
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged)
    );
}

/// Alpha with Joust placed loose, holding only its side A, and its side B in the Ingest Folder, audited.
fn joust_half_placed() -> (Env, &'static str, &'static str) {
    let env = Env::new();
    let (a, b) = (
        "Joust (USA)/Joust (USA) (Side A).a26",
        "Joust (USA)/Joust (USA) (Side B).a26",
    );
    std::fs::create_dir_all(env.library().join("Joust (USA)")).unwrap();
    std::fs::write(env.library().join(a), b"joust-a").unwrap();
    env.store
        .record_placement(&env.config, SET, "Joust (USA)", env.folder.id, &[a])
        .unwrap();
    env.ingest("side-b.a26", b"joust-b");
    env.audit(AuditEvidence::Full);
    (env, a, b)
}

/// Approves `plan` and runs its Repair Job with `hook` at every fault point, without the Audit it owes.
fn repair_hooked(
    env: &Env,
    plan: &RepairPlan,
    hook: impl Fn(Fault) + Send + Sync + 'static,
) -> RepairPlan {
    env.store.hook_faults(Some(std::sync::Arc::new(hook)));
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    env.store.run_repair_job(&env.config, job, None).unwrap();
    env.store.hook_faults(None);
    env.store.repair_plan(plan.id).unwrap()
}

#[test]
fn a_fill_across_filesystems_never_deletes_its_original_once_its_copy_changes_before_the_last_check()
 {
    // Its published copy written over in place, or removed, once its original is set aside to delete.
    for remove in [false, true] {
        let (env, a, b) = joust_half_placed();
        env.store.copy_across_filesystems(true);
        let plan = env.plan(&[]);
        let copy = env.library().join(b);
        let done = repair_hooked(&env, &plan, move |fault| {
            if fault == Fault::IngestIsolated {
                match remove {
                    true => std::fs::remove_file(&copy).unwrap(),
                    false => std::fs::write(&copy, b"spoiled").unwrap(),
                }
            }
        });

        assert_eq!(
            std::fs::read(env.tmp.path().join("in/side-b.a26")).unwrap(),
            b"joust-b",
            "remove={remove}: the original is put back"
        );
        assert_eq!(
            placed(&env),
            [(a.to_owned(), "Joust (USA)".to_owned())],
            "remove={remove}: nothing more is recorded placed"
        );
        let fill = done
            .outcomes
            .iter()
            .find(|o| o.item == "game:Joust (USA)")
            .unwrap();
        assert!(
            !fill.done
                && fill
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("side-b.a26 was not deleted, since")),
            "remove={remove}: {:#?}",
            done.outcomes
        );
    }
}

#[test]
fn a_fill_where_a_game_is_placed_keeps_its_original_when_the_folder_moves_before_it_is_consumed() {
    // The Library Folder moved away and an empty folder put at its path once the file is in it: by a rename, or by
    // a copy whose original is set aside to delete.
    for across in [false, true] {
        let (env, a, b) = joust_half_placed();
        env.store.copy_across_filesystems(across);
        let plan = env.plan(&[]);
        let (library, moved) = (env.library(), env.tmp.path().join("roots/play/moved"));
        let away = moved.clone();
        let swapped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let did = swapped.clone();
        let done = repair_hooked(&env, &plan, move |fault| {
            let now = match across {
                false => fault == Fault::IngestMoved,
                true => fault == Fault::IngestIsolated,
            };
            if now && !did.swap(true, std::sync::atomic::Ordering::SeqCst) {
                std::fs::rename(&library, &away).unwrap();
                std::fs::create_dir(&library).unwrap();
            }
        });

        assert!(
            swapped.load(std::sync::atomic::Ordering::SeqCst),
            "across={across}"
        );
        assert_eq!(
            std::fs::read(env.tmp.path().join("in/side-b.a26")).unwrap(),
            b"joust-b",
            "across={across}: the original stays in the ingest folder"
        );
        assert!(
            !moved.join(b).exists(),
            "across={across}: nothing of it is left in the folder moved away"
        );
        assert!(
            Env::files_under(&env.library()).is_empty(),
            "across={across}"
        );
        assert_eq!(
            placed(&env),
            [(a.to_owned(), "Joust (USA)".to_owned())],
            "across={across}"
        );
        let fill = done
            .outcomes
            .iter()
            .find(|o| o.item == "game:Joust (USA)")
            .unwrap();
        assert!(
            !fill.done
                && fill
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("was moved or replaced")),
            "across={across}: {:#?}",
            done.outcomes
        );
    }
}

/// As the service does when it next starts: requeues the Jobs a stop interrupted, recovers the moves out of the
/// Ingest Folder a stop left, and runs a Repair Job for each repair not finished. Returns what recovering the moves
/// left unfinished or ended, with why, and each Repair Job as it ended.
fn restart(env: &Env) -> (Vec<(i64, String)>, Vec<title_select_api::Job>) {
    let mut jobs: Vec<i64> = env
        .store
        .recover_jobs(WORKERS)
        .unwrap()
        .into_iter()
        .filter_map(|r| r.requeued.map(|j| j.id))
        .collect();
    let left = env.store.recover_ingest_moves(&env.config).unwrap();
    jobs.extend(
        env.store
            .recover_repairs()
            .unwrap()
            .into_iter()
            .map(|j| j.id),
    );
    let ended = jobs
        .into_iter()
        .map(|id| {
            assert_eq!(env.store.claim_job().unwrap().unwrap().id, id);
            env.store.run_repair_job(&env.config, id, None).unwrap()
        })
        .collect();
    (left, ended)
}

/// Runs `plan` with `hook` at every fault point, catching a stop the hook makes.
fn repair_stopped(env: &Env, plan: &RepairPlan, hook: impl Fn(Fault) + Send + Sync + 'static) {
    env.store.hook_faults(Some(std::sync::Arc::new(hook)));
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_repair_job(&env.config, job, None)
    }));
    env.store.hook_faults(None);
    assert!(ran.is_err(), "{ran:?}");
}

/// The files in `dir` and every folder under it, with their contents, as `cp -R` makes them.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir(to).unwrap();
    std::fs::set_permissions(to, std::fs::metadata(from).unwrap().permissions()).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
}

#[test]
fn an_original_that_cannot_be_returned_yet_leaves_its_fill_unfinished_until_it_is() {
    let (env, a, b) = joust_half_placed();
    env.store.copy_across_filesystems(true);
    let plan = env.plan(&[]);
    let (copy, name, retained) = (
        env.library().join(b),
        env.tmp.path().join("in/side-b.a26"),
        env.tmp.path().join("in/.title-select/retained"),
    );
    let blocked = retained.clone();
    // Its copy removed once its original is set aside to delete, its name taken, and no kept files can be made: a
    // file in place of each generation of them not made yet.
    let done = repair_hooked(&env, &plan, move |fault| {
        if fault == Fault::IngestIsolated {
            std::fs::remove_file(&copy).unwrap();
            std::fs::write(&name, b"squatter").unwrap();
            for generation in 1..=20 {
                let at = blocked.join(generation.to_string());
                if !at.exists() {
                    std::fs::write(at.with_extension("blocked"), b"").unwrap();
                    std::fs::write(at, b"").unwrap();
                }
            }
        }
    });

    assert!(done.finished_at.is_none(), "{done:#?}");
    let deleting = Env::files_under(&retained.join(".deleting"));
    assert_eq!(
        deleting.len(),
        1,
        "the original is still set aside: {deleting:?}"
    );
    let (left, _) = restart(&env);
    assert!(
        left.iter()
            .any(|(_, why)| why.contains("could not be returned to the ingest folder yet")),
        "{left:?}"
    );

    // Once nothing is in the way, the next start returns it.
    for generation in 1..=20 {
        let at = retained.join(generation.to_string());
        if at.with_extension("blocked").exists() {
            std::fs::remove_file(at.with_extension("blocked")).unwrap();
            std::fs::remove_file(at).unwrap();
        }
    }
    std::fs::remove_file(env.tmp.path().join("in/side-b.a26")).unwrap();
    let (_, ended) = restart(&env);

    assert_eq!(
        std::fs::read(env.tmp.path().join("in/side-b.a26")).unwrap(),
        b"joust-b"
    );
    assert!(Env::files_under(&retained.join(".deleting")).is_empty());
    assert_eq!(placed(&env), [(a.to_owned(), "Joust (USA)".to_owned())]);
    assert!(
        ended.iter().all(|j| j.state == JobState::Succeeded),
        "{ended:?}"
    );
    assert!(
        env.store
            .repair_plan(plan.id)
            .unwrap()
            .finished_at
            .is_some()
    );
}

#[test]
fn a_stop_while_an_original_is_returned_into_the_kept_files_finishes_the_return_and_records_it() {
    let (env, a, b) = joust_half_placed();
    env.store.copy_across_filesystems(true);
    let plan = env.plan(&[]);
    let (copy, name) = (env.library().join(b), env.tmp.path().join("in/side-b.a26"));
    // Its copy removed once its original is set aside to delete, and its name taken: the original is kept instead,
    // and the service stops just after it is moved into the kept files.
    repair_stopped(&env, &plan, move |fault| match fault {
        Fault::IngestIsolated => {
            std::fs::remove_file(&copy).unwrap();
            std::fs::write(&name, b"squatter").unwrap();
        }
        Fault::IngestSetAside => panic!("stopped as the original is kept"),
        _ => {}
    });

    let (_, ended) = restart(&env);

    let kept = env.store.ingest_kept_files("in").unwrap();
    assert_eq!(kept.len(), 1, "the original is recorded kept: {kept:?}");
    let at = env
        .tmp
        .path()
        .join("in/.title-select/retained")
        .join(kept[0].generation.to_string())
        .join(&kept[0].kept_name);
    assert_eq!(std::fs::read(at).unwrap(), b"joust-b");
    assert_eq!(placed(&env), [(a.to_owned(), "Joust (USA)".to_owned())]);
    assert!(
        ended.iter().all(|j| j.state == JobState::Succeeded),
        "{ended:?}"
    );
    let done = env.store.repair_plan(plan.id).unwrap();
    let fill = done
        .outcomes
        .iter()
        .find(|o| o.item == "game:Joust (USA)")
        .unwrap();
    assert!(!fill.done, "{fill:?}");
}

#[test]
fn a_fill_never_consumes_its_original_once_the_folder_it_goes_in_within_the_library_folder_moves() {
    // The loose game's own folder moved away within the Library Folder, and an empty one put at its path, once the
    // file is in it: by a rename, or by a copy whose original is set aside to delete.
    for across in [false, true] {
        let (env, a, b) = joust_half_placed();
        env.store.copy_across_filesystems(across);
        let plan = env.plan(&[]);
        let (game, moved) = (
            env.library().join("Joust (USA)"),
            env.library().join("Joust (USA) moved"),
        );
        let away = moved.clone();
        let done = repair_hooked(&env, &plan, move |fault| {
            let now = match across {
                false => fault == Fault::IngestMoved,
                true => fault == Fault::IngestIsolated,
            };
            if now {
                std::fs::rename(&game, &away).unwrap();
                std::fs::create_dir(&game).unwrap();
            }
        });

        assert_eq!(
            std::fs::read(env.tmp.path().join("in/side-b.a26")).unwrap(),
            b"joust-b",
            "across={across}: the original stays in the ingest folder"
        );
        let leaf = b.rsplit_once('/').unwrap().1;
        assert!(!moved.join(leaf).exists(), "across={across}");
        assert!(!env.library().join(b).exists(), "across={across}");
        assert_eq!(placed(&env), [(a.to_owned(), "Joust (USA)".to_owned())]);
        let fill = done
            .outcomes
            .iter()
            .find(|o| o.item == "game:Joust (USA)")
            .unwrap();
        assert!(
            !fill.done
                && fill
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("or a folder in it was moved or replaced")),
            "across={across}: {:#?}",
            done.outcomes
        );
    }
}

#[test]
fn a_stop_once_a_fill_went_into_a_library_folder_since_moved_leaves_it_unfinished_until_the_folder_is_back()
 {
    for across in [false, true] {
        let (env, a, b) = joust_half_placed();
        env.store.copy_across_filesystems(across);
        let plan = env.plan(&[]);
        let (library, moved) = (env.library(), env.tmp.path().join("roots/play/moved"));
        let (here, away) = (library.clone(), moved.clone());
        // Moved away, an empty folder put at its path, and the service stopped: once the file is in it by a rename,
        // or once a copy's original is set aside to delete.
        repair_stopped(&env, &plan, move |fault| {
            let now = match across {
                false => fault == Fault::IngestMoved,
                true => fault == Fault::IngestIsolated,
            };
            if now {
                std::fs::rename(&here, &away).unwrap();
                std::fs::create_dir(&here).unwrap();
                panic!("stopped with the folder moved");
            }
        });
        let unfinished = |left: &[(i64, String)], ended: &[title_select_api::Job]| {
            assert!(
                left.iter()
                    .any(|(_, why)| why.contains("put the folder back at play/atari")),
                "across={across}: {left:?}"
            );
            assert!(
                ended.iter().all(|j| j.state == JobState::Failed
                    && j.error
                        .as_deref()
                        .is_some_and(|e| e.contains("put the folder back at play/atari"))),
                "across={across}: {ended:?}"
            );
            assert!(
                env.store
                    .repair_plan(plan.id)
                    .unwrap()
                    .finished_at
                    .is_none()
            );
            assert_eq!(placed(&env), [(a.to_owned(), "Joust (USA)".to_owned())]);
            // The repair it belongs to holds the source set, and the move the folders it goes from and into.
            for held in [
                env.store
                    .unregister_ingest_folder(&env.config, "in")
                    .unwrap_err(),
                env.store
                    .unregister_library_folder(&env.config, env.folder.id)
                    .unwrap_err(),
                env.store
                    .unregister_source_set(&env.config, SET)
                    .unwrap_err(),
            ] {
                assert!(matches!(held, Error::Conflict(_)), "{held:?}");
            }
            match env
                .store
                .unregister_ingest_folder(&env.config, "in")
                .unwrap_err()
            {
                Error::Conflict(why) => assert!(why.contains("put the folder back"), "{why}"),
                other => panic!("{other:?}"),
            }
        };

        let (left, ended) = restart(&env);
        unfinished(&left, &ended);
        // A copy of the folder, its marker and all, is still not the folder the file went into.
        std::fs::remove_dir(&library).unwrap();
        copy_tree(&moved, &library);
        let (left, ended) = restart(&env);
        unfinished(&left, &ended);

        // The folder itself back: the move, and the repair, finish.
        std::fs::remove_dir_all(&library).unwrap();
        std::fs::rename(&moved, &library).unwrap();
        let (left, ended) = restart(&env);

        assert!(left.is_empty(), "across={across}: {left:?}");
        assert!(
            ended.iter().all(|j| j.state == JobState::Succeeded),
            "across={across}: {ended:?}"
        );
        assert!(
            env.store
                .repair_plan(plan.id)
                .unwrap()
                .finished_at
                .is_some()
        );
        assert_eq!(std::fs::read(env.library().join(b)).unwrap(), b"joust-b");
        assert!(
            !env.tmp.path().join("in/side-b.a26").exists(),
            "across={across}"
        );
        assert_eq!(
            placed(&env),
            [
                (a.to_owned(), "Joust (USA)".to_owned()),
                (b.to_owned(), "Joust (USA)".to_owned())
            ],
            "across={across}"
        );
    }
}
