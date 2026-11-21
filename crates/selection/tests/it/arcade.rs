//! The arcade selector (ADR 0027): an Arcade DAT is selected by its filters and family election, and its winners'
//! Dependencies are listed once (ADR 0025). The goldens are on synthetic ListXML.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::Digest;
use title_select_api::selection::{
    CategorySource, ChoiceStatus, SOFTWARE_LIST_REFUSAL, Selection, SelectionGroup,
    SelectionProfile, SystemSettings,
};
use title_select_api::support::SupportFileKind;
use title_select_api::{DatSystem, SourceSetSystem};
use title_select_selection::arcade::{ArcadeSettings, ElectionStep, Orientation, Range, Variant};
use title_select_selection::{Engine, Outcome};

use crate::support;

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// The engine's ListXML family fixture: a BIOS, a device chain, a parent and clone, and a mechanical machine.
fn family_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/listxml-family.xml")
}

fn engine() -> Engine {
    support::fixture_engine()
}

/// A Profile whose global `arcade` block is `settings`.
fn arcade_profile(settings: &ArcadeSettings) -> SelectionProfile {
    SelectionProfile {
        arcade: Some(settings.clone()),
        ..SelectionProfile::default()
    }
}

fn run(dat: &Path, settings: &ArcadeSettings) -> Outcome {
    run_with(dat, &SelectionProfile::default(), settings)
}

/// Selects from `dat` with `profile`, its global `arcade` block set to `settings`.
fn run_with(dat: &Path, profile: &SelectionProfile, settings: &ArcadeSettings) -> Outcome {
    let profile = SelectionProfile {
        arcade: Some(settings.clone()),
        ..profile.clone()
    };
    engine().run(dat, &profile, &support::options()).unwrap()
}

/// Checks the Selection, less its pins, and the trace against the goldens `<name>.selection.json` and
/// `<name>.trace.jsonl`.
fn golden(name: &str, outcome: &Outcome) {
    let mut selection = outcome.selection.clone();
    selection.pins = Default::default();
    let mut json = serde_json::to_string_pretty(&selection).unwrap();
    json.push('\n');
    support::golden(&format!("{name}.selection.json"), &json);
    support::golden(&format!("{name}.trace.jsonl"), &outcome.trace);
}

fn group<'a>(selection: &'a Selection, name: &str) -> &'a SelectionGroup {
    selection
        .groups
        .iter()
        .find(|g| g.group == name)
        .unwrap_or_else(|| panic!("no group {name}"))
}

fn winners(selection: &Selection) -> BTreeSet<String> {
    selection
        .groups
        .iter()
        .flat_map(|g| g.winners.iter().map(|w| w.title.clone()))
        .collect()
}

