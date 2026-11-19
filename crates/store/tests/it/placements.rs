//! Placements (ADR 0023): a Source Set's Games placed in the Library Folder that holds them, and the Audits, checks,
//! Repairs and unregistering that follow them there. No Build places a Game yet, so each test records its Placements
//! directly, from synthetic DATs, Source Sets and files.

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
use title_select_store::{Error, Store, Workers};

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
        for dir in ["roots/sets/Alpha", "roots/play", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"Alpha\"]\n\n[roots.play]\npath = {:?}\nwritable = true\n",
            tmp.path().join("roots/sets").display().to_string(),
            tmp.path().join("roots/play").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
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
fn an_audit_and_a_build_of_the_holding_folder_exclude_each_other() {
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
    // With nothing placed, an audit doesn't wait for a build of the folder.
    let build = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let audit = env
        .store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental)
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, build.id);
    let built = env
        .store
        .run_build_job(&env.config, build.id, None)
        .unwrap();
    assert_eq!(built.state, JobState::Succeeded, "{built:?}");
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, audit.id);
    env.store
        .run_audit_job(&env.config, audit.id, None)
        .unwrap();
    env.place("Combat (USA)", &[("Combat (USA).a26", b"combat")]);

    let audit = env
        .store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental)
        .unwrap();
    let plan = env
        .store
        .plan_library_removal(&env.config, env.folder.id)
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
        .plan_library_removal(&env.config, env.folder.id)
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
fn a_repair_leaves_a_placed_game_it_would_have_to_write_anew() {
    let env = Env::new();
    // Pong's placed ZIP holds its ROM under another name: only a repack would repair it.
    env.place("Pong (USA)", &[("pong.a26", b"pong")]);
    env.audit(AuditEvidence::Full);

    let plan = env.plan(&[]);

    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "game:Pong (USA)")
        .unwrap();
    assert_eq!(item.chosen, RepairChoice::Leave);
    assert_eq!(item.choices, [RepairChoice::Leave]);
    assert!(
        item.note
            .as_deref()
            .unwrap()
            .contains("placed in a play library folder"),
        "{item:?}"
    );
    assert!(plan.content.steps.is_empty(), "{:#?}", plan.content.steps);
    let request = PlanRepair {
        choices: [("game:Pong (USA)".to_owned(), RepairChoice::Apply)].into(),
        additions: Default::default(),
    };
    assert!(matches!(
        env.store.plan_repair(&env.config, SET, &request),
        Err(Error::Refused(_))
    ));
}
