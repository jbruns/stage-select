//! The arcade Selection's pure logic (ADR 0027): its stages in plain words, its Dependencies summed up in one line, and
//! the Profile editor's `arcade` block for a system.

use std::collections::{BTreeMap, BTreeSet};

use title_select_api::arcade::{
    ArcadeSettings, Category, DriverStatus, ElectionStep, Variant, is_arcade_system,
    takes_arcade_settings,
};
use title_select_api::dat::MAME;
use title_select_api::selection::{
    ArcadePins, CategorySource, DEPENDENCY_STAGE, Dependency, Selection, SelectionProfile,
};
use title_select_api::{GameStatus, SourceSetSystem};

/// An arcade Rule's stage in plain words, or `None` for a stage the arcade selector doesn't name.
pub fn stage_label(stage: &str) -> Option<&'static str> {
    Some(match stage {
        "structural" => "Not a game",
        "list" => "Your lists",
        "curated" => "Curated list",
        "hardware" => "Hardware",
        "category" => "Category",
        "rating" => "Rating",
        "quality" => "Driver status",
        "variant" => "Variant",
        "year" => "Year",
        "players" => "Players",
        "controls" => "Controls",
        "orientation" => "Screen",
        "media" => "Disks and samples",
        "election" => "Only one left",
        "election:curated" => "Named by the curated list",
        "election:shortname" => "Tie-break by shortname",
        DEPENDENCY_STAGE => "Dependency",
        _ => {
            return ElectionStep::ALL
                .into_iter()
                .find(|s| s.stage() == stage)
                .map(ElectionStep::label);
        }
    })
}

/// A filter the arcade Selection records as unavailable, in plain words.
pub fn filter_label(filter: &str) -> &str {
    match filter {
        "mechanical" => "mechanical machines",
        "screenless" => "machines with no screen",
        "software_lists" => "software-list machines",
        "category" => "category",
        "quality" => "driver status",
        "year" => "year",
        "players" => "players",
        "controls" => "controls",
        "orientation" => "screen orientation",
        "samples" => "samples",
        "rating" => "rating",
        other => other,
    }
}

/// What an arcade Selection's pins say you should know, one sentence each: where categories came from when catver
/// didn't decide them all, and the filters that dropped nothing because there was nothing to read.
pub fn notices(pins: &ArcadePins) -> Vec<String> {
    const SIGNALS: &str = "its driver's folder, software lists and controls";
    let mut out = Vec::new();
    match (pins.category, pins.category_fallbacks) {
        (Some(CategorySource::ListxmlHeuristic), _) => out.push(format!(
            "No catver.ini was read, so categories come from what the ListXML says about each machine: {SIGNALS}. \
             It can miss quiz, mature and most tabletop machines."
        )),
        (Some(CategorySource::Catver), 1) => out.push(format!(
            "catver.ini doesn't list 1 machine, so its category comes from what the ListXML says about it: {SIGNALS}."
        )),
        (Some(CategorySource::Catver), n) if n > 1 => out.push(format!(
            "catver.ini doesn't list {n} machines, so their categories come from what the ListXML says about each: \
             {SIGNALS}."
        )),
        _ => {}
    }
    // The rating filter reads a file you add, not the DAT.
    let (rating, signals): (Vec<&String>, Vec<&String>) =
        pins.unavailable.iter().partition(|f| *f == "rating");
    if !signals.is_empty() {
        let filters: Vec<&str> = signals.iter().map(|f| filter_label(f)).collect();
        out.push(format!(
            "These filters had nothing to read, so they dropped nothing: {}.",
            filters.join(", ")
        ));
    }
    if !rating.is_empty() {
        out.push("No bestgames.ini was read, so the rating filter dropped nothing.".to_owned());
    }
    out
}