/// Each excluded machine with its rule's stage.
fn exclusions(selection: &Selection) -> BTreeMap<String, String> {
    selection
        .groups
        .iter()
        .flat_map(|g| g.exclusions.iter())
        .map(|e| (e.title.clone(), e.rule.stage.clone()))
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn the_defaults_elect_one_machine_per_family_and_list_each_dependency_once() {
    let outcome = run(&data("arcade.xml"), &ArcadeSettings::default());
    golden("arcade-defaults", &outcome);
    let selection = &outcome.selection;

    assert_eq!(selection.system, "MAME");
    assert_eq!(
        winners(selection),
        set(&[
            "fighteru", "mystery", "proto", "racera", "sailor", "shooterw", "worldh"
        ])
    );
    assert_eq!(
        exclusions(selection),
        BTreeMap::from(
            [
                ("calcul8", "category"),
                ("crane", "hardware"),
                ("homepc", "hardware"),
                ("jukebox", "hardware"),
                ("mjhouse", "category"),
                ("oldbios", "structural"),
                ("pokerace", "category"),
                ("racer", "quality"),
                ("reels", "category"),
                ("typist", "category"),
            ]
            .map(|(t, s)| (t.to_owned(), s.to_owned()))
        )
    );

    // The region decides the shooter family over its Japanese parent, and its bootleg loses as a bootleg.
    let shooter = group(selection, "shooter");
    assert_eq!(shooter.winners[0].title, "shooterw");
    let alternatives: BTreeMap<&str, &str> = shooter
        .alternatives
        .iter()
        .map(|a| (a.title.as_str(), a.rule.stage.as_str()))
        .collect();
    assert_eq!(
        alternatives,
        BTreeMap::from([
            ("shooter", "election:region"),
            ("shootere", "election:region"),
            ("shooteru", "election:region"),
            ("shooterb", "election:variant"),
        ])
    );
    assert_eq!(
        group(selection, "fighter").alternatives[0].rule.stage,
        "election:status"
    );

    // A BIOS and a device chain that two winners share are listed once each; a BIOS no winner needs is excluded.
    let dependencies: BTreeMap<String, Vec<String>> = selection
        .groups
        .iter()
        .flat_map(|g| g.dependencies.iter())
        .map(|d| (d.title.clone(), d.needed_by.clone()))
        .collect();
    let both = vec!["fighteru".to_owned(), "shooterw".to_owned()];
    assert_eq!(
        dependencies,
        BTreeMap::from([
            ("acmebios".to_owned(), both.clone()),
            ("acmesnd".to_owned(), both.clone()),
            ("z80".to_owned(), both),
        ])
    );
    assert_eq!(selection.summary.dependencies, 3);
    assert!(group(selection, "acmebios").exclusions.is_empty());

    let arcade = selection.pins.arcade.as_ref().unwrap();
    assert_eq!(arcade.build, "0.289 (mame0289)");
    assert_eq!(arcade.category, Some(CategorySource::ListxmlHeuristic));
    assert!(arcade.unavailable.is_empty());
}

#[test]
fn a_good_clone_beats_a_preliminary_parent_in_the_election() {
    let settings = ArcadeSettings {
        minimum_driver_status: None,
        ..ArcadeSettings::default()
    };
    let outcome = run(&data("arcade.xml"), &settings);
    golden("arcade-status-election", &outcome);
    let racer = group(&outcome.selection, "racer");
    assert_eq!(racer.winners[0].title, "racera");
    assert_eq!(racer.alternatives[0].title, "racer");
    assert_eq!(racer.alternatives[0].rule.stage, "election:status");
}

#[test]
fn the_family_fixture_elects_a_clone_with_its_bios_and_devices() {
    let outcome = run(&family_fixture(), &ArcadeSettings::default());
    golden("arcade-family", &outcome);
    let selection = &outcome.selection;
    assert_eq!(winners(selection), set(&["blasterj"]));
    assert_eq!(exclusions(selection)["blaster"], "hardware");
    assert_eq!(exclusions(selection)["crane"], "hardware");
    let dependencies: BTreeSet<String> = selection
        .groups
        .iter()
        .flat_map(|g| g.dependencies.iter().map(|d| d.title.clone()))
        .collect();
    assert_eq!(dependencies, set(&["acmesnd", "arcbios", "z80"]));
}

#[test]
fn offered_filters_drop_what_they_name_and_keep_machines_that_dont_say() {
    let dat = data("arcade.xml");
    let dropped = |settings: ArcadeSettings| exclusions(&run(&dat, &settings).selection);

    let variants = dropped(ArcadeSettings {
        drop_variants: [Variant::Bootleg, Variant::Prototype].into(),
        ..ArcadeSettings::default()
    });
    assert_eq!(variants["shooterb"], "variant");
    assert_eq!(variants["proto"], "variant");

    // mystery gives no year, so the year filter keeps it.
    let years = dropped(ArcadeSettings {
        years: Range {
            min: Some(1993),
            max: None,
        },
        ..ArcadeSettings::default()
    });
    for machine in ["fighter", "fighteru", "shooterw", "shooteru"] {
        assert_eq!(years[machine], "year", "{machine}");
    }
    assert!(!years.contains_key("mystery"));
    assert!(!years.contains_key("shooterb"));
    assert!(!years.contains_key("racera"));

    let players = dropped(ArcadeSettings {
        players: Range {
            min: Some(3),
            max: None,
        },
        ..ArcadeSettings::default()
    });
    assert_eq!(players["shooterw"], "players");
    assert!(!players.contains_key("worldh"));

    let controls = dropped(ArcadeSettings {
        forbid_controls: set(&["lightgun"]),
        ..ArcadeSettings::default()
    });
    assert_eq!(controls["worldh"], "controls");
    let required = dropped(ArcadeSettings {
        require_controls: set(&["paddle"]),
        ..ArcadeSettings::default()
    });
    assert_eq!(required["shooterw"], "controls");
    assert!(!required.contains_key("racera"));

    // mystery's screen gives no rotation, so the orientation filter keeps it.
    let vertical = dropped(ArcadeSettings {
        orientation: Some(Orientation::Vertical),
        ..ArcadeSettings::default()
    });
    assert_eq!(vertical["fighteru"], "orientation");
    assert!(!vertical.contains_key("shooterw"));
    assert!(!vertical.contains_key("mystery"));

    let media = dropped(ArcadeSettings {
        drop_disks: true,
        ..ArcadeSettings::default()
    });
    assert_eq!(media["racera"], "media");
    let samples = dropped(ArcadeSettings {
        drop_samples: true,
        ..ArcadeSettings::default()
    });
    assert_eq!(samples["racera"], "media");

    // Unknown driver status keeps mystery under the quality filter.
    let quality = dropped(ArcadeSettings {
        minimum_driver_status: Some(title_select_engine::dat::DriverStatus::Good),
        ..ArcadeSettings::default()
    });
    assert_eq!(quality["proto"], "quality");
    assert!(!quality.contains_key("mystery"));
}

#[test]
fn a_filter_whose_signal_the_dat_lacks_is_unavailable_and_drops_nothing() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("bare.xml");
    std::fs::write(
        &dat,
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="bare"><description>Bare</description><display type="raster" rotate="0"/></machine>
  <machine name="barej" cloneof="bare"><description>Bare (Japan)</description><display type="raster" rotate="0"/></machine>
</mame>
"#,
    )
    .unwrap();
    let settings = ArcadeSettings {
        years: Range {
            min: Some(1990),
            max: Some(1999),
        },
        ..ArcadeSettings::default()
    };
    let outcome = run(&dat, &settings);
    let arcade = outcome.selection.pins.arcade.clone().unwrap();
    assert!(arcade.unavailable.contains(&"quality".to_owned()));
    assert!(arcade.unavailable.contains(&"year".to_owned()));
    // A filter you left off isn't listed, though the DAT has nothing for it either.
    assert!(!arcade.unavailable.contains(&"players".to_owned()));
    // A machine that names no region ranks after every region you order.
    assert_eq!(winners(&outcome.selection), set(&["barej"]));
    assert!(exclusions(&outcome.selection).is_empty());
}

