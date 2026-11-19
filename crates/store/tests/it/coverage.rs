//! Coverage (ADR 0021) as Fixdats, Repair proposals and ingest assessment read it: Games outside a Source Set's
//! Coverage are never filled or listed as wanted, but present ones are still renamed and repacked. From a synthetic
//! DAT, Source Set and Ingest Folder; regions are read from Game names by the Selection engine.

use std::path::{Path, PathBuf};

use crate::common::*;
use title_select_api::ingest::{ItemState, Need, Proposal};
use title_select_api::repair::{PlanRepair, RepairKind, RepairPlan};
use title_select_api::{AuditEvidence, GameStatus, JobState, Workers};
use title_select_engine::dat::Dat;
use title_select_store::config::Config;
use title_select_store::{Error, FixdatScope, Store};

const SET: &str = "Alpha";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// Europe and World Games are covered by a Europe Coverage, a `(USA, Europe)` Game too; USA and Japan Games are not.
fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (Europe)", vec![rom("Pong (Europe).a26", b"pong-eu")]),
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong-us")]),
        (
            "Yars (USA, Europe)",
            vec![rom("Yars (USA, Europe).a26", b"yars")],
        ),
        ("Ball (World)", vec![rom("Ball (World).a26", b"ball")]),
        ("Combat (Japan)", vec![rom("Combat (Japan).a26", b"combat")]),
        (
            "Adventure (Europe)",
            vec![
                rom("Adventure (Europe).a26", b"adventure-eu"),
                rom("Adventure (Europe).txt", b"manual"),
            ],
        ),
        (
            "Adventure (USA)",
            vec![
                rom("Adventure (USA).a26", b"adventure-us"),
                rom("Adventure (USA).txt", b"manual"),
            ],
        ),
    ]
}

