//! Coverage (ADR 0021) as Fixdats, Repair proposals and ingest assessment read it: Games outside a Source Set's
//! Coverage are never filled or listed as wanted, but present ones are still renamed and repacked. From a synthetic
//! DAT, Source Set and Ingest Folder; regions are read from Game names by the Selection engine.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};

use common::*;
use title_select_api::AuditEvidence;
use title_select_api::ingest::{Need, Proposal};
use title_select_api::repair::{PlanRepair, RepairKind, RepairPlan};
use title_select_engine::dat::Dat;
use title_select_store::config::Config;
use title_select_store::{Error, FixdatScope, Store};

const SET: &str = "Alpha";

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
        let regions = title_select_selection::Engine::locate(None)
            .and_then(|engine| engine.regions())
            .expect("the clone lists tools/clone-lists/fetch unpacks");
        store.set_regions(move |game| regions.of(game));
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
        let dat = write_dat(&tmp.path().join("dats"), &games());
        store.import_dat(&dat, None).unwrap();
        store
            .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
        store.register_ingest_folder(&config, "in").unwrap();
        let env = Env { tmp, store, config };
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
        env.store
            .audit_source_set(&env.config, SET, None, AuditEvidence::Full)
            .unwrap();
        env
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
