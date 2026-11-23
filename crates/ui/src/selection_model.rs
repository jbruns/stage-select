//! The Selection screen's and the Profile editor's pure logic: a Selection's releases indexed for filtering and
//! sorting on every keystroke, the Operator's staged choices, and where each system setting comes from.

use std::collections::{BTreeMap, BTreeSet};

use stage_select_api::GameStatus;
use stage_select_api::arcade::same_name;
use stage_select_api::selection::{
    ChoiceStatus, LeftOutGame, Outcome, ReleaseRow, Rule, Selection, SelectionGroup,
    SelectionOptions, SelectionProfile, SystemSettings,
};

use crate::arcade_model::update_arcade;
use crate::model::{Sort, status_text};
use crate::parts::status::GroupStatus;
use crate::placement_model::FlagsNow;

pub fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Kept => "Kept",
        Outcome::Clone => "Clone",
        Outcome::LeftOut => "Left out",
        Outcome::Dependency => "Dependency",
    }
}

/// What an outcome means, for its tooltip.
pub fn outcome_hint(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Kept => "The version of a game the selection keeps.",
        Outcome::Clone => "Another version of a game you kept.",
        Outcome::LeftOut => "A rule of your selection profile leaves it out.",
        Outcome::Dependency => "A game you kept needs it to run.",
    }
}

/// A rule as the workbench shows it: the stage, in plain words for an arcade one, and the engine's message in the
/// glossary's words, "Your choice", or, for a Game its source set's coverage leaves out, that alone.
pub fn rule_text(rule: &Rule) -> String {
    if rule.is_choice() {
        "Your choice".to_string()
    } else if rule.is_coverage() {
        capitalised(&rule.message)
    } else {
        let stage = match rule.stage.as_str() {
            // Retool's name for the stage that leaves Games out.
            "Exclusions" => "Left out",
            other => crate::arcade_model::stage_label(other).unwrap_or(other),
        };
        format!("{stage}: {}", crate::rule_words::explanation(&rule.message))
    }
}

/// Why a Game was left out: the engine's reason and the rule that applied it, or, for a Game outside its source set's
/// coverage, that once.
pub fn left_out_text(exclusion: &LeftOutGame) -> String {
    if exclusion.rule.is_coverage() {
        rule_text(&exclusion.rule)
    } else {
        format!(
            "{}: {}",
            crate::rule_words::explanation(&exclusion.reason),
            rule_text(&exclusion.rule)
        )
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn choice_label(status: ChoiceStatus) -> &'static str {
    match status {
        ChoiceStatus::Applied => "Your choice",
        ChoiceStatus::MatchesDefault => "Matches the default now",
        ChoiceStatus::Conflicting => "Conflicting choice",
        ChoiceStatus::Stale => "Stale choice",
    }
}

/// A group's flags, as a bit set.
pub mod flag {
    pub const UNRESOLVED: u8 = 1;
    pub const STALE: u8 = 2;
    pub const CONFLICTING: u8 = 4;
    pub const DRIFT: u8 = 8;
    /// The group has a choice of yours, in any state.
    pub const CHOICE: u8 = 16;
    pub const NEEDS_REVIEW: u8 = UNRESOLVED | STALE | CONFLICTING;
}

pub fn group_flags(group: &SelectionGroup) -> u8 {
    let mut flags = 0;
    if group.unresolved {
        flags |= flag::UNRESOLVED;
    }
    if group.drift {
        flags |= flag::DRIFT;
    }
    if let Some(choice) = &group.choice {
        flags |= flag::CHOICE;
        match choice.status {
            ChoiceStatus::Stale => flags |= flag::STALE,
            ChoiceStatus::Conflicting => flags |= flag::CONFLICTING,
            ChoiceStatus::Applied | ChoiceStatus::MatchesDefault => {}
        }
    }
    flags
}

/// The words for a group's flags, as the Selection table shows them.
pub fn flag_labels(flags: u8) -> Vec<&'static str> {
    [
        (flag::UNRESOLVED, "Unresolved"),
        (flag::STALE, "Stale choice"),
        (flag::CONFLICTING, "Conflicting choice"),
        (flag::DRIFT, "Coverage drift"),
    ]
    .into_iter()
    .filter(|(bit, _)| flags & bit != 0)
    .map(|(_, label)| label)
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseColumn {
    Group,
    Release,
    Outcome,
    Availability,
}

/// Which releases to show by availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityFilter {
    Is(GameStatus),
    /// No audit against the Selection's DAT Revision says.
    Unknown,
}

/// Which groups to show by their flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagFilter {
    NeedsReview,
    Unresolved,
    Stale,
    Conflicting,
    Drift,
    Choice,
}

impl FlagFilter {
    pub const ALL: [FlagFilter; 6] = [
        FlagFilter::NeedsReview,
        FlagFilter::Unresolved,
        FlagFilter::Stale,
        FlagFilter::Conflicting,
        FlagFilter::Drift,
        FlagFilter::Choice,
    ];

