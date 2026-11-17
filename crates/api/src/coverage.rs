//! Coverage (ADR 0021): the regions of its DAT's Games a Source Set intends to preserve. It is stored as an expanded
//! list of the Selection engine's region names; without one, a Source Set covers every Game.

use std::collections::BTreeMap;

use crate::{CoveredEntries, Finding, GameStatus, OutsideCoverage, RegionCount};

/// A named set of regions to build a Coverage from. Its name is a label only: a Coverage stores the regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub regions: &'static [&'static str],
}

/// The presets, in the order the workbench offers them; they combine. The evidence for each, and which placements are
/// inferred, is in `docs/research/0175-redump-region-splits.md`.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "USA + Canada",
        regions: &["USA", "Canada"],
    },
    Preset {
        name: "Latin America",
        regions: &["Mexico", "Brazil", "Argentina", "Peru", "Latin America"],
    },
    Preset {
        name: "Europe + Australia",
        regions: &[
            "Europe",
            "UK",
            "Ireland",
            "Australia",
            "New Zealand",
            "France",
            "Germany",
            "Spain",
            "Italy",
            "Netherlands",
            "Belgium",
            "Austria",
            "Switzerland",
            "Portugal",
            "Russia",
            "Ukraine",
            "Poland",
            "Scandinavia",
            "Sweden",
            "Norway",
            "Denmark",
            "Finland",
            "Iceland",
            "Greece",
            "Hungary",
            "Czech",
            "Slovakia",
            "Turkey",
            "Croatia",
            "Slovenia",
            "Bulgaria",
            "Romania",
            "Serbia",
            "Estonia",
            "Latvia",
            "Lithuania",
            "Macedonia",
            "Albania",
            "Israel",
            "South Africa",
        ],
    },
    Preset {
        name: "Asia",
        regions: &[
            "Japan",
            "Asia",
            "Korea",
            "Taiwan",
            "Hong Kong",
            "China",
            "Thailand",
            "Singapore",
            "Indonesia",
            "India",
            "United Arab Emirates",
        ],
    },
];

/// The preset named `name`.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// The region every Coverage covers. The engine reads `Export` as World too.
pub const WORLD: &str = "World";
/// The region of a Game with no region tag the engine recognises. No preset includes it.
pub const UNKNOWN: &str = "Unknown";

/// Checks `regions` against `known`, the engine's region names, and returns them as a Coverage stores them: each
/// once, in `known`'s order. Refuses an unknown name, and an empty list, which would cover only World Games.
pub fn canonical(regions: &[String], known: &[String]) -> Result<Vec<String>, String> {
    let unknown: Vec<&str> = regions
        .iter()
        .filter(|r| !known.contains(r))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!("coverage: unknown regions: {}", unknown.join(", ")));
    }
    if regions.is_empty() {
        return Err(
            "coverage: choose at least one region, or clear the coverage to cover every game"
                .into(),
        );
    }
    Ok(known
        .iter()
        .filter(|k| regions.contains(k))
        .cloned()
        .collect())
}

/// Whether a Game whose regions are `regions` (as the selection crate reads them from its name) is covered by
/// `coverage`: any region in it is, World always is, and Unknown only when the Coverage names it. The engine reads a
/// `(United Kingdom)` tag as a region of its own, which is covered where `UK` is.
pub fn covered<S: AsRef<str>>(regions: &[S], coverage: Option<&[String]>) -> bool {
    let Some(coverage) = coverage else {
        return true;
    };
    regions.iter().map(AsRef::as_ref).any(|region| {
        let region = if region == "United Kingdom" {
            "UK"
        } else {
            region
        };
        region == WORLD || coverage.iter().any(|c| c == region)
    })
}

/// An Audit's Games counted by Game Status for each distinct list of regions they carry, and its Entries counted by
/// Finding and Claim Evidence for each distinct set of their Games' region lists. A Coverage reads its counts from
/// this alone, so a large Audit's names are read once, not on every change of Coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    games: BTreeMap<Vec<String>, BTreeMap<GameStatus, usize>>,
    entries: BTreeMap<Vec<Vec<String>>, CoveredEntries>,
}

impl Tally {
    /// Counts a Game whose regions are `regions` and whose Game Status is `status`.
    pub fn add(&mut self, regions: Vec<String>, status: GameStatus) {
        *self
            .games
            .entry(regions)
            .or_default()
            .entry(status)
            .or_default() += 1;
    }

