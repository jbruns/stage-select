//! Binding a Source Set to another DAT in place: an Atari 7800 Source Set registered against the headered (A78) DAT
//! whose files match the headerless (BIN) one, from synthetic DATs and files.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::common::*;
use stage_select_api::build::{AddLibraryFolder, BlockerKind, Stale};
use stage_select_api::repair::PlanRepair;
use stage_select_api::roots::{RootSpec, VoidedPlanKind};
use stage_select_api::selection::{
    CoverageDrift, KeptGame, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary,
};
use stage_select_api::{AuditEvidence, JobKind, JobState, SourceSetStale};
use stage_select_store::{Error, Store, Workers};

const SET: &str = "A7800";
const A78: &str = "No-Intro/Atari - 7800 (A78)";
const BIN: &str = "No-Intro/Atari - 7800 (BIN)";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// A store with the Source Set A7800, which Stage Select may change, bound to the (A78) DAT, holding Pong as both
/// DATs name it and a misnamed Combat of the (A78) DAT; the (BIN) DAT imported beside it; and a writable Root `play`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    a78: String,
    bin: String,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/A7800", "roots/play", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", tmp.path().join("roots/sets")),
                RootSpec::new("play", tmp.path().join("roots/play")).library(),
            ],
        );
        let dats = tmp.path().join("dats");
        let a78 = import(
            &store,
            &dats,
            "Atari - 7800 (A78)",
            &[
                ("Pong (USA)", vec![rom("Pong (USA).a78", b"headerpong")]),
                (
                    "Combat (USA)",
                    vec![rom("Combat (USA).a78", b"headercombat")],
                ),
            ],
        );
        let bin = import(
            &store,
            &dats,
            "Atari - 7800 (BIN)",
            &[
                ("Pong (USA)", vec![rom("Pong (USA).bin", b"pong")]),
                ("Combat (USA)", vec![rom("Combat (USA).bin", b"combat")]),
            ],
        );
        store.add_source_set("sets", SET, A78).unwrap();
        crate::let_change(&store, SET);
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
        let env = Env {
            tmp,
            store,
            a78,
            bin,
        };
        write_file(&env.set_dir().join("Pong (USA).a78"), b"headerpong");
        write_file(&env.set_dir().join("Pong (USA).bin"), b"pong");
        write_file(&env.set_dir().join("combat.a78"), b"headercombat");
        env
    }

    fn set_dir(&self) -> PathBuf {
        self.tmp.path().join("roots/sets").join(SET)
    }

    fn audit(&self) {
        self.store
            .audit_source_set(SET, None, AuditEvidence::Full)
            .unwrap();
    }

    fn change(&self, dat: &str) -> stage_select_store::Result<stage_select_api::DatChange> {
        self.store.change_source_set_dat(SET, dat, "Operator")
    }

    /// Refuses the change to `dat`, preview and all, with a conflict that says `why`; and changes nothing.
    fn refused(&self, dat: &str, why: &str) {
        let before = self.store.source_set(SET).unwrap();
        for refused in [self.store.dat_change(SET, dat), self.change(dat)] {
            match refused {
                Err(Error::Conflict(said)) => assert!(said.contains(why), "{said}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(self.store.source_set(SET).unwrap(), before);
    }

    /// The Library Folder `play/A7800`, built from a Selection of Pong of the (A78) DAT.
    fn built_folder(&self) -> (i64, i64) {
        let folder = self.store.add_library_folder(&add()).unwrap();
        let selection = self.select(&self.a78);
        let plan = self.store.plan_build(folder.id, selection).unwrap();
        let job = self
            .store
            .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        let ended = self.store.run_build_job(job.id, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        (folder.id, selection)
    }

    /// Records a Selection of Pong of the DAT Revision `sha256`.
    fn select(&self, sha256: &str) -> i64 {
        self.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        assert_eq!(job.kind, JobKind::Selection);
        self.store
            .record_selection(job.id, &pong(sha256))
            .unwrap()
            .selection
            .unwrap()
    }

    /// Plans a repair of A7800 and approves it, queueing its Job.
    fn approve_repair(&self) -> stage_select_api::repair::RepairPlan {
        let plan = self.store.plan_repair(SET, &PlanRepair::default()).unwrap();
        self.store
            .approve_repair(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap()
    }
}

fn import(
    store: &Store,
    dir: &std::path::Path,
    name: &str,
    games: &[(&str, Vec<RomSpec>)],
) -> String {
    let dat = Dat {
        file: &format!("{name}.dat"),
        name,
        ..Dat::default()
    };
    store
        .import_dat(&write_dat_as(dir, &dat, games), None)
        .unwrap()
        .0
        .sha256
}

fn add() -> AddLibraryFolder {
    AddLibraryFolder {
        root: "play".into(),
        path: Some(SET.into()),
        template: None,
        source_set: SET.into(),
        profile: PROFILE.into(),
    }
}

fn pong(sha256: &str) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SET.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups: vec![SelectionGroup {
            dependencies: Vec::new(),
            group: "pong".into(),
            unresolved: false,
            kept: vec![KeptGame {
                title: "Pong (USA)".into(),
                rule: Rule {
                    stage: "Parent selection".into(),
                    reference: None,
                    message: "m".into(),
                },
                unresolved: false,
            }],
            clones: vec![],
            left_out: vec![],
            choice: None,
            drift: false,
        }],
        stale_choices: BTreeMap::new(),
    }
}

#[test]
fn a_source_set_keeps_its_history_and_its_next_audit_is_against_its_new_dat() {
    let env = Env::new();
    env.audit();
    env.store.set_follow(SET, true).unwrap();
    // A second revision of the (BIN) DAT, which orders above the (A78) DAT's only one, though it is of another DAT.
    let dat = Dat {
        file: "bin-2.dat",
        name: "Atari - 7800 (BIN)",
        version: "20270101-000000",
        ..Dat::default()
    };
    let bin = env
        .store
        .import_dat(
            &write_dat_as(
                &env.tmp.path().join("dats"),
                &dat,
                &[("Pong (USA)", vec![rom("Pong (USA).bin", b"pong")])],
            ),
            None,
        )
        .unwrap()
        .0
        .sha256;
    assert!(env.store.source_set(SET).unwrap().stale.is_empty());
    let before = env.store.source_set(SET).unwrap();

    let preview = env.store.dat_change(SET, BIN).unwrap();
    assert_eq!((preview.from.as_str(), preview.to.as_str()), (A78, BIN));
    assert_eq!(
        env.store.source_set(SET).unwrap(),
        before,
        "a preview changes nothing"
    );

    let changed = env.change(BIN).unwrap();
    assert_eq!((changed.from.as_str(), changed.to.as_str()), (A78, BIN));
    assert_eq!(changed.source_set.dat, BIN);
    assert!(changed.source_set.follow, "following stays on");
    assert_eq!(changed.source_set.added_at, before.added_at);
    assert!(changed.voided.is_empty() && changed.library_folders.is_empty());
    assert!(
        matches!(
            changed.source_set.stale.as_slice(),
            [SourceSetStale::DatChanged { from, .. }] if from == A78
        ),
        "only its DAT changing makes it stale, not a revision of another DAT: {:?}",
        changed.source_set.stale
    );
    let history = env.store.history(SET).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].dat_revision_sha256, env.a78,
        "the audit keeps the revision it used"
    );

    match env.store.plan_repair(SET, &PlanRepair::default()) {
        Err(Error::Refused(why)) => assert!(why.contains("audit it against"), "{why}"),
        other => panic!("a repair waits for an audit against the new DAT: {other:?}"),
    }

    env.audit();
    let history = env.store.history(SET).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].dat_revision_sha256, bin,
        "its new DAT's newest revision"
    );
    assert_eq!(history[1].dat_revision_sha256, env.a78);
    assert!(env.store.source_set(SET).unwrap().stale.is_empty());
    env.store.plan_repair(SET, &PlanRepair::default()).unwrap();
}