    pub fn key(self) -> &'static str {
        match self {
            FlagFilter::NeedsReview => "needs-review",
            FlagFilter::Unresolved => "unresolved",
            FlagFilter::Stale => "stale",
            FlagFilter::Conflicting => "conflicting",
            FlagFilter::Drift => "drift",
            FlagFilter::Choice => "choice",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FlagFilter::NeedsReview => "Needs review",
            FlagFilter::Unresolved => "Unresolved",
            FlagFilter::Stale => "Stale choice",
            FlagFilter::Conflicting => "Conflicting choice",
            FlagFilter::Drift => "Coverage drift",
            FlagFilter::Choice => "Your choices",
        }
    }

    pub fn parse(key: &str) -> Option<FlagFilter> {
        FlagFilter::ALL.into_iter().find(|f| f.key() == key)
    }

    fn bits(self) -> u8 {
        match self {
            FlagFilter::NeedsReview => flag::NEEDS_REVIEW,
            FlagFilter::Unresolved => flag::UNRESOLVED,
            FlagFilter::Stale => flag::STALE,
            FlagFilter::Conflicting => flag::CONFLICTING,
            FlagFilter::Drift => flag::DRIFT,
            FlagFilter::Choice => flag::CHOICE,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseFilter {
    pub text: String,
    pub outcome: Option<Outcome>,
    pub availability: Option<AvailabilityFilter>,
    pub flag: Option<FlagFilter>,
}

/// A Selection's releases with the keys filtering and sorting use, computed once when it loads.
#[derive(Debug)]
pub struct SelectionIndex {
    pub selection: Selection,
    pub rows: Vec<ReleaseRow>,
    /// The release's group, as an index into `selection.groups`.
    group: Vec<u32>,
    availability: Vec<Option<GameStatus>>,
    /// Whether the release's availability rests on Claim Evidence.
    claimed: Vec<bool>,
    /// The lowercase group and title the filter searches.
    search: Vec<String>,
    /// The lowercase title, for sorting.
    title: Vec<String>,
    flags: Vec<u8>,
}

impl SelectionIndex {
    /// Indexes the Selection, looking up each release's availability by its name; `games` is empty when no Audit
    /// says, and `claimed` names the Games whose status rests on Claim Evidence.
    pub fn new(
        selection: Selection,
        games: &BTreeMap<String, GameStatus>,
        claimed: &BTreeSet<String>,
    ) -> SelectionIndex {
        let rows = selection.rows();
        let groups: BTreeMap<&str, u32> = selection
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.group.as_str(), i as u32))
            .collect();
        let n = rows.len();
        let mut claimed_of = Vec::with_capacity(n);
        let (mut group, mut availability, mut search, mut title_key, mut flags) = (
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        for row in &rows {
            let g = groups[row.group.as_str()];
            group.push(g);
            availability.push(games.get(&row.title).copied());
            claimed_of.push(claimed.contains(&row.title));
            let title = row.title.to_lowercase();
            search.push(format!("{}\n{title}", row.group.to_lowercase()));
            title_key.push(title);
            flags.push(group_flags(&selection.groups[g as usize]));
        }
        SelectionIndex {
            selection,
            rows,
            group,
            availability,
            claimed: claimed_of,
            search,
            title: title_key,
            flags,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn group_of(&self, i: usize) -> &SelectionGroup {
        &self.selection.groups[self.group[i] as usize]
    }

    pub fn availability(&self, i: usize) -> Option<GameStatus> {
        self.availability[i]
    }

    pub fn claimed(&self, i: usize) -> bool {
        self.claimed[i]
    }

    pub fn flags(&self, i: usize) -> u8 {
        self.flags[i]
    }

    /// The group named `name`, if the Selection has it.
    pub fn group(&self, name: &str) -> Option<&SelectionGroup> {
        self.selection
            .groups
            .binary_search_by(|g| g.group.as_str().cmp(name))
            .ok()
            .map(|i| &self.selection.groups[i])
    }

    /// Release indices in the order `sort` asks for, or the Selection's own order.
    pub fn sorted(&self, sort: Option<Sort<ReleaseColumn>>) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.rows.len() as u32).collect();
        let Some(sort) = sort else {
            return order;
        };
        order.sort_by(|&a, &b| {
            let (a, b) = (a as usize, b as usize);
            let ord = match sort.column {
                ReleaseColumn::Group => self.group[a].cmp(&self.group[b]),
                ReleaseColumn::Release => self.title[a].cmp(&self.title[b]),
                ReleaseColumn::Outcome => self.rows[a].outcome.cmp(&self.rows[b].outcome),
                ReleaseColumn::Availability => availability_rank(self.availability[a])
                    .cmp(&availability_rank(self.availability[b])),
            }
            .then(a.cmp(&b));
            if sort.descending { ord.reverse() } else { ord }
        });
        order
    }

    /// The releases of `order` that `filter` keeps.
    pub fn filtered(&self, order: &[u32], filter: &ReleaseFilter) -> Vec<u32> {
        let text = filter.text.trim().to_lowercase();
        order
            .iter()
            .copied()
            .filter(|&i| {
                let i = i as usize;
                filter.outcome.is_none_or(|r| self.rows[i].outcome == r)
                    && filter.availability.is_none_or(|a| match a {
                        AvailabilityFilter::Is(s) => self.availability[i] == Some(s),
                        AvailabilityFilter::Unknown => self.availability[i].is_none(),
                    })
                    && filter.flag.is_none_or(|f| self.flags[i] & f.bits() != 0)
                    && (text.is_empty() || self.search[i].contains(&text))
            })
            .collect()
    }
}

fn availability_rank(status: Option<GameStatus>) -> u8 {
    match status {
        Some(GameStatus::Complete) => 0,
        Some(GameStatus::NothingToCheck) => 1,
        Some(GameStatus::Incomplete) => 2,
        Some(GameStatus::Missing) => 3,
        None => 4,
    }
}

pub fn availability_label(status: Option<GameStatus>, claimed: bool) -> String {
    status.map_or(String::new(), |s| status_text(s, claimed))
}

/// What you have of a group's kept releases: the worst status among them, and whether any rests on claim evidence
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupAvailability {
    /// None when the latest audit says nothing about any kept release.
    pub status: Option<GameStatus>,
    pub claimed: bool,
    /// How many kept releases have the worst status, of how many are kept.
    pub worst: usize,
    pub kept: usize,
}

/// Whether a build flagged any of the group's kept releases as needing review, as far as `flags` knows.
pub fn kept_need_review(group: &SelectionGroup, flags: &FlagsNow) -> bool {
    flags.any(group.kept.iter().map(|w| w.title.as_str()))
}

impl GroupAvailability {
    pub fn of(
        group: &SelectionGroup,
        games: &BTreeMap<String, GameStatus>,
        claimed: &BTreeSet<String>,
    ) -> Self {
        // A release with Nothing to Check lacks nothing, as a Complete one does; Complete names them both when any is.
        let rank = |s: GameStatus| match s {
            GameStatus::Missing => 0,
            GameStatus::Incomplete => 1,
            GameStatus::Complete | GameStatus::NothingToCheck => 2,
        };
        let statuses: Vec<GameStatus> = group
            .kept
            .iter()
            .filter_map(|w| games.get(&w.title).copied())
            .collect();
        let worst_rank = statuses.iter().map(|s| rank(*s)).min();
        let at_worst = statuses.iter().filter(|s| Some(rank(**s)) == worst_rank);
        GroupAvailability {
            status: at_worst.clone().copied().min(),
            claimed: group.kept.iter().any(|w| claimed.contains(&w.title)),
            worst: at_worst.count(),
            kept: group.kept.len(),
        }
    }

    /// The worst status, saying how many kept releases share it when they differ.
    pub fn label(&self) -> String {
        let label = availability_label(self.status, self.claimed);
        if self.status.is_some() && self.kept > 1 && self.worst < self.kept {
            format!("{label}: {} of {} kept", self.worst, self.kept)
        } else {
            label
        }
    }
}

/// The releases checked in a group as you have staged them: your staged choice, else the engine's picks.
pub fn checked(
    profile: &SelectionProfile,
    system: &str,
    group: &SelectionGroup,
) -> BTreeSet<String> {
    profile
        .choices
        .get(system)
        .and_then(|groups| groups.get(&group.group))
        .cloned()
        .unwrap_or_else(|| group.picks())
}

/// Checks or unchecks one release of a group in the staged Profile.
pub fn toggle(
    profile: &mut SelectionProfile,
    system: &str,
    group: &SelectionGroup,
    title: &str,
    on: bool,
) {
    let mut set = checked(profile, system, group);
    if on {
        set.insert(title.to_owned());
    } else {
        set.remove(title);
    }
    profile.set_choice(
        system,
        &group.group,
        set,
        &group.picks(),
        group.engine_unresolved(),
    );
}

/// Confirms the releases checked in an unresolved group, so that it no longer needs review.
pub fn confirm(profile: &mut SelectionProfile, system: &str, group: &SelectionGroup) {
    let set = checked(profile, system, group);
    profile
        .choices
        .entry(system.to_owned())
        .or_default()
        .insert(group.group.clone(), set);
}

/// The override that includes exactly one release, by its full name.
pub fn include_override(title: &str) -> String {
    format!("|{title}")
}

/// Whether including an excluded release can win over what excluded it. The arcade selection never keeps a BIOS,
/// a device or a machine that can't run, whatever its include list says.
pub fn includable(exclusion: &LeftOutGame, arcade: bool) -> bool {
    !(arcade && exclusion.rule.stage == "structural")
}

/// Adds an include override for an excluded release to the system's settings, and checks the release. For an
/// `arcade` Selection it goes in the system's arcade include list, starting from the arcade settings that apply.
pub fn include_and_check(
    profile: &mut SelectionProfile,
    system: &str,
    group: &SelectionGroup,
    title: &str,
    arcade: bool,
) {
    let rule = include_override(title);
    let add = |include: &mut Vec<String>| {
        if !include.contains(&rule) {
            include.push(rule.clone());
        }
    };
    if arcade {
        update_arcade(profile, system, |a| add(&mut a.include));
    } else {
        add(&mut profile
            .systems
            .entry(system.to_owned())
            .or_default()
            .overrides
            .include);
    }
    let mut set = checked(profile, system, group);
    set.insert(title.to_owned());
    profile
        .choices
        .entry(system.to_owned())
        .or_default()
        .insert(group.group.clone(), set);
}

/// Whether the group list shows a group: one that holds only Dependencies, such as a BIOS or a device, is summed up
/// in the Dependency lines instead.
pub fn listed(group: &SelectionGroup) -> bool {
    !(group.kept.is_empty()
        && group.clones.is_empty()
        && group.left_out.is_empty()
        && !group.dependencies.is_empty())
}

/// How a group stands in the Selection: a choice of yours to review comes first, then a pick the engine left to
/// you, then your choice, the engine's choice, and a group whose every release is excluded.
pub fn group_status(group: &SelectionGroup) -> GroupStatus {
    match &group.choice {
        Some(c) if matches!(c.status, ChoiceStatus::Stale | ChoiceStatus::Conflicting) => {
            GroupStatus::ChoiceToReview
        }
        _ if group.unresolved => GroupStatus::Unresolved,
        Some(c) if c.status == ChoiceStatus::Applied => GroupStatus::YourChoice,
        _ if group.kept.is_empty() => GroupStatus::Excluded,
        _ => GroupStatus::Chosen,
    }
}

/// Confirms the engine's pick, or your checked releases, in every unresolved group you haven't staged a choice
/// for, and says how many it confirmed.
pub fn confirm_unresolved(
    profile: &mut SelectionProfile,
    system: &str,
    groups: &[SelectionGroup],
) -> usize {
    let mut confirmed = 0;
    for group in groups.iter().filter(|g| g.unresolved) {
        if profile
            .choices
            .get(system)
            .is_some_and(|c| c.contains_key(&group.group))
        {
            continue;
        }
        confirm(profile, system, group);
        confirmed += 1;
    }
    confirmed
}

/// The systems the Profile editor has a tab for: every system of a registered Source Set, every system the
/// Profile has settings or choices for, and the one last previewed, in name order.
pub fn system_tabs<'a>(
    profile: &SelectionProfile,
    registered: impl IntoIterator<Item = &'a str>,
    previewed: Option<&str>,
) -> Vec<String> {
    let mut tabs: BTreeSet<String> = profile
        .systems
        .keys()
        .chain(profile.choices.keys())
        .cloned()
        .collect();
    tabs.extend(registered.into_iter().map(str::to_owned));
    tabs.extend(previewed.map(str::to_owned));
    tabs.into_iter().collect()
}

