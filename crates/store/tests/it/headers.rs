//! Headers in the store: a stored Audit keeps what a DAT's Header Rule found (ADR 0016).

use stage_select_api::roots::{RootKind, RootSpec};
use std::path::PathBuf;

use crate::common::*;
use std::collections::BTreeMap;

use stage_select_api::additions::AdditionState;
use stage_select_api::additions::NewAddition;
use stage_select_api::build::{
    AddLibraryFolder, CleanupAction, FileOutcome, FolderCheck, PlanCleanup,
};
use stage_select_api::ingest::{IngestAction, PlanIngest};
use stage_select_api::repair::{PlanRepair, RepairKind};
use stage_select_api::selection::{
    CoverageDrift, KeptGame, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, SystemSettings,
};
use stage_select_api::storage::{StorageFormat, SystemStorage};
use stage_select_api::{JobKind, JobState};
use stage_select_engine::{AuditEvidence, Finding, GameWarning, Reuse};
use stage_select_store::{Store, Workers};

const SET: &str = "No-Intro/Test - System";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

struct Env {
    tmp: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("roots/sets");
        std::fs::create_dir_all(root.join(SET)).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        for dir in ["roots/play", "roots/ingest", "roots/additions"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", root),
                RootSpec::new("play", tmp.path().join("roots/play")).library(),
                RootSpec::new("ingest", tmp.path().join("roots/ingest")).of_kind(RootKind::Ingest),
                RootSpec::new("additions", tmp.path().join("roots/additions"))
                    .of_kind(RootKind::Additions),
            ],
        );
        store
            .update_settings(
                &stage_select_api::settings::SettingsUpdate {
                    ingest_folders: Some(
                        [(
                            "ingest".to_owned(),
                            stage_select_api::settings::IngestCadence {
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
        Env { tmp, store }
    }

    /// Adds `payload`, put in the Ingest Folder as `name`, as the addition `addition` of the system.
    fn add_addition(&self, name: &str, payload: &[u8], addition: &str) {
        self.store.register_ingest_folder("ingest").unwrap();
        write_file(&self.tmp.path().join("roots/ingest").join(name), payload);
        for _ in 0..2 {
            let job = self
                .store
                .enqueue_ingest_scan("ingest", "Operator")
                .unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self.store.run_ingest_scan_job(job.id).unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
        let id = self
            .store
            .ingest_items("ingest")
            .unwrap()
            .into_iter()
            .find(|i| i.name == name)
            .unwrap()
            .id;
        let plan = self
            .store
            .plan_ingest(
                "ingest",
                &PlanIngest {
                    action: IngestAction::Addition,
                    items: vec![id],
                    additions: BTreeMap::from([(
                        id,
                        NewAddition {
                            system: SET.into(),
                            name: addition.into(),
                            parent: None,
                        },
                    )]),
                },
            )
            .unwrap();
        self.store
            .approve_ingest_plan(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.settle();
    }

    /// Claims and runs each queued Additions Job until none is left.
    fn settle(&self) {
        while let Some(job) = self.store.claim_job().unwrap() {
            assert_eq!(job.kind, JobKind::Additions, "{job:?}");
            let job = self.store.run_additions_job(job.id).unwrap();
            assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        }
    }

    fn set_dir(&self) -> PathBuf {
        self.tmp.path().join("roots/sets").join(SET)
    }

    fn add(&self, rule: &str, games: &[(&str, Vec<RomSpec>)]) {
        let dat = write_dat_with_header(&self.tmp.path().join("in"), rule, games);
        self.store.import_dat(&dat, None).unwrap();
        self.store
            .add_source_set("sets", SET, "No-Intro/Test - System")
            .unwrap();
        crate::let_change(&self.store, "Test - System");
    }

    fn audit(&self, evidence: AuditEvidence) -> stage_select_store::StoredAudit {
        self.store
            .audit_source_set("Test - System", None, evidence)
            .unwrap()
    }
}

fn ines() -> Vec<u8> {
    let mut h = b"NES\x1a".to_vec();
    h.resize(16, 0x01);
    h
}

#[test]
fn a_stored_audit_keeps_the_header_rule_each_header_and_the_warnings() {
    let env = Env::new();
    let zelda = noise(1, 64);
    let mario = noise(2, 64);
    env.add(
        "No-Intro_NES.xml",
        &[
            ("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)]),
            ("Mario (USA)", vec![rom("Mario (USA).nes", &mario)]),
        ],
    );
    write_file(
        &env.set_dir().join("Zelda (USA).nes"),
        &[ines(), zelda].concat(),
    );
    write_file(&env.set_dir().join("Mario (USA).nes"), &mario);

    let stored = env.audit(AuditEvidence::Full);
    let report = env.store.report(stored.id).unwrap();

    assert_eq!(report, stored.report, "the stored report is what was found");
    let rule = report.dat_revision.header_rule.as_ref().unwrap();
    assert_eq!(rule.name, "No-Intro_NES.xml");
    assert!(rule.rule.is_some());
    let zelda = report
        .entries
        .iter()
        .find(|e| e.path == "Zelda (USA).nes")
        .unwrap();
    assert_eq!(zelda.finding, Finding::Matched);
    assert_eq!(zelda.evidence.as_ref().unwrap().header.unwrap().bytes, 16);
    let mario = report
        .games
        .iter()
        .find(|g| g.name == "Mario (USA)")
        .unwrap();
    assert_eq!(mario.warnings, vec![GameWarning::HeaderMissing]);
    assert_eq!(report.summary.warnings[&GameWarning::HeaderMissing], 1);

    let summary = env.store.audit(stored.id).unwrap();
    assert_eq!(summary.warnings[&GameWarning::HeaderMissing], 1);
    assert_eq!(
        summary.header_rule.as_ref(),
        report.dat_revision.header_rule.as_ref()
    );
    let (games, _) = env.store.games(stored.id, None, 10, None).unwrap();
    assert!(
        games
            .iter()
            .any(|g| g.game.warnings == vec![GameWarning::HeaderMissing])
    );
}

#[test]
fn a_reused_read_keeps_its_header() {
    let env = Env::new();
    let zelda = noise(1, 64);
    env.add(
        "No-Intro_NES.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)])],
    );
    write_file(
        &env.set_dir().join("Zelda (USA).nes"),
        &[ines(), zelda].concat(),
    );
    env.audit(AuditEvidence::Incremental);

    let second = env.audit(AuditEvidence::Incremental).report;

    let evidence = second.entries[0].evidence.as_ref().unwrap();
    assert_eq!(second.entries[0].finding, Finding::Matched);
    assert_eq!(evidence.reused, Some(Reuse::UnchangedFileHeuristic));
    assert_eq!(evidence.header.unwrap().bytes, 16);
}

#[test]
fn an_unknown_header_rule_is_kept_by_name() {
    let env = Env::new();
    env.add(
        "No-Intro_SNES.xml",
        &[("Game", vec![rom("Game.sfc", b"game")])],
    );
    write_file(&env.set_dir().join("Game.sfc"), b"game");

    let stored = env.audit(AuditEvidence::Full);

    let rule = env
        .store
        .report(stored.id)
        .unwrap()
        .dat_revision
        .header_rule
        .unwrap();
    assert_eq!(rule.name, "No-Intro_SNES.xml");
    assert_eq!(rule.rule, None);
}

/// The payload of the sole member of the ZIP archive at `path`.
fn sole_member(path: &std::path::Path) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    assert_eq!(archive.len(), 1);
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_index(0).unwrap(), &mut payload).unwrap();
    payload
}

#[test]
fn a_repair_writes_a_headered_dump_with_its_header_and_its_audit_matches_it() {
    let env = Env::new();
    let zelda = noise(1, 64);
    let mario = noise(2, 64);
    env.add(
        "No-Intro_NES.xml",
        &[
            ("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)]),
            ("Mario (USA)", vec![rom("Mario (USA).nes", &mario)]),
        ],
    );
    let headered_zelda = [ines(), zelda].concat();
    let headered_mario = [ines(), mario].concat();
    write_file(
        &env.set_dir().join("Zelda (Anthology).nes"),
        &headered_zelda,
    );
    write_zip(
        &env.set_dir().join("Mario (USA).zip"),
        &[("Mario (USA).nes", &headered_mario)],
    );
    env.audit(AuditEvidence::Full);

    let plan = env
        .store
        .plan_repair("Test - System", &PlanRepair::default())
        .unwrap();
    let kinds: Vec<_> = plan
        .content
        .items
        .iter()
        .map(|i| (&i.key[..], i.kind))
        .collect();
    assert!(
        kinds.contains(&("game:Zelda (USA)", RepairKind::Rename)),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&("game:Mario (USA)", RepairKind::Repack)),
        "{kinds:?}"
    );
    let approved = env
        .store
        .approve_repair(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = env
        .store
        .run_repair_job(approved.job.unwrap(), None)
        .unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    let done = env.store.repair_plan(plan.id).unwrap();
    assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);

    assert_eq!(
        sole_member(&env.set_dir().join("Zelda (USA).zip")),
        headered_zelda
    );
    assert_eq!(
        sole_member(&env.set_dir().join("Mario (USA).zip")),
        headered_mario
    );
    let report = env.audit(AuditEvidence::Full).report;
    for game in ["Zelda (USA)", "Mario (USA)"] {
        let entry = report
            .entries
            .iter()
            .find(|e| e.path == format!("{game}.zip"))
            .unwrap();
        assert_eq!(entry.finding, Finding::Matched, "{game}");
        assert_eq!(entry.evidence.as_ref().unwrap().header.unwrap().bytes, 16);
    }
}

/// A Selection of the DAT Revision `sha256` with each of `titles` the winner of its group.
fn selection(sha256: &str, titles: &[&str]) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: "Test - System".into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups: titles
            .iter()
            .map(|title| SelectionGroup {
                dependencies: Vec::new(),
                group: title.to_lowercase(),
                unresolved: false,
                kept: vec![KeptGame {
                    title: (*title).into(),
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
            })
            .collect(),
        stale_choices: Default::default(),
    }
}

/// Builds `titles` of the audited Source Set into the new Library Folder `path` as `format`, with `profile`. Returns
/// the folder's id.
fn build_with(
    env: &Env,
    profile: &SelectionProfile,
    titles: &[&str],
    path: &str,
    format: StorageFormat,
) -> i64 {
    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.systems.insert(
        SET.into(),
        SystemStorage {
            format: Some(format),
            ..Default::default()
        },
    );
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    env.store
        .save_profile("Mine", profile, None, "Operator", &[], WORKERS)
        .unwrap();
    env.store
        .enqueue_selection("Test - System", None, "Mine", None, "Operator", WORKERS)
        .unwrap();
    let job = env.store.claim_job().unwrap().unwrap();
    assert_eq!(job.kind, JobKind::Selection);
    let sha256 = env.store.history("Test - System").unwrap()[0]
        .dat_revision_sha256
        .clone();
    let id = env
        .store
        .record_selection(job.id, &selection(&sha256, titles))
        .unwrap()
        .selection
        .unwrap();
    let folder = env
        .store
        .add_library_folder(&AddLibraryFolder {
            root: "play".into(),
            path: Some(path.into()),
            template: None,
            source_set: "Test - System".into(),
            profile: "Mine".into(),
        })
        .unwrap();
    let plan = env.store.plan_build(folder.id, id).unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let job = env
        .store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = env.store.run_build_job(job.id, None).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    env.settle();
    folder.id
}

fn build(env: &Env, titles: &[&str], path: &str, format: StorageFormat) -> i64 {
    build_with(env, &SelectionProfile::default(), titles, path, format)
}

fn check(env: &Env, folder: i64) -> FolderCheck {
    let job = env.store.enqueue_check(folder, "Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    let done = env.store.run_check_job(job.id, None).unwrap();
    assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
    env.store.latest_check(folder).unwrap().unwrap()
}

#[test]
fn a_build_writes_headered_dumps_with_their_headers_and_a_check_knows_them_by_their_dat_checksums()
{
    for format in [StorageFormat::TorrentZip, StorageFormat::Loose] {
        let env = Env::new();
        let zelda = noise(1, 64);
        let mario = noise(2, 64);
        env.add(
            "No-Intro_NES.xml",
            &[
                ("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)]),
                ("Mario (USA)", vec![rom("Mario (USA).nes", &mario)]),
            ],
        );
        let headered_zelda = [ines(), zelda].concat();
        let headered_mario = [ines(), mario].concat();
        write_file(&env.set_dir().join("Zelda (USA).nes"), &headered_zelda);
        write_zip(
            &env.set_dir().join("Mario (USA).zip"),
            &[("Mario (USA).nes", &headered_mario)],
        );
        env.audit(AuditEvidence::Full);

        let folder = build(&env, &["Zelda (USA)", "Mario (USA)"], "nes", format);

        let play = env.tmp.path().join("roots/play/nes");
        for (game, headered) in [
            ("Zelda (USA)", &headered_zelda),
            ("Mario (USA)", &headered_mario),
        ] {
            let built = match format {
                StorageFormat::Loose => std::fs::read(play.join(format!("{game}.nes"))).unwrap(),
                _ => sole_member(&play.join(format!("{game}.zip"))),
            };
            assert_eq!(&built, headered, "{game} as {format:?}");
        }
        assert!(check(&env, folder).drift.is_empty(), "{format:?}");

        let (from, to) = match format {
            StorageFormat::Loose => ("Zelda (USA).nes", "zelda.nes"),
            _ => ("Zelda (USA).zip", "zelda.zip"),
        };
        std::fs::rename(play.join(from), play.join(to)).unwrap();
        let drift = check(&env, folder).drift;
        let renamed = drift.iter().find(|d| d.name == to).unwrap();
        assert_eq!(
            renamed.game.as_deref(),
            Some("Zelda (USA)"),
            "{format:?}: {drift:#?}"
        );
    }
}

#[test]
fn a_check_knows_a_recompressed_headered_addition_by_the_whole_payload_it_was_added_with() {
    let env = Env::new();
    let zelda = noise(1, 64);
    env.add(
        "No-Intro_NES.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)])],
    );
    write_file(&env.set_dir().join("Zelda (USA).nes"), &zelda);
    env.audit(AuditEvidence::Full);
    let hack = [ines(), noise(3, 64)].concat();
    env.add_addition("My Hack.nes", &hack, "My Hack");
    let mut profile = SelectionProfile::default();
    profile.systems.insert(
        "Test - System".into(),
        SystemSettings {
            additions: true,
            ..SystemSettings::default()
        },
    );
    let folder = build_with(
        &env,
        &profile,
        &["Zelda (USA)"],
        "nes",
        StorageFormat::TorrentZip,
    );
    let built = env.tmp.path().join("roots/play/nes/My Hack.zip");
    assert_eq!(sole_member(&built), hack);
    assert!(check(&env, folder).drift.is_empty());

    std::fs::remove_file(&built).unwrap();
    write_zip(&built, &[("My Hack.nes", &hack)]);
    let check = check(&env, folder);

    let file = check
        .files
        .iter()
        .find(|f| f.target == "My Hack.zip")
        .unwrap();
    assert_eq!(file.outcome, FileOutcome::Repacked, "{check:#?}");
    assert!(check.drift.is_empty(), "{check:#?}");
}

#[test]
fn headered_drift_added_as_an_addition_by_a_cleanup_is_added_whole() {
    let env = Env::new();
    let zelda = noise(1, 64);
    env.add(
        "No-Intro_NES.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)])],
    );
    write_file(&env.set_dir().join("Zelda (USA).nes"), &zelda);
    env.audit(AuditEvidence::Full);
    let folder = build(&env, &["Zelda (USA)"], "nes", StorageFormat::TorrentZip);
    let hack = [ines(), noise(3, 64)].concat();
    write_file(&env.tmp.path().join("roots/play/nes/My Hack.nes"), &hack);
    assert!(
        check(&env, folder)
            .drift
            .iter()
            .any(|d| d.name == "My Hack.nes")
    );

    let plan = env
        .store
        .plan_cleanup(
            folder,
            &PlanCleanup {
                entries: BTreeMap::from([("My Hack.nes".to_owned(), CleanupAction::AddAddition)]),
                additions: BTreeMap::from([(
                    "My Hack.nes".to_owned(),
                    NewAddition {
                        system: SET.into(),
                        name: "My Hack".into(),
                        parent: None,
                    },
                )]),
                ..PlanCleanup::default()
            },
        )
        .unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let job = env
        .store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap().unwrap();
    let ended = env.store.run_build_job(job.id, None).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    env.settle();

    let sets = env.store.additions_sets().unwrap();
    let addition = &sets[0].additions[0];
    assert_eq!(addition.state, AdditionState::Active, "{addition:#?}");
    assert_eq!(addition.rom.as_ref().unwrap().size, hack.len() as u64);
    assert_eq!(
        sole_member(
            &env.tmp
                .path()
                .join("roots/additions/Test - System")
                .join(&addition.file_name)
        ),
        hack
    );
}