#[test]
fn include_and_exclude_lists_take_the_override_matchers() {
    let settings = ArcadeSettings {
        exclude: vec![
            "/^shooter[wu]$".into(),
            "|Shooter (Europe)".into(),
            "bootleg".into(),
        ],
        include: vec!["|crane".into()],
        ..ArcadeSettings::default()
    };
    let selection = run(&data("arcade.xml"), &settings).selection;
    let dropped = exclusions(&selection);
    for machine in ["shooterw", "shooteru", "shootere", "shooterb"] {
        assert_eq!(dropped[machine], "list", "{machine}");
    }
    assert_eq!(group(&selection, "shooter").winners[0].title, "shooter");
    assert!(winners(&selection).contains("crane"));
}

#[test]
fn an_invalid_list_pattern_is_refused() {
    let settings = ArcadeSettings {
        exclude: vec!["/(".into()],
        ..ArcadeSettings::default()
    };
    let profile = SelectionProfile {
        arcade: Some(settings),
        ..SelectionProfile::default()
    };
    let error = engine()
        .run(&data("arcade.xml"), &profile, &support::options())
        .unwrap_err();
    assert!(error.to_string().contains("/("), "{error}");
    let checked = engine().check(&profile, &[]).unwrap_err().to_string();
    assert!(
        checked.starts_with("Your global arcade settings: exclude list: /(:"),
        "{checked}"
    );
}

#[test]
fn a_systems_arcade_block_replaces_the_global_one() {
    let mut profile = SelectionProfile {
        arcade: Some(ArcadeSettings {
            exclude: vec!["|fighter".into()],
            ..ArcadeSettings::default()
        }),
        ..SelectionProfile::default()
    };
    let global = engine()
        .run(&data("arcade.xml"), &profile, &support::options())
        .unwrap()
        .selection;
    assert_eq!(exclusions(&global)["fighter"], "list");
    assert!(!global.system_settings);

    profile.systems.insert(
        "MAME".into(),
        SystemSettings {
            arcade: Some(ArcadeSettings {
                exclude: vec!["|shooter".into()],
                ..ArcadeSettings::default()
            }),
            ..SystemSettings::default()
        },
    );
    let own = engine()
        .run(&data("arcade.xml"), &profile, &support::options())
        .unwrap()
        .selection;
    let dropped = exclusions(&own);
    assert_eq!(dropped["shooter"], "list");
    assert!(!dropped.contains_key("fighter"));
    assert!(own.system_settings);
}

#[test]
fn the_election_runs_your_steps_in_your_order() {
    // Parent first: the preliminary parent beats its good clone, which the default order elects.
    let settings = ArcadeSettings {
        minimum_driver_status: None,
        election: vec![ElectionStep::Parent, ElectionStep::Status],
        ..ArcadeSettings::default()
    };
    let racer = run(&data("arcade.xml"), &settings).selection;
    let racer = group(&racer, "racer");
    assert_eq!(racer.winners[0].title, "racer");
    assert_eq!(racer.winners[0].rule.stage, "election:parent");

    // With no steps, the shortname decides.
    let settings = ArcadeSettings {
        minimum_driver_status: None,
        election: Vec::new(),
        ..ArcadeSettings::default()
    };
    let selection = run(&data("arcade.xml"), &settings).selection;
    let racer = group(&selection, "racer");
    assert_eq!(racer.winners[0].title, "racer");
    assert_eq!(racer.winners[0].rule.stage, "election:shortname");

    let twice = SelectionProfile {
        arcade: Some(ArcadeSettings {
            election: vec![ElectionStep::Region, ElectionStep::Region],
            ..ArcadeSettings::default()
        }),
        ..SelectionProfile::default()
    };
    let problem = engine().check(&twice, &[]).unwrap_err().to_string();
    assert_eq!(
        problem,
        "Your global arcade settings: election order: Preferred region is listed twice."
    );
}

#[test]
fn an_arcade_selection_pins_no_clone_lists_or_trace_schema() {
    let selection = run(&data("arcade.xml"), &ArcadeSettings::default()).selection;
    assert!(selection.pins.retool.is_none());
    assert!(selection.pins.title_select_version.is_some());
}