    /// Counts an Entry with `finding`, resting on Claim Evidence when `claimed`, whose ROMs belong to Games whose
    /// regions are `games`: none for an Entry that matches no Game.
    pub fn add_entry(&mut self, mut games: Vec<Vec<String>>, finding: Finding, claimed: bool) {
        games.sort();
        games.dedup();
        let entries = self.entries.entry(games).or_default();
        *entries.findings.entry(finding).or_default() += 1;
        entries.claimed += usize::from(claimed);
    }

    /// The Entries a Source Set's status ranks on under `coverage`: those of a covered Game, and those of no Game.
    pub fn covered_entries(&self, coverage: Option<&[String]>) -> CoveredEntries {
        let mut covered = CoveredEntries::default();
        for (games, entries) in &self.entries {
            if games.is_empty() || games.iter().any(|g| self::covered(g, coverage)) {
                for (finding, n) in &entries.findings {
                    *covered.findings.entry(*finding).or_default() += n;
                }
                covered.claimed += entries.claimed;
            }
        }
        covered
    }

    /// The Games `coverage` covers, by Game Status, and those it doesn't. Without a Coverage, nothing is outside it.
    pub fn split(
        &self,
        coverage: Option<&[String]>,
    ) -> (BTreeMap<GameStatus, usize>, OutsideCoverage) {
        let (mut covered, mut outside) = (BTreeMap::new(), OutsideCoverage::default());
        for (regions, games) in &self.games {
            let into = if self::covered(regions, coverage) {
                &mut covered
            } else {
                if regions.iter().any(|r| r == UNKNOWN) {
                    outside.unknown_region += games.values().sum::<usize>();
                }
                &mut outside.games
            };
            for (status, n) in games {
                *into.entry(*status).or_default() += n;
            }
        }
        (covered, outside)
    }

    /// One row per region a Game carries, ordered as `order` (the engine's region names), then by name; a Game counts
    /// under each of its regions. `covered` says whether `coverage` covers the region.
    pub fn by_region(&self, order: &[String], coverage: Option<&[String]>) -> Vec<RegionCount> {
        let mut rows: BTreeMap<&str, BTreeMap<GameStatus, usize>> = BTreeMap::new();
        for (regions, games) in &self.games {
            for region in regions {
                let row = rows.entry(region).or_default();
                for (status, n) in games {
                    *row.entry(*status).or_default() += n;
                }
            }
        }
        let mut rows: Vec<RegionCount> = rows
            .into_iter()
            .map(|(region, games)| RegionCount {
                region: region.to_owned(),
                covered: covered(&[region], coverage),
                games,
            })
            .collect();
        rows.sort_by_key(|row| {
            (
                order
                    .iter()
                    .position(|r| *r == row.region)
                    .unwrap_or(usize::MAX),
                row.region.clone(),
            )
        });
        rows
    }

    /// Every Game once, by Game Status.
    pub fn total(&self) -> BTreeMap<GameStatus, usize> {
        self.split(None).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(regions: &[&str]) -> Vec<String> {
        regions.iter().map(|r| (*r).to_owned()).collect()
    }

    #[test]
    fn a_game_is_covered_when_any_of_its_regions_is() {
        let europe = coverage(&["Europe"]);
        assert!(covered(&["USA", "Europe"], Some(&europe)));
        assert!(!covered(&["USA"], Some(&europe)));
    }

    #[test]
    fn a_world_game_is_covered_by_every_coverage() {
        assert!(covered(&["World"], Some(&coverage(&["Japan"]))));
    }

    #[test]
    fn an_unknown_region_game_is_covered_only_when_the_coverage_names_unknown() {
        assert!(!covered(&["Unknown"], Some(&coverage(&["USA"]))));
        assert!(covered(&["Unknown"], Some(&coverage(&["USA", "Unknown"]))));
    }

    #[test]
    fn without_a_coverage_every_game_is_covered() {
        assert!(covered(&["Japan"], None));
        assert!(covered(&["Unknown"], None));
    }

    #[test]
    fn an_empty_coverage_is_refused() {
        let known = coverage(&["USA", "Europe"]);
        assert!(
            canonical(&[], &known)
                .unwrap_err()
                .contains("at least one region")
        );
    }

    #[test]
    fn a_united_kingdom_game_is_covered_by_uk() {
        assert!(covered(&["United Kingdom"], Some(&coverage(&["UK"]))));
    }
}
