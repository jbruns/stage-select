//! The arcade selector's settings (ADR 0027): the `arcade` block of a Selection Profile, globally and per system, and
//! the names it uses.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// How well a driver works, as MAME rates it, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverStatus {
    Preliminary,
    Imperfect,
    Good,
}

impl DriverStatus {
    pub const ALL: [DriverStatus; 3] = [
        DriverStatus::Good,
        DriverStatus::Imperfect,
        DriverStatus::Preliminary,
    ];

    /// The name ListXML gives it.
    pub fn as_str(self) -> &'static str {
        match self {
            DriverStatus::Good => "good",
            DriverStatus::Imperfect => "imperfect",
            DriverStatus::Preliminary => "preliminary",
        }
    }
}

/// A kind of machine the category filter can drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Computers, consoles, handhelds, calculators, chess computers, instruments and other machines that aren't arcade
    /// games.
    NonArcade,
    /// Gambling, redemption and electromechanical machines.
    Gambling,
    /// Mahjong, hanafuda and other tabletop machines.
    Tabletop,
    /// Board games and chess computers.
    BoardGame,
    /// Quiz and trivia machines.
    Quiz,
    /// What catver marks `* Mature *`, whatever its category.
    Mature,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::NonArcade,
        Category::Gambling,
        Category::Tabletop,
        Category::BoardGame,
        Category::Quiz,
        Category::Mature,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::NonArcade => "non-arcade",
            Category::Gambling => "gambling",
            Category::Tabletop => "tabletop",
            Category::BoardGame => "board game",
            Category::Quiz => "quiz",
            Category::Mature => "mature",
        }
    }
}

/// A machine that isn't the release it copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    Bootleg,
    Hack,
    Prototype,
}

impl Variant {
    pub const ALL: [Variant; 3] = [Variant::Bootleg, Variant::Hack, Variant::Prototype];

    pub fn as_str(self) -> &'static str {
        match self {
            Variant::Bootleg => "bootleg",
            Variant::Hack => "hack",
            Variant::Prototype => "prototype",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// One step of the election of a family's winner, each one Rule. Ties after the last step go to the shortname that
/// sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElectionStep {
    /// Driver status, good over imperfect.
    Status,
    Region,
    Language,
    /// The parent over its clones.
    Parent,
    /// The newest revision.
    Revision,
    /// Not a bootleg or hack.
    Variant,
}

impl ElectionStep {
    /// ADR 0027's order.
    pub const ALL: [ElectionStep; 6] = [
        ElectionStep::Status,
        ElectionStep::Region,
        ElectionStep::Language,
        ElectionStep::Parent,
        ElectionStep::Revision,
        ElectionStep::Variant,
    ];

    /// The step as you read it, in the workbench's editor, its Rules and the problems a Profile check finds.
    pub fn label(self) -> &'static str {
        match self {
            ElectionStep::Status => "Best driver status",
            ElectionStep::Region => "Preferred region",
            ElectionStep::Language => "Preferred language",
            ElectionStep::Parent => "Parent over clone",
            ElectionStep::Revision => "Newest revision",
            ElectionStep::Variant => "Original over bootleg or hack",
        }
    }

    /// The step's stage, as its Rules name it.
    pub fn stage(self) -> &'static str {
        match self {
            ElectionStep::Status => "election:status",
            ElectionStep::Region => "election:region",
            ElectionStep::Language => "election:language",
            ElectionStep::Parent => "election:parent",
            ElectionStep::Revision => "election:revision",
            ElectionStep::Variant => "election:variant",
        }
    }
}

/// An inclusive range; an open end is `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Range {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

impl Range {
    pub fn is_open(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    pub fn contains(&self, n: u32) -> bool {
        self.min.is_none_or(|min| n >= min) && self.max.is_none_or(|max| n <= max)
    }

    pub fn describe(&self) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) => format!("{min} to {max}"),
            (Some(min), None) => format!("{min} or more"),
            (None, Some(max)) => format!("{max} or fewer"),
            (None, None) => "any number".into(),
        }
    }
}

/// The region names the arcade selector reads from MAME descriptions, in its default order.
pub const REGIONS: &[&str] = &[
    "World",
    "USA",
    "Europe",
    "Japan",
    "Asia",
    "Korea",
    "Taiwan",
    "Hong Kong",
    "China",
    "Brazil",
    "Hispanic",
    "Spain",
    "France",
    "Germany",
    "Italy",
    "UK",
    "Australia",
];

/// The language names the arcade selector reads from MAME descriptions, in its default order. languages.ini can name
/// others.
pub const LANGUAGES: &[&str] = &[
    "English",
    "Japanese",
    "Spanish",
    "French",
    "German",
    "Italian",
    "Portuguese",
    "Korean",
    "Chinese",
];

/// Whether `system` is the one an Arcade DAT's machines are filed under, MAME, the only system an `arcade` block of
/// its own can apply to.
pub fn is_arcade_system(system: &str) -> bool {
    system == crate::dat::MAME
}

/// Whether two region or language names are the same: trimmed, and ignoring ASCII case, as languages.ini may spell a
/// language other than the Operator does.
pub fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The control types ListXML names, which the controls filters take.
pub const CONTROL_TYPES: &[&str] = &[
    "joy",
    "doublejoy",
    "triplejoy",
    "stick",
    "paddle",
    "dial",
    "trackball",
    "lightgun",
    "positional",
    "pedal",
    "mouse",
    "keypad",
    "keyboard",
    "mahjong",
    "hanafuda",
    "gambling",
    "only_buttons",
];