#[test]
fn the_operators_choices_decide_a_family_and_one_gone_from_the_dat_revision_needs_review() {
    let mut profile = SelectionProfile::default();
    let choices = profile.choices.entry("MAME".into()).or_default();
    choices.insert("shooter".into(), set(&["shooteru"]));
    choices.insert("fighter".into(), set(&["fighterj"]));
    let selection = run_with(&data("arcade.xml"), &profile, &ArcadeSettings::default()).selection;

    let shooter = group(&selection, "shooter");
    assert_eq!(shooter.winners[0].title, "shooteru");
    assert_eq!(shooter.winners[0].rule.stage, "choice");
    let acmebios = &group(&selection, "acmebios").dependencies[0];
    assert_eq!(acmebios.needed_by, ["fighteru", "shooteru"]);

    let fighter = group(&selection, "fighter");
    let choice = fighter.choice.as_ref().unwrap();
    assert_eq!(choice.status, ChoiceStatus::Stale);
    assert_eq!(choice.problems, ["fighterj"]);
    assert!(fighter.needs_review());
    assert_eq!(selection.summary.stale, 1);
}

#[test]
fn selection_refuses_a_software_list_and_names_listxml_systems_mame() {
    let work = tempfile::tempdir().unwrap();
    let list = work.path().join("list.xml");
    std::fs::write(
        &list,
        r#"<?xml version="1.0"?>
<softwarelist name="acme_cart" description="Acme cartridges">
  <software name="game"><description>Game</description></software>
</softwarelist>
"#,
    )
    .unwrap();
    let engine = engine();
    let error = engine
        .run(&list, &SelectionProfile::default(), &support::options())
        .unwrap_err();
    assert_eq!(error.to_string(), SOFTWARE_LIST_REFUSAL);

    let listxml = data("arcade.xml");
    assert_eq!(
        engine.systems(&[&listxml, &list]).unwrap(),
        [
            Some(DatSystem {
                name: "MAME".to_owned(),
                arcade: true
            }),
            None
        ]
    );
    // Run dispatches a ListXML to the arcade selector.
    let selection = engine
        .run(&listxml, &SelectionProfile::default(), &support::options())
        .unwrap()
        .selection;
    assert!(selection.pins.arcade.is_some());
}

#[test]
fn coverage_is_refused_for_an_arcade_dat() {
    let options = title_select_selection::RunOptions {
        coverage: Some(vec!["USA".into()]),
        ..support::options()
    };
    let error = engine()
        .run(&data("arcade.xml"), &SelectionProfile::default(), &options)
        .unwrap_err();
    assert!(error.to_string().contains("coverage"), "{error}");
}

#[test]
fn a_machine_that_names_no_language_ties_with_the_first_language() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("gauntlet.xml");
    std::fs::write(
        &dat,
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="gauntlet"><description>Gauntlet (rev 14)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
  <machine name="gauntletj" cloneof="gauntlet"><description>Gauntlet (Japanese, rev 13)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
</mame>
"#,
    )
    .unwrap();
    let selection = run(&dat, &ArcadeSettings::default()).selection;
    let gauntlet = group(&selection, "gauntlet");
    assert_eq!(gauntlet.winners[0].title, "gauntlet");
    assert_eq!(gauntlet.alternatives[0].rule.stage, "election:language");
}

#[test]
fn an_empty_region_order_leaves_region_out_of_the_election() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("outrun.xml");
    std::fs::write(
        &dat,
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="outrun"><description>Out Run</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
  <machine name="outrunj" cloneof="outrun"><description>Out Run (Japan)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
</mame>
"#,
    )
    .unwrap();
    let settings = ArcadeSettings {
        election: vec![ElectionStep::Region, ElectionStep::Parent],
        region_order: Vec::new(),
        ..ArcadeSettings::default()
    };
    let selection = run(&dat, &settings).selection;
    let outrun = group(&selection, "outrun");
    assert_eq!(outrun.winners[0].title, "outrun");
    assert_eq!(outrun.winners[0].rule.stage, "election:parent");
}

#[test]
fn an_empty_language_order_leaves_language_out_of_the_election() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("outrun.xml");
    std::fs::write(
        &dat,
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="outrun"><description>Out Run</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
  <machine name="outrunj" cloneof="outrun"><description>Out Run (Japanese)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
</mame>
"#,
    )
    .unwrap();
    let settings = ArcadeSettings {
        election: vec![ElectionStep::Language, ElectionStep::Parent],
        language_order: Vec::new(),
        ..ArcadeSettings::default()
    };
    let selection = run(&dat, &settings).selection;
    let outrun = group(&selection, "outrun");
    assert_eq!(outrun.winners[0].title, "outrun");
    assert_eq!(outrun.winners[0].rule.stage, "election:parent");
}

#[test]
fn a_newer_six_digit_date_beats_an_older_eight_digit_one_in_the_election() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("fighter.xml");
    std::fs::write(
        &dat,
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="fighter"><description>Fighter (World 900101)</description>
    <display type="raster" rotate="0"/><driver status="preliminary"/></machine>
  <machine name="fighterb" cloneof="fighter"><description>Fighter (World 19900101)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
  <machine name="fighterc" cloneof="fighter"><description>Fighter (World 990101)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
</mame>
"#,
    )
    .unwrap();
    let selection = run(&dat, &ArcadeSettings::default()).selection;
    let fighter = group(&selection, "fighter");
    assert_eq!(fighter.winners[0].title, "fighterc");
    assert_eq!(fighter.alternatives[0].title, "fighterb");
    assert_eq!(fighter.alternatives[0].rule.stage, "election:revision");
}

