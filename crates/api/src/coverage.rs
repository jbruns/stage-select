//! Coverage (ADR 0021): the regions of its DAT's Games a Source Set intends to preserve. It is stored as an expanded
//! list of the Selection engine's region names; without one, a Source Set covers every Game.

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
