//! Coverage in the workbench, without rendering (ADR 0021): its label, the editor's choices grouped like the presets,
//! and the one-line summaries of what a Coverage leaves out and what changing it would do.
//!
//! The presets and the covering rule are the API crate's (`title_select_api::coverage`), so the workbench reads a
//! Coverage as the Service stores it.

use std::collections::BTreeSet;

use title_select_api::coverage::{PRESETS, Preset, UNKNOWN, WORLD};
use title_select_api::{CoveragePreview, GameStatus, OutsideCoverage, RegionCount};

/// What a Coverage is called: "Every region" without one; the names of the presets it is made of, joined
/// ("Asia + Europe + Australia"); else how many regions it names, as after a hand edit.
pub fn label(coverage: Option<&[String]>) -> String {
    let Some(regions) = coverage else {
        return "Every region".to_owned();
    };
    // World is covered by every Coverage, so naming it changes nothing.
    let chosen: BTreeSet<&str> = regions
        .iter()
        .map(String::as_str)
        .filter(|r| *r != WORLD)
        .collect();
    let mut names: Vec<&str> = PRESETS
        .iter()
        .filter(|p| p.regions.iter().all(|r| chosen.contains(r)))
        .map(|p| p.name)
        .collect();
    let union: BTreeSet<&str> = PRESETS
        .iter()
        .filter(|p| names.contains(&p.name))
        .flat_map(|p| p.regions.iter().copied())
        .collect();
    if !names.is_empty() && union == chosen {
        names.sort_unstable();
        return names.join(" + ");
    }
    match chosen.len() {
        0 => "World only".to_owned(),
        1 => "1 region".to_owned(),
        n => format!("{n} regions"),
    }
}

/// The label as it reads inside a sentence: "every region" in lowercase, a preset's name as it is.
pub fn label_in_sentence(coverage: Option<&[String]>) -> String {
    match coverage {
        None => "every region".to_owned(),
        Some(_) => label(coverage),
    }
}

/// Every region a Coverage names, for a tooltip: World is always covered, so it is said once at the end.
pub fn regions_line(coverage: Option<&[String]>) -> String {
    match coverage {
        None => "Every region: every game of the DAT is covered.".to_owned(),
        Some(regions) => {
            let named: Vec<&str> = regions
                .iter()
                .map(|r| region_name(r))
                .filter(|r| *r != WORLD)
                .collect();
            match named.as_slice() {
                [] => "World.".to_owned(),
                [one] => format!("{one} and World."),
                _ => format!("{}, and World.", named.join(", ")),
            }
        }
    }
}

/// The coverage a selection was made under, in one line: what it chose among.
pub fn made_under(coverage: Option<&[String]>) -> String {
    match coverage {
        None => "Every region: it chose among every game.".to_owned(),
        Some(_) => format!(
            "{}: it chose only among games of {}",
            label(coverage),
            regions_line(coverage)
        ),
    }
}

/// How a region is shown: the Unknown region says what it means.
pub fn region_name(region: &str) -> &str {
    if region == UNKNOWN {
        "Unknown (no region tag)"
    } else {
        region
    }
}

/// A group of the editor's region checklist: a preset's regions, or the regions in no preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: &'static str,
    pub regions: Vec<String>,
}

/// The name of the group of regions in no preset.
pub const OTHER_REGIONS: &str = "Other regions";

/// The engine's region names `vocabulary` grouped like the presets, in the presets' order, then the regions in no
/// preset in `vocabulary`'s order, Unknown among them. World is left out: every Coverage covers it.
pub fn groups(vocabulary: &[String]) -> Vec<Group> {
    let mut groups: Vec<Group> = PRESETS
        .iter()
        .map(|p| Group {
            name: p.name,
            regions: p
                .regions
                .iter()
                .filter(|r| vocabulary.iter().any(|v| v == *r))
                .map(|r| (*r).to_owned())
                .collect(),
        })
        .filter(|g| !g.regions.is_empty())
        .collect();
    let other: Vec<String> = vocabulary
        .iter()
        .filter(|r| r.as_str() != WORLD)
        .filter(|r| !PRESETS.iter().any(|p| p.regions.contains(&r.as_str())))
        .cloned()
        .collect();
    if !other.is_empty() {
        groups.push(Group {
            name: OTHER_REGIONS,
            regions: other,
        });
    }
    groups
}