/// A comment longer than any prefix a sniff might read.
fn long_preamble() -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<!-- {} -->\n",
        "a long preamble ".repeat(1 << 17)
    )
}

#[test]
fn a_listxml_behind_a_long_preamble_goes_to_the_arcade_selector() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("listxml.xml");
    let listxml = std::fs::read_to_string(data("arcade.xml")).unwrap();
    let body = listxml.split_once("?>").map_or(&*listxml, |(_, body)| body);
    std::fs::write(&dat, long_preamble() + body).unwrap();
    assert!(std::fs::metadata(&dat).unwrap().len() > 1 << 21);
    let engine = engine();
    assert_eq!(
        engine.systems(&[&dat]).unwrap()[0]
            .as_ref()
            .map(|s| s.name.as_str()),
        Some("MAME")
    );
    let selection = engine
        .run(&dat, &SelectionProfile::default(), &support::options())
        .unwrap()
        .selection;
    assert!(selection.pins.arcade.is_some());
}

#[test]
fn a_software_list_behind_a_long_preamble_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let list = work.path().join("list.xml");
    std::fs::write(
        &list,
        long_preamble()
            + r#"<softwarelist name="acme_cart" description="Acme cartridges">
  <software name="game"><description>Game</description></software>
</softwarelist>
"#,
    )
    .unwrap();
    let engine = engine();
    assert_eq!(engine.systems(&[&list]).unwrap(), [None]);
    let error = engine
        .run(&list, &SelectionProfile::default(), &support::options())
        .unwrap_err();
    assert_eq!(error.to_string(), SOFTWARE_LIST_REFUSAL);
}

#[test]
fn a_clrmamepro_dat_naming_an_xml_root_in_its_text_goes_to_the_retool_port() {
    let original = support::fixtures().join("source/systems/Microsoft - Xbox - BIOS Datfile.dat");
    let text = std::fs::read_to_string(&original).unwrap();
    let engine = engine();
    let expected = engine
        .run(&original, &SelectionProfile::default(), &support::options())
        .unwrap()
        .selection;
    assert!(!winners(&expected).is_empty());
    let work = tempfile::tempdir().unwrap();
    for root in ["<mame>", "<softwarelist>"] {
        let dat = work.path().join("xbox.dat");
        let named = text.replacen(
            "description \"Microsoft - Xbox - BIOS Images (7) (2010-09-13)\"",
            &format!("description \"A {root} guide\""),
            1,
        );
        assert_ne!(named, text);
        std::fs::write(&dat, named).unwrap();
        let selection = engine
            .run(&dat, &SelectionProfile::default(), &support::options())
            .unwrap()
            .selection;
        assert!(selection.pins.arcade.is_none(), "{root}");
        assert_eq!(winners(&selection), winners(&expected), "{root}");
    }
}

#[test]
fn a_number_that_is_no_calendar_date_ranks_as_naming_no_revision() {
    let work = tempfile::tempdir().unwrap();
    let dat = work.path().join("fighter.xml");
    std::fs::write(
        &dat,
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="fighter"><description>Fighter (World)</description>
    <display type="raster" rotate="0"/><driver status="preliminary"/></machine>
  <machine name="fightera" cloneof="fighter"><description>Fighter (World 000000)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
  <machine name="fighterb" cloneof="fighter"><description>Fighter (World 991231)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
  <machine name="fighterc" cloneof="fighter"><description>Fighter (World 991332)</description>
    <display type="raster" rotate="0"/><driver status="good"/></machine>
</mame>
"#,
    )
    .unwrap();
    let selection = run(&dat, &ArcadeSettings::default()).selection;
    let fighter = group(&selection, "fighter");
    assert_eq!(fighter.winners[0].title, "fighterb");
    let alternatives: BTreeMap<_, _> = fighter
        .alternatives
        .iter()
        .map(|a| (a.title.as_str(), a.rule.stage.as_str()))
        .collect();
    assert_eq!(
        alternatives,
        BTreeMap::from([
            ("fightera", "election:revision"),
            ("fighterc", "election:revision"),
        ])
    );
}

/// The synthetic Support Files: catver and languages for MAME 0.289, bestgames for 0.280.
fn support_files(kinds: &[&str]) -> Vec<PathBuf> {
    kinds
        .iter()
        .map(|kind| data(&format!("support/{kind}.ini")))
        .collect()
}

fn run_supported(engine: &Engine, kinds: &[&str], settings: &ArcadeSettings) -> Outcome {
    let options = title_select_selection::RunOptions {
        support_files: support_files(kinds),
        ..support::options()
    };
    engine
        .run(&data("arcade.xml"), &arcade_profile(settings), &options)
        .unwrap()
}