/// How many groups of `system` your staged choices change, and whether its settings changed, compared with `saved`.
pub fn staged_changes(saved: &SelectionProfile, staged: &SelectionProfile, system: &str) -> usize {
    let empty = BTreeMap::new();
    let before = saved.choices.get(system).unwrap_or(&empty);
    let after = staged.choices.get(system).unwrap_or(&empty);
    let groups: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let choices = groups
        .into_iter()
        .filter(|g| before.get(*g) != after.get(*g))
        .count();
    let settings = usize::from(saved.systems.get(system) != staged.systems.get(system));
    let elsewhere = usize::from(
        saved
            .choices
            .iter()
            .any(|(s, c)| s != system && staged.choices.get(s) != Some(c))
            || staged
                .choices
                .keys()
                .any(|s| s != system && !saved.choices.contains_key(s)),
    );
    choices + settings + elsewhere
}

/// Where a system setting comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Global,
    System,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Global => "From global settings",
            Source::System => "Set for this system",
        }
    }
}

/// The system settings a system tab edits: one per field that can replace the global one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    RegionOrder,
    LanguageOrder,
    LocalizationOrder,
    VideoOrder,
    Exclude,
    Options,
}

impl Setting {
    pub fn source(self, system: &SystemSettings) -> Source {
        let set = match self {
            Setting::RegionOrder => system.region_order.is_some(),
            Setting::LanguageOrder => system.language_order.is_some(),
            Setting::LocalizationOrder => system.localization_order.is_some(),
            Setting::VideoOrder => system.video_order.is_some(),
            Setting::Exclude => system.exclude.is_some(),
            Setting::Options => system.options.is_some(),
        };
        if set { Source::System } else { Source::Global }
    }

