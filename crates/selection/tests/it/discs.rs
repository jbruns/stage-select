//! Multi-disc Games: the disc-name normalization is Retool's own, and the check after Selection flags a group
//! whose discs don't come whole from one release, without changing what the engine picks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::support;
use stage_select_api::discs::{self, DISC_RENAME, normalize};
use stage_select_api::selection::{ChoiceStatus, Selection, SelectionProfile};

#[test]
fn the_disc_rename_table_is_the_pinned_configs() {
    let path = Path::new(
        &std::env::var_os("STAGE_SELECT_CLONE_LISTS").expect("set in .cargo/config.toml"),
    )
    .join("config/internal-config.json");
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(rename = "discRename")]
        disc_rename: indexmap::IndexMap<String, String>,
    }
    let config: Config = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nthe Selection tests need the clone lists: run tools/clone-lists/fetch",
            path.display()
        )
    }))
    .unwrap();
    let pinned: Vec<(String, String)> = config.disc_rename.into_iter().collect();
    let ours: Vec<(String, String)> = DISC_RENAME
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(ours, pinned);
}

/// Retool's `TitleTools.normalize_discs` on names built from every key of the table, frozen in `tests/oracle/`.
#[test]
fn disc_names_normalize_exactly_as_retools_title_tools_do() {
    let mut differences = Vec::new();
    for line in include_str!("../oracle/disc-names.jsonl").lines() {
        let (name, retools): (String, String) = serde_json::from_str(line).unwrap();
        if normalize(&name) != retools {
            differences.push(format!(
                "{name:?}: {:?}, Retool {retools:?}",
                normalize(&name)
            ));
        }
    }
    assert!(differences.is_empty(), "{differences:#?}");
}

fn game(name: &str) -> String {
    format!(
        "\t<game name=\"{name}\">\n\t\t<category>Games</category>\n\t\t<description>{name}</description>\n\
         \t\t<rom name=\"{name}.bin\" size=\"1000\" crc=\"00000000\" md5=\"00000000000000000000000000000000\" \
         sha1=\"0000000000000000000000000000000000000000\"/>\n\t</game>\n"
    )
}

/// A synthetic DAT where only Europe has Disc 2 of `Saga`, beside a single-disc Game.
fn only_europe_has_disc_2(dir: &Path) -> PathBuf {
    write_dat(
        dir,
        "discs.dat",
        &[
            "Saga (USA) (Disc 1)",
            "Saga (Europe) (Disc 1)",
            "Saga (Europe) (Disc 2)",
            "Solo (USA)",
        ],
    )
}

fn write_dat(dir: &Path, file: &str, games: &[&str]) -> PathBuf {
    let name = "Stage Select - Discs";
    let games: String = games.iter().map(|g| game(g)).collect();
    let path = dir.join(file);
    std::fs::write(
        &path,
        format!(
            "<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n\t\t<name>{name}</name>\n\
             \t\t<description>{name}</description>\n\t\t<version>2024-01-01 00-00-00</version>\n\
             \t</header>\n{games}</datafile>\n"
        ),
    )
    .unwrap();
    for sub in ["clonelists", "metadata", "mias", "retroachievements"] {
        std::fs::create_dir_all(dir.join("data").join(sub)).unwrap();
    }
    path
}

fn select(dir: &Path, dat: &Path, profile: &SelectionProfile) -> Selection {
    let engine = support::engine().with_data_dir(&dir.join("data"));
    engine.select(dat, profile, &support::options()).unwrap()
}

fn titles<'a>(titles: impl IntoIterator<Item = &'a String>) -> BTreeSet<&'a str> {
    titles.into_iter().map(String::as_str).collect()
}