#[test]
fn each_refusal_says_why_and_changes_nothing() {
    let env = Env::new();
    env.audit();
    env.refused(A78, "already bound to");

    let arcade = env.tmp.path().join("dats/mame.xml");
    std::fs::write(
        &arcade,
        r#"<?xml version="1.0"?>
<mame build="0.1">
  <machine name="pong"><rom name="pong.bin" size="4" crc="d87f7e0c"/></machine>
</mame>
"#,
    )
    .unwrap();
    let (arcade, _) = env.store.import_dat(&arcade, None).unwrap();
    env.refused(&arcade.dat, "choose a DAT of the same kind");

    let audit = env
        .store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Full)
        .unwrap();
    env.refused(BIN, &format!("audit job {} is queued", audit.id));
    env.store.claim_job().unwrap().unwrap();
    env.refused(BIN, &format!("audit job {} is running", audit.id));
    env.store.run_audit_job(audit.id, None).unwrap();

    let repair = env.approve_repair();
    let job = env.store.claim_job().unwrap().unwrap();
    assert_eq!(Some(job.id), repair.job);
    env.refused(
        BIN,
        &format!("repair plan {} is approved and has started", repair.id),
    );
    env.store.run_repair_job(job.id, None).unwrap();
    env.store.fulfil_repair_audits().unwrap();
    let owed = env.store.claim_job().unwrap().unwrap();
    env.store.run_audit_job(owed.id, None).unwrap();

    match env.store.dat_change(SET, "No-Intro/Nothing") {
        Err(Error::NotFound(why)) => assert!(why.contains("There is no DAT"), "{why}"),
        other => panic!("{other:?}"),
    }
    env.store.dat_change(SET, BIN).unwrap();
}

#[test]
fn a_game_placed_under_single_copy_must_be_sent_back_first() {
    let env = Env::new();
    env.audit();
    let folder = env.store.add_library_folder(&add()).unwrap();
    let dir = env.store.dir().to_path_buf();
    let Env {
        tmp,
        store,
        a78,
        bin,
    } = env;
    drop(store);
    rusqlite::Connection::open(dir.join(stage_select_store::DATABASE))
        .unwrap()
        .execute(
            "INSERT INTO placed_games (source_set_id, game, folder_id, placed_at)
             SELECT id, 'Pong (USA)', ?1, 'then' FROM source_sets WHERE name = ?2",
            rusqlite::params![folder.id, SET],
        )
        .unwrap();
    let env = Env {
        tmp,
        store: Store::open(&dir).unwrap(),
        a78,
        bin,
    };
    env.refused(BIN, "Send back to source");
}