/// What the arcade selector keeps and prefers. A setting left out of the block takes its default, ADR 0027's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArcadeSettings {
    pub drop_mechanical: bool,
    /// Drop machines with no screen.
    pub drop_screenless: bool,
    /// Drop machines that name a software list: computers and consoles.
    pub drop_software_lists: bool,
    pub drop_categories: BTreeSet<Category>,
    /// Drop machines whose driver status is below this; `None` keeps every status.
    pub minimum_driver_status: Option<DriverStatus>,
    pub drop_variants: BTreeSet<Variant>,
    pub years: Range,
    pub players: Range,
    /// Keep only machines with at least one of these control types; empty keeps every machine.
    pub require_controls: BTreeSet<String>,
    /// Drop machines with any of these control types.
    pub forbid_controls: BTreeSet<String>,
    /// Keep only machines whose first screen is this way round; `None` keeps both.
    pub orientation: Option<Orientation>,
    /// Drop machines that need Disks (CHDs).
    pub drop_disks: bool,
    /// Drop machines that play samples.
    pub drop_samples: bool,
    /// The election's steps, in order; a step left out doesn't run. The shortname breaks any tie left.
    pub election: Vec<ElectionStep>,
    /// Regions in order of preference, from [`REGIONS`]; a region left out ranks after those listed. When empty,
    /// region doesn't decide.
    pub region_order: Vec<String>,
    /// Languages in order of preference: [`LANGUAGES`], which descriptions name, or any language languages.ini names,
    /// matched ignoring ASCII case. When empty, language doesn't decide.
    pub language_order: Vec<String>,
    /// Machines kept whatever the filters say, but for BIOSes, devices and machines that can't run, in the override
    /// syntax: plain text matches part of a shortname or description, `|` all of one, `/` a regular expression.
    pub include: Vec<String>,
    /// Machines dropped, in the same syntax; `include` wins.
    pub exclude: Vec<String>,
    /// Keep only the families the curated list names, and prefer the machine it names before the election's steps.
    pub curated: bool,
    /// Drop families whose parent bestgames rates below this, as a tier's lower bound, such as 70 for
    /// `70 to 80 (Good)`; `None` keeps every rating. Without bestgames, the Selection records it unavailable.
    pub minimum_rating: Option<u32>,
    /// Drop families bestgames doesn't rate, whatever `minimum_rating` says.
    pub drop_unrated: bool,
}

impl Default for ArcadeSettings {
    fn default() -> Self {
        ArcadeSettings {
            drop_mechanical: true,
            drop_screenless: true,
            drop_software_lists: true,
            drop_categories: Category::ALL.into(),
            minimum_driver_status: Some(DriverStatus::Imperfect),
            drop_variants: BTreeSet::new(),
            years: Range::default(),
            players: Range::default(),
            require_controls: BTreeSet::new(),
            forbid_controls: BTreeSet::new(),
            orientation: None,
            drop_disks: false,
            drop_samples: false,
            election: ElectionStep::ALL.to_vec(),
            region_order: REGIONS.iter().map(|r| r.to_string()).collect(),
            language_order: LANGUAGES.iter().map(|l| l.to_string()).collect(),
            include: Vec::new(),
            exclude: Vec::new(),
            curated: false,
            minimum_rating: None,
            drop_unrated: false,
        }
    }
}

impl ArcadeSettings {
    /// What the selector can't read in these settings, one message each, as you read it after "your arcade
    /// settings": a step or name listed twice, a language with no name, or a region or control type it doesn't know:
    /// languages.ini can name any language. The include and exclude lists' patterns are the selector's to check.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen = BTreeSet::new();
        for step in &self.election {
            if !seen.insert(*step) {
                problems.push(format!("election order: {} is listed twice", step.label()));
            }
        }
        // Regions come from descriptions alone, so the selector reads only those it knows; languages.ini can name
        // any language, so a language needs only a name.
        for (i, name) in self.region_order.iter().enumerate() {
            if !REGIONS.iter().any(|r| same_name(r, name)) {
                problems.push(format!(
                    "region order: {name} is not a region the arcade selection reads"
                ));
            } else if self.region_order[..i].iter().any(|n| same_name(n, name)) {
                problems.push(format!("region order: {name} is listed twice"));
            }
        }
        for (i, name) in self.language_order.iter().enumerate() {
            if name.trim().is_empty() {
                problems.push("language order: a language has no name".to_owned());
            } else if self.language_order[..i].iter().any(|n| same_name(n, name)) {
                problems.push(format!("language order: {name} is listed twice"));
            }
        }
        for (list, controls) in [
            ("required controls", &self.require_controls),
            ("forbidden controls", &self.forbid_controls),
        ] {
            for control in controls {
                if !CONTROL_TYPES.contains(&control.as_str()) {
                    problems.push(format!(
                        "{list}: {control} is not a control type ListXML names"
                    ));
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_have_no_problems() {
        assert_eq!(ArcadeSettings::default().problems(), Vec::<String>::new());
    }

    #[test]
    fn problems_name_each_step_or_name_the_selector_cant_read() {
        let settings = ArcadeSettings {
            election: vec![ElectionStep::Region, ElectionStep::Region],
            region_order: vec!["World".into(), "Atlantis".into(), "world".into()],
            forbid_controls: ["telepathy".to_owned()].into(),
            ..ArcadeSettings::default()
        };
        assert_eq!(
            settings.problems(),
            [
                "election order: Preferred region is listed twice",
                "region order: Atlantis is not a region the arcade selection reads",
                "region order: world is listed twice",
                "forbidden controls: telepathy is not a control type ListXML names",
            ]
        );
    }

    #[test]
    fn a_language_needs_only_a_name() {
        let settings = ArcadeSettings {
            language_order: vec!["Russian".into(), " ".into(), "russian".into()],
            ..ArcadeSettings::default()
        };
        assert_eq!(
            settings.problems(),
            [
                "language order: a language has no name",
                "language order: russian is listed twice",
            ]
        );
    }
}