#[test]
fn only_europe_having_disc_2_leaves_the_group_unresolved_until_your_choice_settles_it() {
    let work = tempfile::tempdir().unwrap();
    let dat = only_europe_has_disc_2(work.path());

    let selection = select(work.path(), &dat, &SelectionProfile::default());
    let saga = selection.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(saga.unresolved, "{saga:#?}");
    // The engine's pick is unchanged: each disc is chosen by its own name, so Disc 2 can only come from Europe.
    assert_eq!(
        titles(saga.kept.iter().map(|w| &w.title)),
        BTreeSet::from(["Saga (USA) (Disc 1)", "Saga (Europe) (Disc 2)"])
    );
    assert!(saga.kept.iter().all(|w| !w.unresolved));
    let solo = selection.groups.iter().find(|g| g.group == "solo").unwrap();
    assert!(!solo.unresolved);
    assert_eq!(selection.summary.unresolved, 1);

    let europe: BTreeSet<String> = ["Saga (Europe) (Disc 1)", "Saga (Europe) (Disc 2)"]
        .map(String::from)
        .into();
    let mut profile = SelectionProfile::default();
    profile.set_choice(
        &selection.system,
        "saga",
        europe.clone(),
        &saga.picks(),
        saga.engine_unresolved(),
    );
    let chosen = select(work.path(), &dat, &profile);
    let saga = chosen.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(!saga.unresolved && !saga.needs_review(), "{saga:#?}");
    assert_eq!(saga.choice.as_ref().unwrap().status, ChoiceStatus::Applied);
    assert_eq!(titles(saga.kept.iter().map(|w| &w.title)), titles(&europe));
    assert_eq!(chosen.summary.needs_review(), 0);

    // Confirming the engine's mixed pick is an explicit choice too, and it is kept.
    let mut profile = SelectionProfile::default();
    let picks = selection
        .groups
        .iter()
        .find(|g| g.group == "saga")
        .unwrap()
        .picks();
    profile.set_choice(&selection.system, "saga", picks.clone(), &picks, true);
    assert_eq!(
        profile.choices[&selection.system],
        BTreeMap::from([("saga".to_owned(), picks)])
    );
    let confirmed = select(work.path(), &dat, &profile);
    let saga = confirmed.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(!saga.unresolved, "{saga:#?}");
    assert_eq!(saga.choice.as_ref().unwrap().status, ChoiceStatus::Applied);
    assert_eq!(discs::unresolved_reason(saga), None);
}

fn europe() -> BTreeSet<String> {
    ["Saga (Europe) (Disc 1)", "Saga (Europe) (Disc 2)"]
        .map(String::from)
        .into()
}

fn choosing(selection: &Selection, checked: BTreeSet<String>) -> SelectionProfile {
    let saga = selection.groups.iter().find(|g| g.group == "saga").unwrap();
    let mut profile = SelectionProfile::default();
    profile.set_choice(
        &selection.system,
        "saga",
        checked,
        &saga.picks(),
        saga.engine_unresolved(),
    );
    profile
}

#[test]
fn a_choice_that_leaves_out_a_disc_keeps_the_group_unresolved_and_is_kept() {
    let work = tempfile::tempdir().unwrap();
    let dat = only_europe_has_disc_2(work.path());
    let selection = select(work.path(), &dat, &SelectionProfile::default());
    let partial = BTreeSet::from(["Saga (Europe) (Disc 1)".to_owned()]);
    let profile = choosing(&selection, partial.clone());

    let chosen = select(work.path(), &dat, &profile);
    let saga = chosen.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(saga.unresolved, "{saga:#?}");
    let choice = saga.choice.as_ref().unwrap();
    assert_eq!(
        (choice.status, &choice.checked),
        (ChoiceStatus::Applied, &partial)
    );
    assert_eq!(
        discs::unresolved_reason(saga).as_deref(),
        Some("your choice doesn't cover disc 2")
    );
    assert_eq!(chosen.summary.unresolved, 1);
}

#[test]
fn a_saved_whole_choice_is_flagged_again_when_a_newer_dat_adds_a_disc() {
    let work = tempfile::tempdir().unwrap();
    let dat = only_europe_has_disc_2(work.path());
    let selection = select(work.path(), &dat, &SelectionProfile::default());
    let profile = choosing(&selection, europe());
    let saga = select(work.path(), &dat, &profile)
        .groups
        .into_iter()
        .find(|g| g.group == "saga");
    assert!(!saga.unwrap().unresolved);

    let newer = write_dat(
        work.path(),
        "discs (newer).dat",
        &[
            "Saga (USA) (Disc 1)",
            "Saga (Europe) (Disc 1)",
            "Saga (Europe) (Disc 2)",
            "Saga (Europe) (Disc 3)",
            "Solo (USA)",
        ],
    );
    let again = select(work.path(), &newer, &profile);
    let saga = again.groups.iter().find(|g| g.group == "saga").unwrap();
    assert!(saga.unresolved, "{saga:#?}");
    let choice = saga.choice.as_ref().unwrap();
    assert_eq!(
        (choice.status, &choice.checked),
        (ChoiceStatus::Applied, &europe())
    );
    assert_eq!(
        discs::unresolved_reason(saga).as_deref(),
        Some("your choice doesn't cover disc 3")
    );
    assert!(
        !again
            .groups
            .iter()
            .find(|g| g.group == "solo")
            .unwrap()
            .unresolved
    );
}