#[test]
fn approved_plans_not_yet_started_are_voided_and_listed() {
    let env = Env::new();
    env.audit();
    let (folder, _) = env.built_folder();
    let selection = env.select(&env.a78);
    let plan = env.store.plan_build(folder, selection).unwrap();
    let build = env
        .store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let preview = env.store.dat_change(SET, BIN).unwrap();
    let changed = env.change(BIN).unwrap();
    assert_eq!(preview.voided, changed.voided);
    let voided: Vec<_> = changed
        .voided
        .iter()
        .map(|v| (v.kind, v.plan, v.job, v.writes_to.as_str()))
        .collect();
    assert_eq!(
        voided,
        [(
            VoidedPlanKind::Build,
            plan.id,
            build.id,
            "play library folder play/A7800"
        )]
    );
    let job = env.store.job(build.id).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert!(job.error.unwrap().contains("another DAT"));

    let env = Env::new();
    env.audit();
    let repair = env.approve_repair();
    let changed = env.change(BIN).unwrap();
    let voided: Vec<_> = changed
        .voided
        .iter()
        .map(|v| (v.kind, v.plan, v.job, v.writes_to.as_str()))
        .collect();
    assert_eq!(
        voided,
        [(
            VoidedPlanKind::Repair,
            repair.id,
            repair.job.unwrap(),
            "source set A7800"
        )]
    );
    let job = env.store.job(repair.job.unwrap()).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert!(job.error.unwrap().contains("another DAT"));
    assert!(
        env.store
            .repair_plan(repair.id)
            .unwrap()
            .finished_at
            .is_some()
    );
}

#[test]
fn a_folder_built_from_the_old_dat_is_stale_until_a_selection_of_the_new_one_is_built() {
    let env = Env::new();
    env.audit();
    let (folder, old) = env.built_folder();
    assert!(env.store.library_folder(folder).unwrap().stale.is_empty());

    let changed = env.change(BIN).unwrap();
    assert_eq!(
        changed
            .library_folders
            .iter()
            .map(|f| (f.id, f.root.as_str(), f.path.as_str()))
            .collect::<Vec<_>>(),
        [(folder, "play", SET)]
    );
    let stale = env.store.library_folder(folder).unwrap().stale;
    assert!(
        stale.contains(&Stale::DatChanged { built: A78.into() }),
        "{stale:?}"
    );
    let plan = env.store.plan_build(folder, old).unwrap();
    assert!(
        plan.content
            .blockers
            .iter()
            .any(|b| b.kind == BlockerKind::RevisionMismatch && b.message.contains(BIN)),
        "{:?}",
        plan.content.blockers
    );

    env.audit();
    let selection = env.select(&env.bin);
    let plan = env.store.plan_build(folder, selection).unwrap();
    assert!(
        plan.content.blockers.is_empty(),
        "{:?}",
        plan.content.blockers
    );
    let job = env
        .store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = env.store.run_build_job(job.id, None).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let stale = env.store.library_folder(folder).unwrap().stale;
    assert!(
        !stale.iter().any(|s| matches!(s, Stale::DatChanged { .. })),
        "{stale:?}"
    );
}

fn regions(game: &str) -> Vec<String> {
    ["USA", "Europe"]
        .into_iter()
        .filter(|r| game.contains(&format!("({r})")))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_coverage_stays_while_the_new_dat_has_games_in_its_regions_and_is_cleared_otherwise() {
    let env = Env::new();
    let europe = import(
        &env.store,
        &env.tmp.path().join("dats"),
        "Atari - 7800 (Europe)",
        &[("Pong (Europe)", vec![rom("Pong (Europe).bin", b"pongeu")])],
    );
    assert!(!europe.is_empty());
    env.store
        .set_coverage(SET, Some(&["USA".to_owned()]))
        .unwrap();
    match env.store.dat_change(SET, BIN) {
        Err(Error::RegionsUnavailable(why)) => assert!(why.contains("clone lists"), "{why}"),
        other => panic!("{other:?}"),
    }
    env.store.set_regions(regions);

    let cleared = env
        .store
        .dat_change(SET, "No-Intro/Atari - 7800 (Europe)")
        .unwrap();
    assert_eq!(cleared.cleared_coverage, Some(vec!["USA".to_owned()]));

    let kept = env.change(BIN).unwrap();
    assert_eq!(kept.cleared_coverage, None);
    assert_eq!(kept.source_set.coverage, Some(vec!["USA".to_owned()]));

    let cleared = env.change("No-Intro/Atari - 7800 (Europe)").unwrap();
    assert_eq!(cleared.cleared_coverage, Some(vec!["USA".to_owned()]));
    assert_eq!(cleared.source_set.coverage, None);
}
