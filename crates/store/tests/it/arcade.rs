//! MAME Source Sets and the arcade selector (ADR 0027): a software list is audited but never selected or built, a
//! ListXML Source Set takes no Coverage, and a Selection and an Audit keep their Dependencies.

use title_select_api::roots::RootSpec;
use title_select_api::build::{AddLibraryFolder, LibraryFolder};
use title_select_api::selection::{
    ARCADE_COVERAGE_REFUSAL, Dependency, Role, Rule, SOFTWARE_LIST_REFUSAL, Selection,
    SelectionGroup, SelectionPins, SelectionProfile, SelectionSummary, Winner,
};
use title_select_engine::{AuditEvidence, GameStatus};
use title_select_store::{Error, Store, Workers};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

const PROFILE: &str = "Mine";

/// A store with a read-only Root `sets` holding the folder `mame` and a writable Root `play` for Library Folders.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Env {
        Env::with(&[])
    }

    /// As [`Env::new`], but `sets` lets repairs write the folder `mame`.
    fn writable() -> Env {
        Env::with(&["mame"])
    }

    /// With `sets` writable in `writable_paths`.
    fn with(writable_paths: &[&str]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("sets");
        let play = tmp.path().join("play");
        std::fs::create_dir_all(sets.join("mame")).unwrap();
        std::fs::create_dir_all(&play).unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", &sets).writing(writable_paths),
                RootSpec::new("play", &play).writable(),
            ],
        );
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
        Env { tmp, store }
    }

    /// Imports `dat` and registers the Source Set `mame` for it.
    fn add(&self, dat: &[u8]) -> String {
        let (revision, _) = self.store.import_dat_bytes(dat, "mame.xml", None).unwrap();
        self.store
            .add_source_set("sets", "mame", &revision.dat)
            .unwrap();
        revision.sha256
    }

    fn folder(&self) -> Result<LibraryFolder, Error> {
        self.store.add_library_folder(&AddLibraryFolder {
                root: "play".into(),
                path: Some("mame".into()),
                template: None,
                source_set: "mame".into(),
                profile: PROFILE.into(),
            },
        )
    }
}

const SOFTWARE_LIST: &str = r#"<?xml version="1.0"?>
<softwarelist name="acme_cart" description="Acme cartridges">
  <software name="game"><description>Game</description>
    <part name="cart" interface="cart"><dataarea name="rom" size="4">
      <rom name="game.bin" size="4" crc="d87f7e0c"/>
    </dataarea></part>
  </software>
</softwarelist>
"#;

const LISTXML: &str = r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="acmebios" isbios="yes"><description>Acme BIOS</description></machine>
  <machine name="pong" romof="acmebios"><description>Pong (World)</description>
    <rom name="pong.bin" size="4" crc="d87f7e0c"/></machine>
</mame>
"#;