#[test]
fn catver_decides_category_and_the_heuristic_decides_a_machine_it_lacks() {
    let outcome = run_supported(
        &engine(),
        &["catver", "languages", "bestgames"],
        &ArcadeSettings::default(),
    );
    golden("arcade-support", &outcome);
    let selection = &outcome.selection;
    let dropped = exclusions(selection);
    let reason = |title: &str| {
        selection
            .groups
            .iter()
            .flat_map(|g| g.exclusions.iter())
            .find(|e| e.title == title)
            .map(|e| (e.reason.clone(), e.rule.message.clone()))
            .unwrap()
    };
    // catver sees what the heuristic can't: quiz and mature machines.
    assert_eq!(dropped["worldh"], "category");
    assert_eq!(reason("worldh").0, "quiz");
    assert_eq!(reason("proto").0, "mature");
    assert_eq!(reason("mjhouse").0, "tabletop");
    assert_eq!(
        reason("pokerace").1,
        "gambling by catver.ini: Casino / Cards"
    );
    // catver doesn't list calcul8, so the heuristic drops it by its driver's directory; nor mystery, which it keeps.
    assert_eq!(
        reason("calcul8").1,
        "non-arcade by the ListXML heuristic, as catver.ini doesn't list it: its driver is in casio/"
    );
    assert!(winners(selection).contains("mystery"));

    let arcade = selection.pins.arcade.as_ref().unwrap();
    assert_eq!(arcade.category, Some(CategorySource::Catver));
    assert_eq!(arcade.category_fallbacks, 2);
    let pinned: Vec<(SupportFileKind, &str, bool)> = arcade
        .support_files
        .iter()
        .map(|p| (p.kind, p.mame_version.as_str(), p.drift))
        .collect();
    assert_eq!(
        pinned,
        [
            (SupportFileKind::Catver, "0.289", false),
            (SupportFileKind::Languages, "0.289", false),
            (SupportFileKind::Bestgames, "0.280", true),
        ]
    );
    let catver = std::fs::read(data("support/catver.ini")).unwrap();
    assert_eq!(
        arcade.support_files[0].sha256,
        sha2::Sha256::digest(&catver)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert!(
        outcome
            .trace
            .lines()
            .any(|l| l.contains(r#""type":"drift""#) && l.contains("bestgames.ini")),
        "{}",
        outcome.trace
    );
}

#[test]
fn languages_ini_gives_the_language_election_the_languages_of_each_machine_it_lists() {
    let engine = engine();
    let without = run(&data("arcade.xml"), &ArcadeSettings::default()).selection;
    assert_eq!(group(&without, "sailor").winners[0].title, "sailor");

    let with = run_supported(&engine, &["languages"], &ArcadeSettings::default()).selection;
    let sailor = group(&with, "sailor");
    assert_eq!(sailor.winners[0].title, "sailore");
    assert_eq!(sailor.winners[0].rule.stage, "election:language");
    assert_eq!(
        sailor.alternatives[0].rule.message,
        "language Japanese, which ranks below language English"
    );
}

#[test]
fn a_language_only_languages_ini_names_can_lead_the_order_in_any_case() {
    let work = tempfile::tempdir().unwrap();
    let languages = work.path().join("languages.ini");
    std::fs::write(
        &languages,
        ";; languages.ini 0.289 / 01-Jan-26 / MAME 0.289 ;;\n\n[English]\nsailore\n\n[Russian]\nsailor\n",
    )
    .unwrap();
    let profile = arcade_profile(&ArcadeSettings {
        language_order: vec!["russian".into(), "English".into()],
        ..ArcadeSettings::default()
    });
    let engine = engine();
    engine.check(&profile, &[]).unwrap();
    let options = title_select_selection::RunOptions {
        support_files: vec![languages],
        ..support::options()
    };
    let selection = engine
        .run(&data("arcade.xml"), &profile, &options)
        .unwrap()
        .selection;
    let sailor = group(&selection, "sailor");
    assert_eq!(sailor.winners[0].title, "sailor");
    assert_eq!(sailor.winners[0].rule.stage, "election:language");
}

#[test]
fn an_arcade_block_for_a_system_that_isnt_arcade_is_refused() {
    let mut profile = SelectionProfile::default();
    profile.systems.insert(
        "Nintendo - Game Boy".into(),
        SystemSettings {
            arcade: Some(ArcadeSettings::default()),
            ..SystemSettings::default()
        },
    );
    let problem = engine().check(&profile, &[]).unwrap_err().to_string();
    assert_eq!(
        problem,
        "Your Nintendo - Game Boy settings: arcade settings apply only to MAME, whose DATs are arcade DATs. Remove \
         them from Nintendo - Game Boy."
    );
}

#[test]
fn an_arcade_block_for_mame_is_refused_when_its_dats_are_logiqx_dats() {
    let mut profile = SelectionProfile::default();
    profile.systems.insert(
        "MAME".into(),
        SystemSettings {
            arcade: Some(ArcadeSettings::default()),
            ..SystemSettings::default()
        },
    );
    let logiqx = SourceSetSystem {
        source_set: "mame-logiqx".into(),
        system: Some("MAME".into()),
        arcade: false,
    };
    let problem = engine()
        .check(&profile, std::slice::from_ref(&logiqx))
        .unwrap_err()
        .to_string();
    assert_eq!(
        problem,
        "Your MAME settings: your MAME DATs aren't arcade DATs, so arcade settings never apply to them. Remove them \
         from MAME."
    );
    // With no MAME DAT registered yet, or a ListXML among them, the block can apply.
    engine().check(&profile, &[]).unwrap();
    let listxml = SourceSetSystem {
        source_set: "mame".into(),
        arcade: true,
        ..logiqx.clone()
    };
    engine().check(&profile, &[logiqx, listxml]).unwrap();
}

#[test]
fn a_minimum_rating_reads_the_parents_rating_and_keeps_unrated_families_unless_told() {
    let engine = engine();
    let settings = ArcadeSettings {
        minimum_rating: Some(70),
        ..ArcadeSettings::default()
    };
    let outcome = run_supported(&engine, &["bestgames"], &settings);
    golden("arcade-rating", &outcome);
    let selection = &outcome.selection;
    let dropped = exclusions(selection);
    // racera is rated best itself, but its family's parent is rated below 70.
    assert_eq!(dropped["racera"], "rating");
    let racera = &group(selection, "racer").exclusions;
    assert_eq!(
        racera
            .iter()
            .find(|e| e.title == "racera")
            .unwrap()
            .rule
            .message,
        "its family is rated 40–50 (Decent) by progetto-SNAPS, MAME 0.280, below 70"
    );
    assert!(winners(selection).is_superset(&set(&["fighteru", "shooterw", "mystery", "sailor"])));
    assert!(
        outcome
            .trace
            .lines()
            .any(|l| l.contains(r#""group":"fighter""#)
                && l.contains("rated 70–80 (Good) by progetto-SNAPS, MAME 0.280"))
    );

    let strict = run_supported(
        &engine,
        &["bestgames"],
        &ArcadeSettings {
            drop_unrated: true,
            ..settings
        },
    )
    .selection;
    assert_eq!(winners(&strict), set(&["fighteru", "shooterw"]));
    assert_eq!(exclusions(&strict)["mystery"], "rating");

    // Without bestgames the filter is unavailable and drops nothing.
    let unrated = run(
        &data("arcade.xml"),
        &ArcadeSettings {
            minimum_rating: Some(70),
            ..ArcadeSettings::default()
        },
    );
    let arcade = unrated.selection.pins.arcade.as_ref().unwrap();
    assert!(arcade.unavailable.contains(&"rating".to_owned()));
    assert!(winners(&unrated.selection).contains("racera"));
}

#[test]
fn the_include_list_keeps_a_family_the_rating_filter_would_drop_and_the_trace_says_so() {
    let outcome = run_supported(
        &engine(),
        &["bestgames"],
        &ArcadeSettings {
            minimum_rating: Some(70),
            drop_unrated: true,
            include: vec!["|mystery".into(), "|racera".into()],
            ..ArcadeSettings::default()
        },
    );
    assert!(winners(&outcome.selection).is_superset(&set(&["mystery", "racera"])));
    let rating_line = |family: &str| {
        outcome
            .trace
            .lines()
            .find(|l| {
                l.contains(r#""type":"rating""#) && l.contains(&format!(r#""group":"{family}""#))
            })
            .unwrap_or_else(|| panic!("no rating line for {family}: {}", outcome.trace))
    };
    assert!(rating_line("mystery").contains(
        "unrated by progetto-SNAPS, MAME 0.280, which you drop, but your include list keeps mystery"
    ));
    assert!(rating_line("racer").contains(
        "rated 40–50 (Decent) by progetto-SNAPS, MAME 0.280, below 70, but your include list keeps racera"
    ));
}

/// A curated list at the pinned commit, naming `names`.
fn curated_list(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let pinned = title_select_selection::curated_list().unwrap();
    std::fs::write(dir.path().join("commit"), format!("{}\n", pinned.commit)).unwrap();
    std::fs::write(dir.path().join("ROMSET"), names.join("\n") + "\n").unwrap();
    dir
}

#[test]
fn the_curated_list_keeps_only_its_families_and_prefers_the_machine_it_names() {
    // racer names a parent the quality filter drops; nosuch and shooterx aren't in the DAT, and shooterx is never
    // matched to the shooter family by its start.
    let list = curated_list(&["shooteru", "racer", "sailor", "nosuch", "shooterx"]);
    let engine = engine().with_curated_list(list.path());
    let settings = ArcadeSettings {
        curated: true,
        ..ArcadeSettings::default()
    };
    let outcome = run_supported(&engine, &[], &settings);
    // The golden names no commit, so that a new pin doesn't change it.
    let pinned = title_select_selection::curated_list().unwrap();
    let mut masked = outcome.clone();
    for group in &mut masked.selection.groups {
        let rules = group
            .winners
            .iter_mut()
            .map(|w| &mut w.rule)
            .chain(group.alternatives.iter_mut().map(|a| &mut a.rule));
        for rule in rules {
            if let Some(reference) = &mut rule.reference {
                *reference = reference.replace(&pinned.commit, "<commit>");
            }
        }
    }
    golden("arcade-curated", &masked);
    let selection = &outcome.selection;
    assert_eq!(winners(selection), set(&["racera", "sailor", "shooteru"]));
    let shooter = group(selection, "shooter");
    assert_eq!(shooter.winners[0].rule.stage, "election:curated");
    assert_eq!(
        shooter.winners[0].rule.message,
        "named by curated-mame-arcade-romset"
    );
    assert_eq!(
        shooter.winners[0].rule.reference.as_deref(),
        Some(format!("{}/blob/{}/ROMSET", pinned.repository, pinned.commit).as_str())
    );
    assert_eq!(
        group(selection, "sailor").winners[0].rule.stage,
        "election:curated"
    );
    assert_eq!(exclusions(selection)["fighteru"], "curated");
    let curated = selection
        .pins
        .arcade
        .as_ref()
        .unwrap()
        .curated
        .as_ref()
        .unwrap();
    assert_eq!(curated.commit, pinned.commit);
    assert_eq!(curated.unmatched, ["nosuch", "shooterx"]);
    assert_eq!(selection.summary.curated_unmatched, 2);
    // The winners' Dependencies are still listed.
    assert_eq!(
        group(selection, "acmebios").dependencies[0].needed_by,
        ["shooteru"]
    );
}

#[test]
fn the_curated_list_at_another_commit_is_refused() {
    let list = curated_list(&["shooteru"]);
    std::fs::write(list.path().join("commit"), "0000000\n").unwrap();
    let settings = ArcadeSettings {
        curated: true,
        ..ArcadeSettings::default()
    };
    let error = engine()
        .with_curated_list(list.path())
        .run(
            &data("arcade.xml"),
            &arcade_profile(&settings),
            &support::options(),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("tools/curated-list/fetch"),
        "{error}"
    );
}

#[test]
fn two_support_files_of_one_kind_are_refused() {
    let options = title_select_selection::RunOptions {
        support_files: support_files(&["catver", "catver"]),
        ..support::options()
    };
    let error = engine()
        .run(
            &data("arcade.xml"),
            &arcade_profile(&ArcadeSettings::default()),
            &options,
        )
        .unwrap_err();
    assert!(error.to_string().contains("catver.ini"), "{error}");
}

#[test]
fn the_pinned_curated_list_reads_where_tools_curated_list_fetch_unpacks_it() {
    let dir = std::env::var_os(title_select_selection::CURATED_LIST_ENV).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.curated-list"),
        PathBuf::from,
    );
    let settings = ArcadeSettings {
        curated: true,
        ..ArcadeSettings::default()
    };
    let outcome = engine()
        .with_curated_list(&dir)
        .run(
            &data("arcade.xml"),
            &arcade_profile(&settings),
            &support::options(),
        )
        .unwrap_or_else(|e| {
            panic!("{e}\nthe arcade tests need the curated list: run tools/curated-list/fetch")
        });
    // None of the synthetic machines is a real game it names.
    assert!(winners(&outcome.selection).is_empty());
    assert!(
        outcome
            .selection
            .pins
            .arcade
            .unwrap()
            .curated
            .unwrap()
            .unmatched
            .len()
            > 100
    );
}

#[test]
fn a_profiles_rating_settings_reach_the_selector() {
    // A system's own block, read from JSON as the store keeps it.
    let profile: SelectionProfile = serde_json::from_str(
        r#"{"systems":{"MAME":{"arcade":{"minimum_rating":70,"drop_unrated":true}}}}"#,
    )
    .unwrap();
    let options = title_select_selection::RunOptions {
        support_files: support_files(&["bestgames"]),
        ..support::options()
    };
    let selection = engine()
        .run(&data("arcade.xml"), &profile, &options)
        .unwrap()
        .selection;
    assert_eq!(winners(&selection), set(&["fighteru", "shooterw"]));
    let dropped = exclusions(&selection);
    // Below the minimum, and unrated.
    assert_eq!(dropped["racera"], "rating");
    assert_eq!(dropped["mystery"], "rating");
    assert!(selection.system_settings);

    // A profile that leaves them out rates nothing.
    let unrated = engine()
        .run(&data("arcade.xml"), &SelectionProfile::default(), &options)
        .unwrap()
        .selection;
    assert!(!exclusions(&unrated).values().any(|stage| stage == "rating"));
}

#[test]
fn a_profiles_curated_switch_reaches_the_selector_and_is_off_by_default() {
    let list = curated_list(&["shooteru"]);
    let engine = engine().with_curated_list(list.path());
    let profile: SelectionProfile = serde_json::from_str(r#"{"arcade":{"curated":true}}"#).unwrap();
    let selection = engine
        .run(&data("arcade.xml"), &profile, &support::options())
        .unwrap()
        .selection;
    assert_eq!(winners(&selection), set(&["shooteru"]));
    assert_eq!(
        group(&selection, "shooter").winners[0].rule.stage,
        "election:curated"
    );
    assert_eq!(exclusions(&selection)["fighteru"], "curated");

    // Off, the list isn't read, so a missing one doesn't matter, and the Selection records none.
    let off = engine
        .with_curated_list(&list.path().join("missing"))
        .run(
            &data("arcade.xml"),
            &SelectionProfile::default(),
            &support::options(),
        )
        .unwrap()
        .selection;
    assert!(winners(&off).len() > 1);
    assert_eq!(off.pins.arcade.unwrap().curated, None);
}