/// The Source Set Alpha in a Root that lets a repair write to it, holding: Ball, Complete; Combat (Japan), Misnamed;
/// Adventure (Europe) in a ZIP that holds its manual under an old name, so it counts towards both Adventures;
/// Adventure (USA), Incomplete, its manual Available Elsewhere in Adventure (Europe)'s ZIP. Pong (Europe), Pong (USA) and Yars are Missing. `in` is an Ingest Folder that settles at once.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let env = Env::with(&games());
        env.put("Ball (World).a26", b"ball");
        env.put("Combat (Anthology).a26", b"combat");
        write_zip(
            &env.set().join("Adventure (Europe).zip"),
            &[
                ("Adventure (Europe).a26", b"adventure-eu"),
                ("Adventure (Old).txt", b"manual"),
            ],
        );
        write_zip(
            &env.set().join("Adventure (USA).zip"),
            &[("Adventure (USA).a26", b"adventure-us")],
        );
        env.audit();
        env
    }

    /// An empty Source Set Alpha of a DAT of `games`, and the Ingest Folder `in`.
    fn with(games: &[(&'static str, Vec<RomSpec>)]) -> Env {
        Env::reading(games, true)
    }

    /// As [`Env::with`], with the store reading Games' regions only when `regions`.
    fn reading(games: &[(&'static str, Vec<RomSpec>)], regions: bool) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/Alpha", "in", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"Alpha\"]\n\n\
             [roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
            tmp.path().join("roots/sets").display().to_string(),
            tmp.path().join("in").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        if regions {
            let regions = title_select_selection::Engine::locate(None)
                .and_then(|engine| engine.regions())
                .expect("the clone lists tools/clone-lists/fetch unpacks");
            store.set_regions(move |game| regions.of(game));
        }
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
        let dat = write_dat(&tmp.path().join("dats"), games);
        store.import_dat(&dat, None).unwrap();
        store
            .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
        store.register_ingest_folder(&config, "in").unwrap();
        Env { tmp, store, config }
    }

    fn audit(&self) {
        self.store
            .audit_source_set(&self.config, SET, None, AuditEvidence::Full)
            .unwrap();
    }

    fn status(&self, game: &str) -> GameStatus {
        let latest = &self.store.history(SET).unwrap()[0];
        let report = self.store.report(latest.id).unwrap();
        report.games.iter().find(|g| g.name == game).unwrap().status
    }

    /// Approves `plan`, runs its Job and the audit it queues.
    fn approve(&self, plan: &RepairPlan) {
        let approved = self
            .store
            .approve_repair(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let job = approved.job.unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job);
        let ended = self.store.run_repair_job(&self.config, job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.store.fulfil_repair_audits().unwrap();
        let done = self.store.repair_plan(plan.id).unwrap();
        let audit = done.audit_job.unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, audit);
        self.store.run_audit_job(&self.config, audit, None).unwrap();
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("roots/sets").join(SET)
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        write_file(&self.set().join(rel), bytes);
    }

    fn cover(&self, regions: &[&str]) {
        let regions: Vec<String> = regions.iter().map(|r| (*r).to_owned()).collect();
        self.store.set_coverage(SET, Some(&regions)).unwrap();
    }

    /// Puts `name` in the Ingest Folder and scans it twice, so it settles and is assessed.
    fn ingest(&self, name: &str, bytes: &[u8]) {
        write_file(&self.tmp.path().join("in").join(name), bytes);
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            self.store
                .run_ingest_scan_job(&self.config, job.id)
                .unwrap();
        }
    }

    fn plan(&self) -> RepairPlan {
        self.store
            .plan_repair(&self.config, SET, &PlanRepair::default())
            .unwrap()
    }

    fn fixdat_games(&self, scope: Option<FixdatScope>) -> (Vec<String>, String) {
        let export = self.store.fixdat(SET, scope).unwrap();
        let read = Dat::parse(export.content.as_bytes(), Path::new(&export.file_name)).unwrap();
        (read.games, export.content)
    }
}

/// The kind of the item a plan proposes for Game `game`, if it proposes one.
fn proposed(plan: &RepairPlan, game: &str) -> Option<RepairKind> {
    let key = format!("game:{game}");
    plan.content
        .items
        .iter()
        .find(|i| i.key == key)
        .map(|i| i.kind)
}

#[test]
fn without_a_coverage_the_fixdat_lists_every_game_the_source_set_lacks() {
    let env = Env::new();

    let (games, xml) = env.fixdat_games(None);

    assert_eq!(
        games,
        [
            "Pong (Europe)",
            "Pong (USA)",
            "Yars (USA, Europe)",
            "Adventure (USA)"
        ]
    );
    assert!(!xml.contains("coverage"), "{xml}");
}

#[test]
fn with_a_coverage_the_fixdat_lists_only_what_it_covers_by_default_and_names_the_coverage() {
    let env = Env::new();
    env.cover(&["Europe", "UK"]);

    let (games, xml) = env.fixdat_games(None);

    assert_eq!(games, ["Pong (Europe)", "Yars (USA, Europe)"]);
    assert!(
        xml.contains("limited to the games in its coverage: Europe, UK"),
        "{xml}"
    );
    assert_eq!(env.fixdat_games(Some(FixdatScope::Coverage)).0, games);
    assert_eq!(
        env.fixdat_games(Some(FixdatScope::Dat)).0,
        [
            "Pong (Europe)",
            "Pong (USA)",
            "Yars (USA, Europe)",
            "Adventure (USA)"
        ],
        "the whole DAT is still yours to export"
    );
}

#[test]
fn a_coverage_fixdat_of_a_source_set_without_a_coverage_is_refused() {
    let env = Env::new();

    let refused = env.store.fixdat(SET, Some(FixdatScope::Coverage));

    let Err(Error::Refused(why)) = refused else {
        panic!("{refused:?}");
    };
    assert!(why.contains("no coverage"), "{why}");
}

#[test]
fn without_a_coverage_repair_fills_every_game_it_can_and_records_no_coverage() {
    let env = Env::new();
    env.ingest("pong-us.a26", b"pong-us");

    let plan = env.plan();

    assert_eq!(proposed(&plan, "Pong (USA)"), Some(RepairKind::Fill));
    assert!(
        proposed(&plan, "Adventure (USA)").is_some(),
        "filled from the manual available elsewhere: {:#?}",
        plan.content.notes
    );
    assert_eq!(proposed(&plan, "Combat (Japan)"), Some(RepairKind::Rename));
    assert_eq!(plan.content.coverage, None);
}

#[test]
fn repair_proposes_no_fill_for_a_game_outside_the_coverage_but_still_renames_one_present() {
    let env = Env::new();
    env.cover(&["Europe", "UK"]);
    env.ingest("pong-us.a26", b"pong-us");
    env.ingest("pong-eu.a26", b"pong-eu");

    let plan = env.plan();

    assert_eq!(
        proposed(&plan, "Pong (USA)"),
        None,
        "not filled from an ingest folder"
    );
    assert_eq!(
        proposed(&plan, "Adventure (USA)"),
        None,
        "not filled from bytes available elsewhere"
    );
    assert_eq!(proposed(&plan, "Combat (Japan)"), Some(RepairKind::Rename));
    assert_eq!(proposed(&plan, "Pong (Europe)"), Some(RepairKind::Fill));
    assert_eq!(
        plan.content.coverage.as_deref(),
        Some(&["Europe".to_owned(), "UK".to_owned()][..])
    );
}

impl Env {
    /// The assessment of the Ingest Folder item `name`: each Game it matches with what the item does for it, and
    /// what it proposes.
    fn assessed(&self, name: &str) -> (Vec<(String, Need)>, Option<Proposal>) {
        let item = self
            .store
            .ingest_items("in")
            .unwrap()
            .into_iter()
            .find(|i| i.name == name)
            .unwrap();
        let matches = item
            .assessment
            .matches
            .iter()
            .map(|m| (m.game.clone(), m.need))
            .collect();
        (matches, item.assessment.proposal)
    }
}

#[test]
fn without_a_coverage_an_ingest_item_fills_the_missing_game_it_matches() {
    let env = Env::new();
    env.ingest("pong-us.a26", b"pong-us");

    let (matches, proposal) = env.assessed("pong-us.a26");

    assert_eq!(matches, [("Pong (USA)".to_owned(), Need::Missing)]);
    assert_eq!(
        proposal,
        Some(Proposal::SourceSet {
            source_set: SET.into(),
            game: "Pong (USA)".into(),
            need: Need::Missing,
        })
    );
}

#[test]
fn an_ingest_item_matching_only_a_game_outside_the_coverage_is_shown_as_outside_it() {
    let env = Env::new();
    env.cover(&["Europe"]);
    env.ingest("pong-us.a26", b"pong-us");
    env.ingest("pong-eu.a26", b"pong-eu");

    let (matches, proposal) = env.assessed("pong-us.a26");

    assert_eq!(matches, [("Pong (USA)".to_owned(), Need::OutsideCoverage)]);
    assert_eq!(
        proposal,
        Some(Proposal::SourceSet {
            source_set: SET.into(),
            game: "Pong (USA)".into(),
            need: Need::OutsideCoverage,
        })
    );
    assert_eq!(
        env.assessed("pong-eu.a26").0,
        [("Pong (Europe)".to_owned(), Need::Missing)]
    );
}

#[test]
fn a_covered_game_is_built_from_a_rom_an_uncovered_game_shares_and_the_uncovered_games_file_stays()
{
    // Beta's ZIP holds its shared ROM under an old name, so it counts towards Alpha too: Alpha is filled from it.
    let env = Env::with(&[
        ("Alpha (Europe)", vec![rom("Alpha (Europe).bin", b"shared")]),
        (
            "Beta (USA)",
            vec![
                rom("Beta (USA).bin", b"shared"),
                rom("Beta (USA).dat", b"beta-only"),
            ],
        ),
    ]);
    write_zip(&env.set().join("Beta (USA).zip"), &[("Old.bin", b"shared")]);
    env.audit();
    assert_eq!(env.status("Beta (USA)"), GameStatus::Incomplete);
    env.cover(&["Europe"]);

    let plan = env.plan();

    assert!(
        proposed(&plan, "Alpha (Europe)").is_some(),
        "{:#?}",
        plan.content.notes
    );
    assert_eq!(proposed(&plan, "Beta (USA)"), None);
    env.approve(&plan);
    assert_eq!(env.status("Alpha (Europe)"), GameStatus::Complete);
    assert_eq!(env.status("Beta (USA)"), GameStatus::Incomplete);
    assert!(env.set().join("Beta (USA).zip").is_file());
}

#[test]
fn a_repair_plan_made_under_another_coverage_is_refused_on_approval() {
    let env = Env::new();
    env.cover(&["Europe"]);
    let plan = env.plan();
    env.cover(&["Europe", "USA"]);

    let refused = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS);

    let Err(Error::Conflict(why)) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        why.contains("coverage") && why.contains("Plan the repair again"),
        "{why}"
    );
    let cleared = env.plan();
    env.store.set_coverage(SET, None).unwrap();
    assert!(matches!(
        env.store.approve_repair(
            &env.config,
            cleared.id,
            &cleared.digest,
            "Operator",
            WORKERS
        ),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn the_coverage_cannot_change_while_an_approved_repair_has_not_finished() {
    let env = Env::new();
    env.cover(&["Europe"]);
    let plan = env.plan();
    let approved = env
        .store
        .approve_repair(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    let usa = ["USA".to_owned()];
    let refused = env.store.set_coverage(SET, Some(&usa));

    let Err(Error::Conflict(why)) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        why.contains(&format!("repair plan {}", plan.id)) && why.contains("once it has finished"),
        "{why}"
    );
    assert_eq!(
        env.store.source_set(SET).unwrap().coverage.as_deref(),
        Some(&["Europe".to_owned()][..])
    );
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    let ended = env.store.run_repair_job(&env.config, job, None).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    env.store.set_coverage(SET, Some(&usa)).unwrap();
}

#[test]
fn without_regions_to_read_ingest_leaves_a_covered_sets_matches_unresolved_and_still_serves_every_other_set()
 {
    let env = Env::reading(
        &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong-us")])],
        false,
    );
    env.cover(&["Europe"]);
    let other = write_dat_from(
        &env.tmp.path().join("dats"),
        "other.dat",
        "20260101-000000",
        "Redump",
        &[("Kart (USA)", vec![rom("Kart (USA).bin", b"kart")])],
    );
    env.store.import_dat(&other, None).unwrap();
    std::fs::create_dir_all(env.tmp.path().join("roots/sets/Beta")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "Beta", "Redump/Test - System")
        .unwrap();
    let audit = env
        .store
        .audit_source_set(&env.config, "Beta", None, AuditEvidence::Full);
    audit.unwrap();
    write_file(&env.tmp.path().join("in/mystery.bin"), b"mystery");
    write_dat_from(
        &env.tmp.path().join("in"),
        "new.dat",
        "20260202-000000",
        "TOSEC",
        &[("Zork (USA)", vec![rom("Zork (USA).z5", b"zork")])],
    );
    write_file(&env.tmp.path().join("in/kart.bin"), b"kart");

    env.ingest("pong-us.a26", b"pong-us");

    let item = |name: &str| {
        env.store
            .ingest_items("in")
            .unwrap()
            .into_iter()
            .find(|i| i.name == name)
            .unwrap()
    };
    let pong = item("pong-us.a26");
    assert_eq!(pong.state, ItemState::Assessed, "{pong:?}");
    assert_eq!(
        pong.assessment.proposal,
        Some(Proposal::SourceSet {
            source_set: SET.into(),
            game: "Pong (USA)".into(),
            need: Need::CoverageUnavailable,
        }),
        "neither an addition nor a fill"
    );
    assert_eq!(
        item("kart.bin").assessment.proposal,
        Some(Proposal::SourceSet {
            source_set: "Beta".into(),
            game: "Kart (USA)".into(),
            need: Need::Missing,
        })
    );
    assert_eq!(
        item("new.dat").assessment.proposal,
        Some(Proposal::DatImport)
    );
    assert!(matches!(
        item("mystery.bin").assessment.proposal,
        Some(Proposal::Addition { .. })
    ));
    env.audit();
    assert!(matches!(
        env.store
            .plan_repair(&env.config, SET, &PlanRepair::default()),
        Err(Error::RegionsUnavailable(_))
    ));
}