/// The systems whose tabs hold the arcade settings in place of the Retool ones: those of the registered Arcade DATs,
/// `previewed` when its preview is an arcade Selection, and MAME, the system of every Arcade DAT, when the Profile has
/// system settings for it and no DAT is registered for it yet (`takes_arcade_settings`): a MAME whose registered DATs
/// are all Logiqx DATs keeps its Retool settings. An `arcade` block on any other system never makes it arcade: the check refuses it.
pub fn arcade_systems(
    registered: &[SourceSetSystem],
    previewed: Option<&str>,
    profile: &SelectionProfile,
) -> BTreeSet<String> {
    let mut arcade: BTreeSet<String> = registered
        .iter()
        .filter(|s| s.arcade)
        .filter_map(|s| s.system.clone())
        .collect();
    arcade.extend(previewed.map(str::to_owned));
    arcade.extend(
        profile
            .systems
            .keys()
            .filter(|s| takes_arcade_settings(s, registered))
            .cloned(),
    );
    arcade
}

/// How many Dependencies some Games need, by what the latest Audit says of each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    pub complete: usize,
    pub incomplete: usize,
    pub missing: usize,
    /// Not in the latest Audit, or not audited.
    pub unknown: usize,
    /// Complete or incomplete with some ROM on Claim Evidence alone.
    pub claimed: usize,
}

impl Needs {
    /// Counts `dependencies` by their status in `games`; `claimed` names the Games whose status rests on Claim
    /// Evidence.
    pub fn of<'a>(
        dependencies: impl IntoIterator<Item = &'a Dependency>,
        games: &BTreeMap<String, GameStatus>,
        claimed: &BTreeSet<String>,
    ) -> Needs {
        let mut needs = Needs::default();
        for d in dependencies {
            let status = games.get(&d.title);
            match status {
                Some(GameStatus::Complete) => needs.complete += 1,
                Some(GameStatus::Incomplete) => needs.incomplete += 1,
                Some(GameStatus::Missing) => needs.missing += 1,
                None => needs.unknown += 1,
            }
            if matches!(status, Some(GameStatus::Complete | GameStatus::Incomplete))
                && claimed.contains(&d.title)
            {
                needs.claimed += 1;
            }
        }
        needs
    }

    pub fn count(&self) -> usize {
        self.complete + self.incomplete + self.missing + self.unknown
    }

    /// The count and what you have of them: "3 dependencies, all complete", "2 dependencies: 1 missing, 1 not
    /// audited", and how many rest on claim evidence alone: "(1 on claim evidence only)". `None` when there are none.
    pub fn summary(&self) -> Option<String> {
        let n = self.count();
        if n == 0 {
            return None;
        }
        let what = if n == 1 { "dependency" } else { "dependencies" };
        let claims = match self.claimed {
            0 => String::new(),
            _ if n == 1 => " (claim evidence only)".to_owned(),
            k => format!(" ({k} on claim evidence only)"),
        };
        if self.complete == n {
            let all = if n == 1 { "complete" } else { "all complete" };
            return Some(format!("{n} {what}, {all}{claims}"));
        }
        let parts: Vec<String> = [
            (self.missing, "missing"),
            (self.incomplete, "incomplete"),
            (self.unknown, "not audited"),
            (self.complete, "complete"),
        ]
        .into_iter()
        .filter(|(k, _)| *k > 0)
        .map(|(k, label)| format!("{k} {label}"))
        .collect();
        Some(format!("{n} {what}: {}{claims}", parts.join(", ")))
    }
}

/// Every Dependency of `selection`, in group order.
pub fn dependencies(selection: &Selection) -> Vec<&Dependency> {
    selection
        .groups
        .iter()
        .flat_map(|g| &g.dependencies)
        .collect()
}

/// The Dependencies the Game `title` needs.
pub fn dependencies_of<'a>(selection: &'a Selection, title: &str) -> Vec<&'a Dependency> {
    dependencies(selection)
        .into_iter()
        .filter(|d| d.needed_by.iter().any(|n| n == title))
        .collect()
}

pub fn category_label(category: Category) -> &'static str {
    match category {
        Category::NonArcade => "Not arcade games: computers, consoles, handhelds, instruments",
        Category::Gambling => "Gambling, redemption and electromechanical",
        Category::Tabletop => "Mahjong, hanafuda and other tabletop",
        Category::BoardGame => "Board games and chess",
        Category::Quiz => "Quiz and trivia",
        Category::Mature => "Mature, as catver.ini marks it",
    }
}

pub fn variant_label(variant: Variant) -> &'static str {
    match variant {
        Variant::Bootleg => "Bootlegs",
        Variant::Hack => "Hacks",
        Variant::Prototype => "Prototypes",
    }
}