    /// Sets the system's own value to a copy of the global one, so it can be changed for this system.
    pub fn set_for_system(self, profile: &mut SelectionProfile, system: &str) {
        let global = profile.clone();
        let own = profile.systems.entry(system.to_owned()).or_default();
        match self {
            Setting::RegionOrder => own.region_order = Some(global.region_order),
            Setting::LanguageOrder => own.language_order = Some(global.language_order),
            Setting::LocalizationOrder => own.localization_order = Some(global.localization_order),
            Setting::VideoOrder => own.video_order = Some(global.video_order),
            Setting::Exclude => own.exclude = Some(global.exclude),
            Setting::Options => own.options = Some(global.options),
        }
    }

    /// Drops the system's own value, so the global one applies.
    pub fn use_global(self, profile: &mut SelectionProfile, system: &str) {
        let Some(own) = profile.systems.get_mut(system) else {
            return;
        };
        match self {
            Setting::RegionOrder => own.region_order = None,
            Setting::LanguageOrder => own.language_order = None,
            Setting::LocalizationOrder => own.localization_order = None,
            Setting::VideoOrder => own.video_order = None,
            Setting::Exclude => own.exclude = None,
            Setting::Options => own.options = None,
        }
        if own.is_empty() {
            profile.systems.remove(system);
        }
    }
}

/// Drops every setting of a system, so the global settings apply to it. Its choices stay, and so does whether its
/// builds read its additions set.
pub fn use_global_settings(profile: &mut SelectionProfile, system: &str) {
    let additions = profile.systems.get(system).is_some_and(|s| s.additions);
    profile.systems.remove(system);
    if additions {
        profile.systems.insert(
            system.to_owned(),
            SystemSettings {
                additions,
                ..SystemSettings::default()
            },
        );
    }
}

/// Resets the global settings to the defaults, keeping system settings and choices.
pub fn reset_global(profile: &mut SelectionProfile) {
    *profile = SelectionProfile {
        systems: std::mem::take(&mut profile.systems),
        choices: std::mem::take(&mut profile.choices),
        ..SelectionProfile::default()
    };
}

/// Sets every setting of a system to the defaults, whatever the global settings say, keeping its choices. Global
/// overrides still apply: a system's overrides only add to them. Whether its builds read its additions set stays.
pub fn reset_system(profile: &mut SelectionProfile, system: &str) {
    let additions = profile.systems.get(system).is_some_and(|s| s.additions);
    profile.systems.insert(
        system.to_owned(),
        SystemSettings {
            region_order: Some(Vec::new()),
            language_order: Some(Vec::new()),
            localization_order: Some(Vec::new()),
            video_order: Some(Vec::new()),
            exclude: Some(BTreeSet::new()),
            options: Some(SelectionOptions::default()),
            replace_post_filters: true,
            additions,
            ..SystemSettings::default()
        },
    );
}

/// Moves the item at `from` to `to` in an ordered list.
pub fn move_item(list: &mut Vec<String>, from: usize, to: usize) {
    if from >= list.len() || to >= list.len() || from == to {
        return;
    }
    let item = list.remove(from);
    list.insert(to, item);
}

/// Adds `typed`, trimmed, to the end of an ordered list, unless it is blank or the list has it already by
/// [`same_name`]; whether it was added.
pub fn add_item(list: &mut Vec<String>, typed: &str) -> bool {
    let typed = typed.trim();
    if typed.is_empty() || list.iter().any(|i| same_name(i, typed)) {
        return false;
    }
    list.push(typed.to_owned());
    true
}