/// Whether a preset's checkbox is ticked: all of its regions are chosen, some are (shown indeterminate), or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    All,
    Some,
    None,
}

/// A Coverage being edited: every region, or only the regions chosen. The regions chosen are kept while you choose
/// every region, so choosing only some again brings them back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub every: bool,
    pub regions: BTreeSet<String>,
}

impl Draft {
    /// The draft of a stored Coverage.
    pub fn of(coverage: Option<&[String]>) -> Draft {
        Draft {
            every: coverage.is_none(),
            regions: coverage.unwrap_or_default().iter().cloned().collect(),
        }
    }

    /// Every region: no Coverage.
    pub fn every() -> Draft {
        Draft {
            every: true,
            regions: BTreeSet::new(),
        }
    }

    pub fn tick(&self, preset: &Preset) -> Tick {
        let chosen = preset
            .regions
            .iter()
            .filter(|r| self.regions.contains(**r))
            .count();
        match chosen {
            0 => Tick::None,
            n if n == preset.regions.len() => Tick::All,
            _ => Tick::Some,
        }
    }

    /// Chooses every region of `preset`, or none of them.
    pub fn set_preset(&mut self, preset: &Preset, on: bool) {
        for region in preset.regions {
            self.set_region(region, on);
        }
    }

    pub fn set_region(&mut self, region: &str, on: bool) {
        if on {
            self.regions.insert(region.to_owned());
        } else {
            self.regions.remove(region);
        }
    }

    /// The Coverage to save, its regions in `order` (the engine's region names) and any others after them: `None`
    /// for every region. Refused when only some regions are to be covered and none is chosen.
    pub fn coverage(&self, order: &[String]) -> Result<Option<Vec<String>>, String> {
        if self.every {
            return Ok(None);
        }
        if self.regions.is_empty() {
            return Err(
                "Choose at least one region, or choose every region to cover every game.".into(),
            );
        }
        let mut regions: Vec<String> = order
            .iter()
            .filter(|r| self.regions.contains(*r))
            .cloned()
            .collect();
        regions.extend(self.regions.iter().filter(|r| !order.contains(r)).cloned());
        Ok(Some(regions))
    }

    /// What the draft is called, as a saved Coverage would be.
    pub fn label(&self) -> String {
        if self.every {
            return label(None);
        }
        let regions: Vec<String> = self.regions.iter().cloned().collect();
        label(Some(&regions))
    }
}

fn games(n: usize) -> &'static str {
    if n == 1 { "game" } else { "games" }
}

/// What saving a Coverage would change, in one line: the Missing games it covers before and after, and how many
/// library folders would become stale.
pub fn preview_line(preview: &CoveragePreview) -> String {
    let missing = match (preview.missing_before, preview.missing_after) {
        (Some(before), Some(after)) if before == after => {
            format!(
                "Missing in your coverage: {before} {}, unchanged.",
                games(before)
            )
        }
        (Some(before), Some(after)) => {
            format!("Missing in your coverage: {before} now, {after} after saving.")
        }
        _ => "No missing count until its first audit.".to_owned(),
    };
    let stale = match preview.stale_library_folders.len() {
        0 => "No library folder will become stale.".to_owned(),
        1 => "1 library folder will become stale.".to_owned(),
        n => format!("{n} library folders will become stale."),
    };
    format!("{missing} {stale}")
}

/// A Coverage to save, or why the draft can't be saved: what [`Draft::coverage`] gives.
pub type Proposal = Result<Option<Vec<String>>, String>;

/// The line under the editor: why the draft can't be saved; else the preview made for the draft as it is now, or,
/// while that preview is still on its way, that it is coming. `made` is the latest preview to arrive and the proposal
/// it was made for: one made for an earlier draft is never shown.
pub fn preview_text(
    now: &Proposal,
    made: Option<&(Proposal, Result<CoveragePreview, String>)>,
) -> String {
    if let Err(why) = now {
        return why.clone();
    }
    match made {
        Some((made_for, Ok(preview))) if made_for == now => preview_line(preview),
        Some((made_for, Err(problem))) if made_for == now => problem.clone(),
        _ => "Previewing what saving changes…".to_owned(),
    }
}