pub fn driver_status_label(status: Option<DriverStatus>) -> &'static str {
    match status {
        Some(DriverStatus::Good) => "Good only",
        Some(DriverStatus::Imperfect) => "Imperfect or better",
        Some(DriverStatus::Preliminary) | None => "Any status",
    }
}

/// Where a system's arcade settings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcadeSource {
    Defaults,
    Global,
    System,
}

impl ArcadeSource {
    pub fn of(profile: &SelectionProfile, system: &str) -> ArcadeSource {
        if profile
            .systems
            .get(system)
            .is_some_and(|s| s.arcade.is_some())
        {
            ArcadeSource::System
        } else if profile.arcade.is_some() {
            ArcadeSource::Global
        } else {
            ArcadeSource::Defaults
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArcadeSource::Defaults => "Defaults",
            ArcadeSource::Global => "From global settings",
            ArcadeSource::System => "Set for this system",
        }
    }
}

/// Changes `system`'s arcade settings, starting from the ones that apply to it now, so that the change applies to
/// this system only.
pub fn update_arcade(
    profile: &mut SelectionProfile,
    system: &str,
    f: impl FnOnce(&mut ArcadeSettings),
) {
    let mut settings = profile.arcade_settings(system);
    f(&mut settings);
    profile.systems.entry(system.to_owned()).or_default().arcade = Some(settings);
}

/// Why `system`'s own arcade settings, on a tab that holds the Retool ones, never apply.
pub fn stray_arcade_problem(system: &str) -> String {
    match is_arcade_system(system) {
        true => format!(
            "Your {system} DATs aren't arcade DATs, so your arcade settings for {system} never apply. Remove them to save."
        ),
        false => format!(
            "Only {MAME} uses arcade settings, so your arcade settings for {system} never apply. Remove them to save."
        ),
    }
}

/// Drops `system`'s own arcade settings, so the global ones, or the defaults, apply.
pub fn use_global_arcade(profile: &mut SelectionProfile, system: &str) {
    let Some(own) = profile.systems.get_mut(system) else {
        return;
    };
    own.arcade = None;
    if own.is_empty() {
        profile.systems.remove(system);
    }
}