/// A synthetic DAT and clone list that group Rival's discs by content, not number, as Redump's PlayStation clone list
/// does: USA numbers the Arcade Disc 1 and the Evolution Disc 2, Europe the other way round.
fn per_disc_groups(dir: &Path) -> PathBuf {
    let dat = write_dat(
        dir,
        "rival.dat",
        &[
            "Rival (USA) (Disc 1) (Arcade Disc)",
            "Rival (USA) (Disc 2) (Evolution Disc)",
            "Rival (Europe) (Disc 1) (Evolution Disc)",
            "Rival (Europe) (Disc 2) (Arcade Disc)",
        ],
    );
    std::fs::write(
        dir.join("data/clonelists/Stage Select - Discs.json"),
        r#"{
  "description": {"name": "Stage Select - Discs", "lastUpdated": "2024-01-01 00:00:00", "minimumVersion": "2.4.8"},
  "variants": [
    {"group": "Rival (Disc 1) (Arcade Disc)", "titles": [
      {"searchTerm": "Rival (Disc 1) (Arcade Disc)"}, {"searchTerm": "Rival (Disc 2) (Arcade Disc)"}]},
    {"group": "Rival (Disc 2) (Evolution Disc)", "titles": [
      {"searchTerm": "Rival (Disc 1) (Evolution Disc)"}, {"searchTerm": "Rival (Disc 2) (Evolution Disc)"}]}
  ]
}
"#,
    )
    .unwrap();
    dat
}

const ARCADE: &str = "rival (disc 1) (arcade disc)";
const EVOLUTION: &str = "rival (disc 2) (evolution disc)";

#[test]
fn a_per_disc_group_whose_regions_number_the_disc_differently_resolves_with_one_title() {
    let work = tempfile::tempdir().unwrap();
    let dat = per_disc_groups(work.path());
    let usa = stage_select_selection::RunOptions {
        coverage: Some(vec!["USA".into()]),
        ..support::options()
    };
    let engine = support::engine().with_data_dir(&work.path().join("data"));

    let selection = engine
        .select(&dat, &SelectionProfile::default(), &usa)
        .unwrap();
    for (name, title) in [
        (ARCADE, "Rival (USA) (Disc 1) (Arcade Disc)"),
        (EVOLUTION, "Rival (USA) (Disc 2) (Evolution Disc)"),
    ] {
        let g = selection
            .groups
            .iter()
            .find(|g| g.group == name)
            .unwrap_or_else(|| panic!("{:#?}", selection.groups));
        assert_eq!(
            titles(g.kept.iter().map(|w| &w.title)),
            BTreeSet::from([title])
        );
        assert!(g.left_out.iter().all(|e| e.rule.is_coverage()), "{g:#?}");
        assert!(!g.unresolved, "{g:#?}");
        assert_eq!(discs::unresolved_reason(g), None);
        assert!(discs::missing(g).is_empty());
    }

    // A choice of the USA title in each, saved while the groups were unresolved, now matches the engine's pick.
    let saved = |checked: [&str; 2]| {
        let mut profile = SelectionProfile::default();
        profile.choices.insert(
            selection.system.clone(),
            [ARCADE, EVOLUTION]
                .into_iter()
                .zip(checked)
                .map(|(name, title)| (name.to_owned(), BTreeSet::from([title.to_owned()])))
                .collect(),
        );
        profile
    };
    let usa_titles = [
        "Rival (USA) (Disc 1) (Arcade Disc)",
        "Rival (USA) (Disc 2) (Evolution Disc)",
    ];
    let chosen = engine.select(&dat, &saved(usa_titles), &usa).unwrap();
    for name in [ARCADE, EVOLUTION] {
        let g = chosen.groups.iter().find(|g| g.group == name).unwrap();
        assert_eq!(
            g.choice.as_ref().unwrap().status,
            ChoiceStatus::MatchesDefault
        );
        assert!(!g.unresolved, "{g:#?}");
        assert_eq!(discs::unresolved_reason(g), None);
    }
    assert_eq!(chosen.summary.unresolved, 0);

    // With no Coverage, a choice of Europe's Arcade Disc, which Europe numbers 2, is applied and settles the group.
    let europe = saved([
        "Rival (Europe) (Disc 2) (Arcade Disc)",
        "Rival (USA) (Disc 2) (Evolution Disc)",
    ]);
    let chosen = engine.select(&dat, &europe, &support::options()).unwrap();
    let g = chosen.groups.iter().find(|g| g.group == ARCADE).unwrap();
    assert_eq!(g.choice.as_ref().unwrap().status, ChoiceStatus::Applied);
    assert!(!g.unresolved, "{g:#?}");
    assert_eq!(discs::unresolved_reason(g), None);
    assert!(discs::missing(g).is_empty());
    assert_eq!(chosen.summary.unresolved, 0, "{:#?}", chosen.groups);
}