/// What a Coverage leaves out of a source set's counts, in one line: games present, incomplete, and of an unknown
/// region.
pub fn outside_line(outside: &OutsideCoverage) -> String {
    let n = |s: GameStatus| outside.games.get(&s).copied().unwrap_or(0);
    let present = n(GameStatus::Complete);
    format!(
        "Outside your coverage: {present} {} present, {} incomplete, {} with an unknown region. You keep them; none \
         counts as missing.",
        games(present),
        n(GameStatus::Incomplete),
        outside.unknown_region,
    )
}

/// The rows of the by-region table: the regions your Coverage covers, or every region with `all`.
pub fn region_rows(regions: &[RegionCount], all: bool) -> Vec<RegionCount> {
    regions
        .iter()
        .filter(|r| all || r.covered)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use title_select_api::CoverageStaleFolder;
    use title_select_api::coverage::preset;

    use super::*;

    fn regions(names: &[&str]) -> Vec<String> {
        names.iter().map(|r| (*r).to_owned()).collect()
    }

    fn of(presets: &[&str]) -> Vec<String> {
        presets
            .iter()
            .flat_map(|name| preset(name).unwrap().regions.iter())
            .map(|r| (*r).to_owned())
            .collect()
    }

    #[test]
    fn a_coverage_made_of_presets_is_named_by_them_and_a_hand_edit_by_its_count() {
        assert_eq!(label(None), "Every region");
        assert_eq!(label(Some(&of(&["USA + Canada"]))), "USA + Canada");
        assert_eq!(
            label(Some(&of(&["Europe + Australia", "Asia"]))),
            "Asia + Europe + Australia"
        );

        let mut edited = of(&["Europe + Australia"]);
        edited.retain(|r| r != "Israel");
        assert_eq!(label(Some(&edited)), "39 regions");
        edited = of(&["USA + Canada"]);
        edited.push(UNKNOWN.to_owned());
        assert_eq!(label(Some(&edited)), "3 regions");
        assert_eq!(label(Some(&regions(&["Japan"]))), "1 region");
    }

    #[test]
    fn inside_a_sentence_every_region_is_lowercase() {
        assert_eq!(label_in_sentence(None), "every region");
        assert_eq!(label_in_sentence(Some(&of(&["Asia"]))), "Asia");
    }

    #[test]
    fn the_full_list_names_the_unknown_region_and_ends_with_world() {
        assert_eq!(
            regions_line(Some(&regions(&["USA", "Canada", "Unknown"]))),
            "USA, Canada, Unknown (no region tag), and World."
        );
    }

    #[test]
    fn regions_are_grouped_like_the_presets_with_the_rest_after_them_and_world_left_out() {
        let vocabulary = regions(&["USA", "World", "Japan", "Europe", "Atlantis", "Unknown"]);

        let groups = groups(&vocabulary);

        let names: Vec<&str> = groups.iter().map(|g| g.name).collect();
        assert_eq!(
            names,
            ["USA + Canada", "Europe + Australia", "Asia", OTHER_REGIONS]
        );
        assert_eq!(groups[0].regions, ["USA"]);
        assert_eq!(groups[3].regions, ["Atlantis", "Unknown"]);
    }

    #[test]
    fn a_preset_is_ticked_when_all_its_regions_are_chosen_and_indeterminate_when_some_are() {
        let asia = preset("Asia").unwrap();
        let mut draft = Draft::of(Some(&regions(&["USA"])));
        assert_eq!(draft.tick(asia), Tick::None);

        draft.set_preset(asia, true);
        assert_eq!(draft.tick(asia), Tick::All);
        assert_eq!(draft.label(), "12 regions", "USA alone is no preset");

        draft.set_region("Japan", false);
        assert_eq!(draft.tick(asia), Tick::Some);
        draft.set_preset(asia, false);
        assert_eq!(draft.regions, BTreeSet::from(["USA".to_owned()]));
    }

    #[test]
    fn a_draft_saves_in_the_engines_order_and_keeps_its_regions_while_every_region_is_chosen() {
        let order = regions(&["USA", "Europe", "Japan"]);
        let mut draft = Draft::of(Some(&regions(&["Japan", "USA"])));
        assert_eq!(draft.coverage(&order), Ok(Some(regions(&["USA", "Japan"]))));

        draft.every = true;
        assert_eq!(draft.coverage(&order), Ok(None));
        assert_eq!(draft.label(), "Every region");
        draft.every = false;
        assert_eq!(draft.coverage(&order), Ok(Some(regions(&["USA", "Japan"]))));
    }

    #[test]
    fn only_some_regions_with_none_chosen_is_refused() {
        let draft = Draft {
            every: false,
            regions: BTreeSet::new(),
        };
        assert!(
            draft
                .coverage(&[])
                .unwrap_err()
                .starts_with("Choose at least one region")
        );
    }

    fn preview(before: Option<usize>, after: Option<usize>, folders: usize) -> CoveragePreview {
        CoveragePreview {
            coverage: None,
            audit: before.map(|_| 4),
            missing_before: before,
            missing_after: after,
            stale_library_folders: (0..folders as i64)
                .map(|id| CoverageStaleFolder {
                    id,
                    root: "lib".into(),
                    path: format!("f{id}"),
                })
                .collect(),
        }
    }

    #[test]
    fn the_preview_says_missing_before_and_after_and_the_folders_that_go_stale() {
        assert_eq!(
            preview_line(&preview(Some(1204), Some(312), 2)),
            "Missing in your coverage: 1204 now, 312 after saving. 2 library folders will become stale."
        );
        assert_eq!(
            preview_line(&preview(Some(1), Some(1), 1)),
            "Missing in your coverage: 1 game, unchanged. 1 library folder will become stale."
        );
        assert_eq!(
            preview_line(&preview(None, None, 0)),
            "No missing count until its first audit. No library folder will become stale."
        );
    }

    #[test]
    fn outside_the_coverage_counts_present_incomplete_and_unknown_region_games() {
        let outside = OutsideCoverage {
            games: BTreeMap::from([
                (GameStatus::Complete, 12),
                (GameStatus::Incomplete, 3),
                (GameStatus::Missing, 900),
            ]),
            unknown_region: 40,
        };
        assert_eq!(
            outside_line(&outside),
            "Outside your coverage: 12 games present, 3 incomplete, 40 with an unknown region. You keep them; none \
             counts as missing."
        );
    }

    #[test]
    fn the_region_table_shows_covered_regions_unless_you_ask_for_all() {
        let row = |region: &str, covered: bool| RegionCount {
            region: region.into(),
            covered,
            games: BTreeMap::new(),
        };
        let rows = [row("USA", true), row("Japan", false), row("World", true)];

        let shown: Vec<String> = region_rows(&rows, false)
            .into_iter()
            .map(|r| r.region)
            .collect();
        assert_eq!(shown, ["USA", "World"]);
        assert_eq!(region_rows(&rows, true).len(), 3);
    }

    #[test]
    fn a_selection_says_what_it_chose_among() {
        assert_eq!(made_under(None), "Every region: it chose among every game.");
        let regions = vec!["Japan".to_owned(), "World".to_owned()];
        assert_eq!(
            made_under(Some(&regions)),
            "1 region: it chose only among games of Japan and World."
        );
    }

    #[test]
    fn a_preview_shows_only_for_the_draft_it_was_made_for() {
        let asia = regions(&["Japan"]);
        let preview = |missing_after| CoveragePreview {
            coverage: Some(asia.clone()),
            audit: Some(1),
            missing_before: Some(10),
            missing_after: Some(missing_after),
            stale_library_folders: Vec::new(),
        };
        let made = (Ok(Some(asia.clone())), Ok(preview(4)));
        assert_eq!(
            preview_text(&Ok(Some(asia.clone())), Some(&made)),
            "Missing in your coverage: 10 now, 4 after saving. No library folder will become stale."
        );
        assert_eq!(
            preview_text(&Ok(None), Some(&made)),
            "Previewing what saving changes…"
        );
        assert_eq!(
            preview_text(&Ok(None), None),
            "Previewing what saving changes…"
        );
        assert_eq!(
            preview_text(&Err("Choose at least one region.".into()), Some(&made)),
            "Choose at least one region."
        );
    }

    #[test]
    fn saving_a_loaded_coverage_unchanged_keeps_it() {
        let order = regions(&["USA", "Japan", "Unknown"]);
        let stored = regions(&["Japan", "Unknown"]);
        assert_eq!(Draft::of(Some(&stored)).coverage(&order), Ok(Some(stored)));
        assert_eq!(Draft::of(None).coverage(&order), Ok(None));
    }
}