/// Sets `system`'s arcade settings to the defaults, whatever the global ones say.
pub fn reset_arcade(profile: &mut SelectionProfile, system: &str) {
    profile.systems.entry(system.to_owned()).or_default().arcade = Some(ArcadeSettings::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::{Rule, SelectionGroup, SystemSettings};

    fn dependency(title: &str, needed_by: &[&str]) -> Dependency {
        Dependency {
            title: title.into(),
            needed_by: needed_by.iter().map(|n| n.to_string()).collect(),
            rule: Rule {
                stage: DEPENDENCY_STAGE.into(),
                reference: None,
                message: format!("needed by {}", needed_by.join(", ")),
            },
        }
    }

    fn statuses(of: &[(&str, GameStatus)]) -> BTreeMap<String, GameStatus> {
        of.iter().map(|(t, s)| (t.to_string(), *s)).collect()
    }

    #[test]
    fn arcade_stages_read_in_plain_words_and_retool_stages_are_left_alone() {
        assert_eq!(stage_label("election:parent"), Some("Parent over clone"));
        assert_eq!(stage_label("structural"), Some("Not a game"));
        assert_eq!(
            stage_label("election:curated"),
            Some("Named by the curated list")
        );
        assert_eq!(stage_label(DEPENDENCY_STAGE), Some("Dependency"));
        assert_eq!(stage_label("Parent selection"), None);
        for step in ElectionStep::ALL {
            assert!(stage_label(step.stage()).is_some(), "{step:?}");
        }
    }

    #[test]
    fn dependencies_sum_up_in_one_line() {
        let deps = [
            dependency("neogeo", &["mslug"]),
            dependency("z80", &["mslug"]),
            dependency("ym2610", &["mslug"]),
        ];
        let all = statuses(&[
            ("neogeo", GameStatus::Complete),
            ("z80", GameStatus::Complete),
            ("ym2610", GameStatus::Complete),
        ]);
        let none = BTreeSet::new();
        assert_eq!(
            Needs::of(&deps, &all, &none).summary().as_deref(),
            Some("3 dependencies, all complete")
        );
        let some = statuses(&[
            ("neogeo", GameStatus::Missing),
            ("z80", GameStatus::Complete),
        ]);
        assert_eq!(
            Needs::of(&deps, &some, &none).summary().as_deref(),
            Some("3 dependencies: 1 missing, 1 not audited, 1 complete")
        );
        assert_eq!(
            Needs::of(&deps[..1], &all, &none).summary().as_deref(),
            Some("1 dependency, complete")
        );
        assert_eq!(Needs::of(&[], &all, &none).summary(), None);
    }

    #[test]
    fn a_dependency_you_have_only_on_claim_evidence_says_so_in_the_line() {
        let deps = [
            dependency("neogeo", &["mslug"]),
            dependency("z80", &["mslug"]),
            dependency("ym2610", &["mslug"]),
        ];
        let all = statuses(&[
            ("neogeo", GameStatus::Complete),
            ("z80", GameStatus::Complete),
            ("ym2610", GameStatus::Missing),
        ]);
        let claimed = BTreeSet::from(["neogeo".to_string(), "ym2610".to_string()]);
        assert_eq!(
            Needs::of(&deps[..2], &all, &claimed).summary().as_deref(),
            Some("2 dependencies, all complete (1 on claim evidence only)")
        );
        assert_eq!(
            Needs::of(&deps[..1], &all, &claimed).summary().as_deref(),
            Some("1 dependency, complete (claim evidence only)")
        );
        // A missing Game's claims say nothing about what you have.
        assert_eq!(
            Needs::of(&deps, &all, &claimed).summary().as_deref(),
            Some("3 dependencies: 1 missing, 2 complete (1 on claim evidence only)")
        );
    }

    #[test]
    fn a_games_dependencies_are_those_that_name_it() {
        let mut bios = SelectionGroup {
            group: "neogeo".into(),
            unresolved: false,
            winners: Vec::new(),
            alternatives: Vec::new(),
            exclusions: Vec::new(),
            dependencies: vec![dependency("neogeo", &["kof98", "mslug"])],
            choice: None,
            drift: false,
        };
        let mut devices = bios.clone();
        devices.group = "z80".into();
        devices.dependencies = vec![dependency("z80", &["pacman"])];
        bios.group = "neogeo".into();
        let selection = Selection {
            pins: Default::default(),
            dat_sha256: String::new(),
            system: "MAME".into(),
            system_settings: false,
            summary: Default::default(),
            coverage_drift: Default::default(),
            groups: vec![bios, devices],
            stale_choices: Default::default(),
        };
        assert_eq!(dependencies(&selection).len(), 2);
        let titles = |t| {
            dependencies_of(&selection, t)
                .iter()
                .map(|d| d.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("mslug"), ["neogeo"]);
        assert_eq!(titles("pacman"), ["z80"]);
        assert!(titles("galaga").is_empty());
    }

    #[test]
    fn the_notices_say_where_categories_came_from_and_what_dropped_nothing() {
        let pins = ArcadePins {
            build: "0.289".into(),
            category: Some(CategorySource::ListxmlHeuristic),
            unavailable: vec!["year".into(), "rating".into()],
            category_fallbacks: 0,
            support_files: Vec::new(),
            curated: None,
        };
        let said = notices(&pins);
        assert!(
            said[0].starts_with(
                "No catver.ini was read, so categories come from what the ListXML says"
            )
        );
        assert_eq!(
            said[1..],
            [
                "These filters had nothing to read, so they dropped nothing: year.",
                "No bestgames.ini was read, so the rating filter dropped nothing.",
            ]
        );
        // Where catver lacks some machines, the notice counts those the heuristic decided.
        let partial = ArcadePins {
            category: Some(CategorySource::Catver),
            category_fallbacks: 2,
            unavailable: Vec::new(),
            ..pins.clone()
        };
        assert_eq!(
            notices(&partial),
            [
                "catver.ini doesn't list 2 machines, so their categories come from what the ListXML says about each: \
                 its driver's folder, software lists and controls."
            ]
        );
        let one = ArcadePins {
            category_fallbacks: 1,
            ..partial
        };
        assert!(
            notices(&one)[0]
                .starts_with("catver.ini doesn't list 1 machine, so its category comes from")
        );
        // Where catver decided every machine, or with the category filter off, nothing says where categories came
        // from.
        for category in [Some(CategorySource::Catver), None] {
            let quiet = ArcadePins {
                category,
                unavailable: Vec::new(),
                ..pins.clone()
            };
            assert!(notices(&quiet).is_empty());
        }
    }

    #[test]
    fn a_change_for_one_system_starts_from_what_applies_to_it_and_using_global_drops_it() {
        let mut profile = SelectionProfile {
            arcade: Some(ArcadeSettings {
                drop_samples: true,
                ..ArcadeSettings::default()
            }),
            ..SelectionProfile::default()
        };
        assert_eq!(ArcadeSource::of(&profile, "MAME"), ArcadeSource::Global);
        update_arcade(&mut profile, "MAME", |a| a.drop_disks = true);
        assert_eq!(ArcadeSource::of(&profile, "MAME"), ArcadeSource::System);
        let own = profile.arcade_settings("MAME");
        assert!(own.drop_samples && own.drop_disks);

        use_global_arcade(&mut profile, "MAME");
        assert!(!profile.systems.contains_key("MAME"));
        profile.systems.insert(
            "MAME".into(),
            SystemSettings {
                additions: true,
                ..SystemSettings::default()
            },
        );
        reset_arcade(&mut profile, "MAME");
        assert_eq!(profile.arcade_settings("MAME"), ArcadeSettings::default());
        use_global_arcade(&mut profile, "MAME");
        assert!(profile.systems["MAME"].additions);
        profile.arcade = None;
        assert_eq!(ArcadeSource::of(&profile, "Other"), ArcadeSource::Defaults);
    }

    #[test]
    fn a_system_is_arcade_by_its_dat_never_by_an_arcade_block() {
        let registered = vec![
            SourceSetSystem {
                source_set: "mame".into(),
                system: Some("MAME".into()),
                arcade: true,
            },
            SourceSetSystem {
                source_set: "gb".into(),
                system: Some("Nintendo - Game Boy".into()),
                arcade: false,
            },
        ];
        let mut profile = SelectionProfile::default();
        for system in ["Nintendo - Game Boy", "Sega - Mega Drive"] {
            profile.systems.insert(
                system.into(),
                SystemSettings {
                    arcade: Some(ArcadeSettings::default()),
                    ..SystemSettings::default()
                },
            );
        }
        assert_eq!(
            arcade_systems(&registered, None, &profile),
            BTreeSet::from(["MAME".to_owned()])
        );
        // With no MAME source set registered, a MAME preview, or MAME's own tab, still shows its arcade settings.
        assert_eq!(
            arcade_systems(&[], Some("MAME"), &SelectionProfile::default()),
            BTreeSet::from(["MAME".to_owned()])
        );
        profile
            .systems
            .insert("MAME".into(), SystemSettings::default());
        assert_eq!(
            arcade_systems(&[], None, &profile),
            BTreeSet::from(["MAME".to_owned()])
        );
    }

    #[test]
    fn a_logiqx_dat_named_mame_keeps_its_system_settings_editor() {
        // A Logiqx DAT may name itself MAME; the Retool port selects it, so its tab keeps the Retool settings.
        let registered = vec![SourceSetSystem {
            source_set: "mame-logiqx".into(),
            system: Some("MAME".into()),
            arcade: false,
        }];
        let mut profile = SelectionProfile::default();
        assert!(arcade_systems(&registered, None, &profile).is_empty());
        profile.systems.insert(
            "MAME".into(),
            SystemSettings {
                region_order: Some(vec!["Japan".into(), "USA".into()]),
                ..SystemSettings::default()
            },
        );
        assert!(arcade_systems(&registered, None, &profile).is_empty());
        // Nor does an arcade block left on it: the tab says so and offers to remove it.
        profile.systems.get_mut("MAME").unwrap().arcade = Some(ArcadeSettings::default());
        assert!(arcade_systems(&registered, None, &profile).is_empty());
        assert_eq!(
            stray_arcade_problem("MAME"),
            "Your MAME DATs aren't arcade DATs, so your arcade settings for MAME never apply. Remove them to save."
        );
    }
}