/// Why an unresolved group needs your decision: its discs, when the selection's picks or your choice leave a
/// multi-disc game mixed or missing a disc, otherwise a tie-break.
pub fn unresolved_explanation(group: &SelectionGroup) -> String {
    match stage_select_api::discs::unresolved_reason(group) {
        Some(reason) => {
            let mut chars = reason.chars();
            let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
            let next = if group.choice.is_some() {
                "Check the discs you want."
            } else {
                "Check the discs you want, or confirm the pick."
            };
            format!("{}{}. {next}", first.unwrap_or_default(), chars.as_str())
        }
        None => "The selection picked this by a tie-break, so it needs your decision. Check the games you want, \
                 or confirm the pick."
            .to_string(),
    }
}

/// The lines of a line editor: trimmed, with blank lines dropped.
pub fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::arcade::ArcadeSettings;
    use stage_select_api::selection::{
        Category, CloneGame, CoverageDrift, Dependency, GroupChoice, KeptGame, LeftOutGame, Rule,
        SelectionOptions, SelectionPins, SelectionSummary,
    };

    fn rule() -> Rule {
        Rule {
            stage: "Parent selection".into(),
            reference: Some("REF0001".into()),
            message: "Higher region".into(),
        }
    }

    fn group(name: &str, winner: &str, clones: &[&str], excluded: &[&str]) -> SelectionGroup {
        SelectionGroup {
            dependencies: Vec::new(),
            group: name.into(),
            unresolved: false,
            kept: vec![KeptGame {
                title: winner.into(),
                rule: rule(),
                unresolved: false,
            }],
            clones: clones
                .iter()
                .map(|t| CloneGame {
                    title: (*t).into(),
                    clone_of: winner.into(),
                    rule: rule(),
                })
                .collect(),
            left_out: excluded
                .iter()
                .map(|t| LeftOutGame {
                    title: (*t).into(),
                    reason: "Demo".into(),
                    rule: rule(),
                })
                .collect(),
            choice: None,
            drift: false,
        }
    }

    #[test]
    fn an_unresolved_group_of_discs_explains_which_releases_its_discs_come_from() {
        let mut saga = group(
            "saga",
            "Saga (USA) (Disc 1)",
            &["Saga (Europe) (Disc 1)"],
            &[],
        );
        saga.kept.push(KeptGame {
            title: "Saga (Europe) (Disc 2)".into(),
            rule: rule(),
            unresolved: false,
        });
        saga.unresolved = true;
        assert_eq!(
            unresolved_explanation(&saga),
            "Its selected discs come from different versions: disc 1 from Saga (USA), disc 2 from Saga (Europe). \
             Check the discs you want, or confirm the pick."
        );
        let partial = BTreeSet::from(["Saga (Europe) (Disc 1)".to_owned()]);
        saga.choice = Some(GroupChoice {
            checked: partial,
            status: ChoiceStatus::Applied,
            picks: saga.kept.iter().map(|w| w.title.clone()).collect(),
            engine_unresolved: true,
            problems: vec![],
        });
        assert_eq!(
            unresolved_explanation(&saga),
            "Your choice doesn't cover disc 2. Check the discs you want."
        );
        let mut tie = group("pong", "Pong (USA)", &["Pong (Europe)"], &[]);
        tie.unresolved = true;
        assert!(
            unresolved_explanation(&tie).starts_with("The selection picked this by a tie-break")
        );
    }

    #[test]
    fn a_group_shows_a_choice_to_review_before_an_unresolved_pick_then_yours_the_engines_and_none()
    {
        let applied = |status| GroupChoice {
            checked: BTreeSet::from(["Pong (USA)".to_owned()]),
            status,
            picks: vec!["Pong (USA)".into()],
            engine_unresolved: false,
            problems: Vec::new(),
        };
        let mut review = group("pong", "Pong (USA)", &[], &[]);
        review.unresolved = true;
        review.choice = Some(applied(ChoiceStatus::Stale));
        let mut unresolved = group("pong", "Pong (USA)", &[], &[]);
        unresolved.unresolved = true;
        let mut yours = group("pong", "Pong (USA)", &[], &[]);
        yours.choice = Some(applied(ChoiceStatus::Applied));
        let mut matching = group("pong", "Pong (USA)", &[], &[]);
        matching.choice = Some(applied(ChoiceStatus::MatchesDefault));
        let mut excluded = group("demo", "Demo (USA)", &[], &["Demo (USA)"]);
        excluded.kept.clear();

        let statuses: Vec<GroupStatus> = [review, unresolved, yours, matching, excluded]
            .iter()
            .map(group_status)
            .collect();

        assert_eq!(
            statuses,
            [
                GroupStatus::ChoiceToReview,
                GroupStatus::Unresolved,
                GroupStatus::YourChoice,
                GroupStatus::Chosen,
                GroupStatus::Excluded,
            ]
        );
    }

    #[test]
    fn confirming_every_unresolved_pick_keeps_the_choices_you_staged() {
        let mut tie = group("tie", "Tie Alpha (Europe)", &["Tie Beta (Europe)"], &[]);
        tie.unresolved = true;
        let mut staged = group("staged", "Staged (USA)", &["Staged (Japan)"], &[]);
        staged.unresolved = true;
        let settled = group("settled", "Settled (USA)", &[], &[]);
        let mut profile = SelectionProfile::default();
        toggle(&mut profile, "Atari", &staged, "Staged (Japan)", true);

        let confirmed = confirm_unresolved(&mut profile, "Atari", &[tie, staged, settled]);

        assert_eq!(confirmed, 1);
        let choices = &profile.choices["Atari"];
        assert_eq!(
            choices.keys().collect::<Vec<_>>(),
            ["staged", "tie"],
            "the settled group needs no choice"
        );
        assert_eq!(
            choices["tie"],
            BTreeSet::from(["Tie Alpha (Europe)".to_owned()])
        );
        assert_eq!(
            choices["staged"],
            BTreeSet::from(["Staged (USA)".to_owned(), "Staged (Japan)".to_owned()])
        );
    }

    #[test]
    fn the_editor_has_a_tab_for_every_registered_system_before_any_preview() {
        let mut profile = SelectionProfile::default();
        profile
            .systems
            .insert("Atari".into(), SystemSettings::default());
        profile
            .choices
            .entry("Coleco".into())
            .or_default()
            .insert("g".into(), BTreeSet::new());

        let tabs = system_tabs(
            &profile,
            ["Nintendo - Game Boy Advance (No-Intro)", "Atari"],
            None,
        );

        assert_eq!(
            tabs,
            ["Atari", "Coleco", "Nintendo - Game Boy Advance (No-Intro)"]
        );
        assert_eq!(
            system_tabs(&SelectionProfile::default(), [], Some("Previewed")),
            ["Previewed"]
        );
    }

    fn selection(groups: Vec<SelectionGroup>) -> Selection {
        Selection {
            pins: SelectionPins::default(),
            dat_sha256: String::new(),
            system: "Atari".into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups,
            stale_choices: BTreeMap::new(),
        }
    }

    fn index() -> SelectionIndex {
        let mut combat = group("combat", "Combat (USA)", &[], &[]);
        combat.choice = Some(GroupChoice {
            checked: BTreeSet::new(),
            status: ChoiceStatus::Conflicting,
            picks: vec!["Combat (USA)".into()],
            engine_unresolved: false,
            problems: vec![],
        });
        let mut maze = group("maze", "Maze (USA)", &[], &[]);
        maze.unresolved = true;
        maze.drift = true;
        let games = BTreeMap::from([
            ("Pong (USA)".to_string(), GameStatus::Missing),
            ("Pong (Europe)".to_string(), GameStatus::Complete),
        ]);
        SelectionIndex::new(
            selection(vec![
                combat,
                maze,
                group(
                    "pong",
                    "Pong (USA)",
                    &["Pong (Europe)"],
                    &["Pong (USA) (Demo)"],
                ),
            ]),
            &games,
            &BTreeSet::from(["Pong (Europe)".to_string()]),
        )
    }

    #[test]
    fn availability_resting_on_claims_says_so() {
        let index = index();
        let at = |title: &str| index.rows.iter().position(|r| r.title == title).unwrap();

        assert!(index.claimed(at("Pong (Europe)")));
        assert!(!index.claimed(at("Pong (USA)")));
        assert_eq!(
            availability_label(Some(GameStatus::Complete), true),
            "Complete (claim evidence only)"
        );
        assert_eq!(
            availability_label(Some(GameStatus::Missing), false),
            "Missing"
        );
        assert_eq!(availability_label(None, false), "");
    }

    fn titles(index: &SelectionIndex, shown: &[u32]) -> Vec<String> {
        shown
            .iter()
            .map(|&i| index.rows[i as usize].title.clone())
            .collect()
    }

    #[test]
    fn releases_filter_by_text_role_availability_and_flag() {
        let index = index();
        let all = index.sorted(None);
        let only = |filter: ReleaseFilter| titles(&index, &index.filtered(&all, &filter));

        assert_eq!(
            only(ReleaseFilter {
                text: "PONG".into(),
                outcome: Some(Outcome::Clone),
                ..Default::default()
            }),
            ["Pong (Europe)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                availability: Some(AvailabilityFilter::Is(GameStatus::Missing)),
                ..Default::default()
            }),
            ["Pong (USA)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                flag: Some(FlagFilter::NeedsReview),
                ..Default::default()
            }),
            ["Combat (USA)", "Maze (USA)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                flag: Some(FlagFilter::Drift),
                ..Default::default()
            }),
            ["Maze (USA)"]
        );
        assert_eq!(
            only(ReleaseFilter {
                availability: Some(AvailabilityFilter::Unknown),
                text: "maze".into(),
                ..Default::default()
            }),
            ["Maze (USA)"]
        );
    }

    #[test]
    fn releases_sort_by_availability_then_their_place() {
        let index = index();
        let order = index.sorted(Some(Sort {
            column: ReleaseColumn::Availability,
            descending: false,
        }));
        assert_eq!(titles(&index, &order[..2]), ["Pong (Europe)", "Pong (USA)"]);
    }

    #[test]
    fn checking_stages_a_choice_and_checking_the_picks_again_drops_it() {
        let index = index();
        let pong = index.group("pong").unwrap();
        let saved = SelectionProfile::default();
        let mut staged = saved.clone();

        toggle(&mut staged, "Atari", pong, "Pong (Europe)", true);
        toggle(&mut staged, "Atari", pong, "Pong (USA)", false);
        assert_eq!(
            checked(&staged, "Atari", pong),
            BTreeSet::from(["Pong (Europe)".to_string()])
        );
        assert_eq!(staged_changes(&saved, &staged, "Atari"), 1);

        toggle(&mut staged, "Atari", pong, "Pong (USA)", true);
        toggle(&mut staged, "Atari", pong, "Pong (Europe)", false);
        assert_eq!(staged, saved, "back to the engine's picks");
    }

    #[test]
    fn confirming_an_unresolved_group_stores_its_picks() {
        let index = index();
        let maze = index.group("maze").unwrap();
        let mut staged = SelectionProfile::default();
        confirm(&mut staged, "Atari", maze);
        assert_eq!(
            staged.choices["Atari"]["maze"],
            BTreeSet::from(["Maze (USA)".to_string()])
        );
    }

    #[test]
    fn including_an_excluded_release_adds_an_override_and_checks_it() {
        let index = index();
        let pong = index.group("pong").unwrap();
        let mut staged = SelectionProfile::default();
        include_and_check(&mut staged, "Atari", pong, "Pong (USA) (Demo)", false);
        include_and_check(&mut staged, "Atari", pong, "Pong (USA) (Demo)", false);
        assert_eq!(
            staged.systems["Atari"].overrides.include,
            ["|Pong (USA) (Demo)"]
        );
        assert!(staged.systems["Atari"].arcade.is_none());
        assert!(staged.choices["Atari"]["pong"].contains("Pong (USA) (Demo)"));
        assert!(staged.choices["Atari"]["pong"].contains("Pong (USA)"));
    }

    fn arcade_rule(stage: &str, message: &str) -> Rule {
        Rule {
            stage: stage.into(),
            reference: None,
            message: message.into(),
        }
    }

    fn racer() -> SelectionGroup {
        SelectionGroup {
            group: "racer".into(),
            unresolved: false,
            kept: vec![KeptGame {
                title: "racera".into(),
                rule: arcade_rule(
                    "election",
                    "the only machine its filters left in its family",
                ),
                unresolved: false,
            }],
            clones: Vec::new(),
            left_out: vec![LeftOutGame {
                title: "racer".into(),
                rule: arcade_rule("quality", "driver status preliminary, below imperfect"),
                reason: "driver status".into(),
            }],
            dependencies: Vec::new(),
            choice: None,
            drift: false,
        }
    }

    #[test]
    fn including_an_excluded_arcade_machine_adds_it_to_the_arcade_include_list() {
        let mut staged = SelectionProfile {
            arcade: Some(ArcadeSettings {
                drop_samples: true,
                ..ArcadeSettings::default()
            }),
            ..SelectionProfile::default()
        };
        include_and_check(&mut staged, "MAME", &racer(), "racer", true);
        include_and_check(&mut staged, "MAME", &racer(), "racer", true);
        let own = staged.systems["MAME"].arcade.as_ref().unwrap();
        assert_eq!(own.include, ["|racer"]);
        assert!(own.drop_samples, "it starts from the settings that applied");
        assert!(staged.systems["MAME"].overrides.include.is_empty());
        assert_eq!(
            staged.choices["MAME"]["racer"],
            BTreeSet::from(["racer".to_string(), "racera".to_string()])
        );
    }

    #[test]
    fn a_group_holding_only_dependencies_is_left_out_of_the_group_list() {
        let mut z80 = racer();
        z80.group = "z80".into();
        z80.kept.clear();
        z80.left_out.clear();
        z80.dependencies = vec![Dependency {
            title: "z80".into(),
            needed_by: vec!["racera".into()],
            rule: arcade_rule("dependency", "needed by racera"),
        }];
        assert!(!listed(&z80));
        assert!(listed(&racer()));
        let mut excluded = racer();
        excluded.kept.clear();
        assert!(
            listed(&excluded),
            "a family whose every machine is excluded"
        );
        let mut both = racer();
        both.dependencies = z80.dependencies.clone();
        assert!(
            listed(&both),
            "a family one of whose machines another needs"
        );
    }

    #[test]
    fn only_an_exclusion_your_include_list_can_win_over_offers_include_and_check() {
        let group = racer();
        let quality = &group.left_out[0];
        assert!(includable(quality, true));
        assert!(includable(quality, false));
        let bios = LeftOutGame {
            title: "acmebios".into(),
            rule: arcade_rule("structural", "a BIOS, which isn't played on its own"),
            reason: "BIOS".into(),
        };
        assert!(!includable(&bios, true));
    }

    #[test]
    fn a_system_setting_comes_from_the_global_settings_until_set_for_the_system() {
        let mut profile = SelectionProfile {
            region_order: vec!["USA".into()],
            ..Default::default()
        };
        let source = |p: &SelectionProfile| {
            Setting::RegionOrder
                .source(p.systems.get("Atari").unwrap_or(&SystemSettings::default()))
        };
        assert_eq!(source(&profile), Source::Global);

        Setting::RegionOrder.set_for_system(&mut profile, "Atari");
        assert_eq!(source(&profile), Source::System);
        assert_eq!(
            profile.systems["Atari"].region_order.as_deref(),
            Some(&["USA".to_string()][..])
        );
        Setting::RegionOrder.use_global(&mut profile, "Atari");
        assert!(profile.systems.is_empty(), "nothing of its own is left");

        Setting::Exclude.set_for_system(&mut profile, "Atari");
        use_global_settings(&mut profile, "Atari");
        assert!(profile.systems.is_empty());
    }

    #[test]
    fn dropping_or_resetting_a_systems_settings_keeps_its_additions_opt_in() {
        let mut profile = SelectionProfile::default();
        Setting::Exclude.set_for_system(&mut profile, "Atari");
        profile.systems.get_mut("Atari").unwrap().additions = true;
        use_global_settings(&mut profile, "Atari");
        assert_eq!(
            profile.systems["Atari"],
            SystemSettings {
                additions: true,
                ..SystemSettings::default()
            }
        );
        reset_system(&mut profile, "Atari");
        assert!(profile.systems["Atari"].additions);
        profile.systems.get_mut("Atari").unwrap().additions = false;
        use_global_settings(&mut profile, "Atari");
        assert!(profile.systems.is_empty());
    }

    #[test]
    fn resetting_the_global_settings_keeps_system_settings_and_choices() {
        let mut profile = SelectionProfile {
            region_order: vec!["USA".into()],
            ..Default::default()
        };
        Setting::Exclude.set_for_system(&mut profile, "Atari");
        profile.set_choice("Atari", "pong", BTreeSet::new(), &BTreeSet::new(), true);
        reset_global(&mut profile);
        assert!(profile.region_order.is_empty());
        assert!(profile.systems.contains_key("Atari"));
        assert!(profile.choices.contains_key("Atari"));
    }

    #[test]
    fn resetting_a_system_to_the_defaults_ignores_the_global_settings_and_keeps_choices() {
        let mut profile = SelectionProfile {
            region_order: vec!["Japan".into()],
            exclude: [Category::Demos].into(),
            post_filters: vec!["/USA/".into()],
            ..Default::default()
        };
        Setting::VideoOrder.set_for_system(&mut profile, "Atari");
        profile.systems.get_mut("Atari").unwrap().video_order = Some(vec!["PAL".into()]);
        profile.set_choice("Atari", "pong", BTreeSet::new(), &BTreeSet::new(), true);
        reset_system(&mut profile, "Atari");
        let own = &profile.systems["Atari"];
        assert_eq!(own.region_order, Some(vec![]), "the default order");
        assert_eq!(own.video_order, Some(vec![]), "the default order");
        assert_eq!(own.exclude, Some(BTreeSet::new()));
        assert_eq!(own.options, Some(SelectionOptions::default()));
        assert!(own.replace_post_filters && own.post_filters.is_empty());
        assert!(profile.choices.contains_key("Atari"));
        assert_eq!(profile.region_order, ["Japan"], "the global settings stay");
    }

    #[test]
    fn ordered_lists_move_items_and_line_editors_drop_blank_lines() {
        let mut list = vec!["USA".to_string(), "Europe".into(), "Japan".into()];
        move_item(&mut list, 2, 0);
        assert_eq!(list, ["Japan", "USA", "Europe"]);
        move_item(&mut list, 0, 5);
        assert_eq!(list, ["Japan", "USA", "Europe"]);
        // An item you type is trimmed, and one already listed in any case, or blank, isn't added.
        assert!(add_item(&mut list, " Russian "));
        assert!(!add_item(&mut list, "usa"));
        assert!(!add_item(&mut list, "  "));
        assert_eq!(list, ["Japan", "USA", "Europe", "Russian"]);
        assert_eq!(
            lines(" |Pong (USA) \n\n/Demo/\n"),
            ["|Pong (USA)", "/Demo/"]
        );
    }

    #[test]
    fn a_groups_availability_is_its_worst_kept_release() {
        let mut g = group("golf", "Golf (USA)", &[], &[]);
        g.kept.push(KeptGame {
            title: "Golf (Europe)".into(),
            rule: rule(),
            unresolved: false,
        });
        let games = BTreeMap::from([
            ("Golf (USA)".to_string(), GameStatus::Complete),
            ("Golf (Europe)".to_string(), GameStatus::Missing),
        ]);
        let none = BTreeSet::new();

        let mixed = GroupAvailability::of(&g, &games, &none);
        assert_eq!(mixed.status, Some(GameStatus::Missing));
        assert_eq!(
            mixed.label(),
            format!(
                "{}: 1 of 2 kept",
                availability_label(Some(GameStatus::Missing), false)
            )
        );

        let both = BTreeMap::from([
            ("Golf (USA)".to_string(), GameStatus::Complete),
            ("Golf (Europe)".to_string(), GameStatus::Complete),
        ]);
        let claimed = BTreeSet::from(["Golf (Europe)".to_string()]);
        let held = GroupAvailability::of(&g, &both, &claimed);
        assert_eq!(held.status, Some(GameStatus::Complete));
        assert!(
            held.claimed,
            "a second kept release on claim evidence alone shows"
        );
        assert_eq!(
            held.label(),
            availability_label(Some(GameStatus::Complete), true)
        );

        let device = BTreeMap::from([
            ("Golf (USA)".to_string(), GameStatus::NothingToCheck),
            ("Golf (Europe)".to_string(), GameStatus::Complete),
        ]);
        let lacking_nothing = GroupAvailability::of(&g, &device, &none);
        assert_eq!(lacking_nothing.status, Some(GameStatus::Complete));
        assert_eq!(
            lacking_nothing.label(),
            availability_label(Some(GameStatus::Complete), false),
            "a release with nothing to check falls short of nothing"
        );

        let unaudited = GroupAvailability::of(&g, &BTreeMap::new(), &none);
        assert_eq!(unaudited.label(), "");
    }

    #[test]
    fn a_group_needs_review_when_a_build_flagged_a_release_it_keeps() {
        let flag = |game: &str| stage_select_api::ReviewFlag {
            game: game.into(),
            path: format!(".stage-select/placed/7/{game}.zip"),
            reason: "its CRC32 differs from the DAT's".into(),
            build: 3,
            raised_at: "t".into(),
        };
        let g = group("golf", "Golf (USA)", &["Golf (Japan)"], &[]);
        let read = |game: &str| {
            FlagsNow::default().next(
                Some(&crate::placement_model::stamped_flags(
                    1,
                    Ok(vec![flag(game)]),
                )),
                1,
            )
        };
        assert!(kept_need_review(&g, &read("Golf (USA)")));
        assert!(
            !kept_need_review(&g, &read("Golf (Japan)")),
            "only a kept release counts"
        );
        assert!(
            !kept_need_review(&g, &FlagsNow::default()),
            "nothing read yet"
        );
    }

    #[test]
    fn a_release_outside_the_coverage_says_so_once() {
        let outside = LeftOutGame {
            title: "Pong (Japan)".into(),
            rule: Rule::coverage(),
            reason: stage_select_api::selection::COVERAGE_MESSAGE.into(),
        };
        assert_eq!(left_out_text(&outside), "Outside its source set's coverage");

        let demo = LeftOutGame {
            title: "Pong (Demo)".into(),
            rule: rule(),
            reason: "Demos".into(),
        };
        assert_eq!(
            left_out_text(&demo),
            "Demos: Parent selection: Higher region"
        );
        assert_eq!(rule_text(&Rule::choice()), "Your choice");
        let left_out = Rule {
            stage: "Exclusions".into(),
            reference: None,
            message: "Demos".into(),
        };
        assert_eq!(rule_text(&left_out), "Left out: Demos");
        let sole = Rule {
            stage: "Parent selection".into(),
            reference: None,
            message: "No other release competed".into(),
        };
        assert_eq!(
            rule_text(&sole),
            "Parent selection: No other version competed"
        );
    }
}