fn refused(result: Result<impl std::fmt::Debug, Error>) -> String {
    match result {
        Err(Error::Refused(message)) => message,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn a_software_list_is_audited_but_never_selected_or_built() {
    let env = Env::new();
    env.add(SOFTWARE_LIST.as_bytes());

    env.store
        .audit_source_set("mame", None, AuditEvidence::Incremental)
        .unwrap();
    let selection = env
        .store
        .enqueue_selection("mame", None, PROFILE, None, "Operator", WORKERS);
    assert_eq!(refused(selection), SOFTWARE_LIST_REFUSAL);
    assert_eq!(refused(env.folder()), SOFTWARE_LIST_REFUSAL);
}

#[test]
fn a_software_lists_source_set_follows_the_format_the_storage_policy_gives_source_sets() {
    let env = Env::writable();
    env.add(SOFTWARE_LIST.as_bytes());
    let zip = std::fs::File::create_new(env.tmp.path().join("sets/mame/game.zip")).unwrap();
    let mut writer = title_select_engine::torrentzip::Writer::new(zip).unwrap();
    writer.add("game.bin", 4, &mut &b"test"[..]).unwrap();
    writer.finish().unwrap();
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = title_select_api::storage::StorageFormat::Loose;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    env.store
        .audit_source_set("mame", None, AuditEvidence::Full)
        .unwrap();

    let plan = env
        .store
        .plan_repair("mame",
            &title_select_api::repair::PlanRepair::default(),
        )
        .unwrap();
    let kinds: Vec<_> = plan
        .content
        .items
        .iter()
        .map(|i| (i.kind, i.summary.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [(
            title_select_api::repair::RepairKind::Convert,
            "convert game.zip to game.bin, stored loose"
        )]
    );
}

#[test]
fn a_stored_audit_keeps_each_games_dependencies_with_their_game_statuses() {
    let env = Env::new();
    env.add(LISTXML.as_bytes());

    let audit = env
        .store
        .audit_source_set("mame", None, AuditEvidence::Full)
        .unwrap();

    let (games, _) = env.store.games(audit.id, None, 10, None).unwrap();
    let pong = games.iter().find(|g| g.game.name == "pong").unwrap();
    assert_eq!(pong.game.status, GameStatus::Missing);
    assert_eq!(
        pong.game.dependencies,
        [title_select_engine::Dependency {
            game: "acmebios".into(),
            status: GameStatus::NothingToCheck,
        }]
    );
    assert_eq!(
        env.store.report(audit.id).unwrap().games,
        audit.report.games
    );
}

#[test]
fn a_listxml_source_set_takes_no_coverage() {
    let env = Env::new();
    let (revision, _) = env
        .store
        .import_dat_bytes(LISTXML.as_bytes(), "mame.xml", None)
        .unwrap();
    let usa = ["USA".to_owned()];
    let added =
        env.store
            .add_source_set_covering("sets", "mame", &revision.dat, Some(&usa));
    assert_eq!(refused(added), ARCADE_COVERAGE_REFUSAL);

    env.store
        .add_source_set("sets", "mame", &revision.dat)
        .unwrap();
    assert_eq!(
        refused(env.store.set_coverage("mame", Some(&usa))),
        ARCADE_COVERAGE_REFUSAL
    );
    env.store.set_coverage("mame", None).unwrap();
    env.folder().unwrap();
}

#[test]
fn a_selection_keeps_its_dependencies_with_the_winners_that_need_them() {
    let env = Env::new();
    let sha256 = env.add(LISTXML.as_bytes());
    let job = env
        .store
        .enqueue_selection("mame", None, PROFILE, None, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap();
    let rule = |stage: &str, message: &str| Rule {
        stage: stage.into(),
        reference: None,
        message: message.into(),
    };
    let group = |name: &str| SelectionGroup {
        group: name.into(),
        unresolved: false,
        winners: Vec::new(),
        alternatives: Vec::new(),
        exclusions: Vec::new(),
        dependencies: Vec::new(),
        choice: None,
        drift: false,
    };
    let selection = Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256,
        system: "MAME".into(),
        system_settings: false,
        summary: SelectionSummary {
            groups: 2,
            winners: 1,
            dependencies: 1,
            ..SelectionSummary::default()
        },
        coverage_drift: Default::default(),
        groups: vec![
            SelectionGroup {
                dependencies: vec![Dependency {
                    title: "acmebios".into(),
                    needed_by: vec!["pong".into()],
                    rule: rule("dependency", "needed by pong"),
                }],
                ..group("acmebios")
            },
            SelectionGroup {
                winners: vec![Winner {
                    title: "pong".into(),
                    rule: rule(
                        "election",
                        "the only machine its filters left in its family",
                    ),
                    unresolved: false,
                }],
                ..group("pong")
            },
        ],
        stale_choices: Default::default(),
    };
    let id = env
        .store
        .record_selection(job.id, &selection)
        .unwrap()
        .selection
        .unwrap();

    assert_eq!(env.store.selection(id).unwrap(), selection);
    let (rows, _) = env
        .store
        .selection_releases(id, None, 10, Some(Role::Dependency), None)
        .unwrap();
    let rows: Vec<(&str, &[String])> = rows
        .iter()
        .map(|r| (r.release.title.as_str(), r.release.needed_by.as_slice()))
        .collect();
    assert_eq!(rows, [("acmebios", ["pong".to_owned()].as_slice())]);
}
