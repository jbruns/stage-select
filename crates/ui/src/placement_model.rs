//! Single copy's pure logic: how many of a source set's games are placed and where, which library folder holds them,
//! where an audit found each game, and the games a build flagged as needing review.

use std::collections::{BTreeMap, HashMap};

use title_select_api::build::{BuiltFile, ItemKind, LibraryFolder};
use title_select_api::storage::split_placed;
use title_select_api::{EntryRow, GameStatus, ReviewFlag, SourceSetSummary};

use crate::model::when;
use crate::parts::status::Placement;

/// How many games the source set's latest audit found present, in its coverage and outside it: what "N of M games
/// placed" counts against. None before its first audit.
pub fn present_games(summary: &SourceSetSummary) -> Option<u64> {
    let present = |games: &BTreeMap<GameStatus, usize>| {
        [GameStatus::Complete, GameStatus::Incomplete]
            .iter()
            .filter_map(|s| games.get(s))
            .sum::<usize>() as u64
    };
    let latest = summary.latest.as_ref()?;
    let outside = summary
        .outside_coverage
        .as_ref()
        .map_or(0, |o| present(&o.games));
    Some(present(&latest.games) + outside)
}

/// "3 of 10 games placed in play: atari", or without an audit to count against, "3 games placed in play: atari".
pub fn placed_line(placed: u64, games: Option<u64>, folder: &str) -> String {
    match games {
        Some(games) => format!("{placed} of {} games placed in {folder}", games.max(placed)),
        None if placed == 1 => format!("1 game placed in {folder}"),
        None => format!("{placed} games placed in {folder}"),
    }
}

/// How a library folder reads: "root: path", as the library folder lists show it, or by its number once it is gone.
pub fn folder_name(folders: &[LibraryFolder], id: i64) -> String {
    folders.iter().find(|f| f.id == id).map_or_else(
        || format!("library folder #{id}"),
        |f| format!("{}: {}", f.root, f.path),
    )
}

/// Which library folder holds the source set's placed games, and whether you choose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holding {
    /// No library folder is built from the source set, so none holds its games.
    NoFolder,
    /// One library folder is built from it: that one holds them.
    Only(i64),
    /// Several are: you choose the one that holds them, from `folders` by id, or none.
    Choose {
        folders: Vec<i64>,
        chosen: Option<i64>,
    },
}

impl Holding {
    /// Of `folders`, those bound to source set `set`, with `holding` the folder the service says holds its games.
    pub fn of(folders: &[LibraryFolder], set: &str, holding: Option<i64>) -> Holding {
        let bound: Vec<i64> = folders
            .iter()
            .filter(|f| f.source_set == set)
            .map(|f| f.id)
            .collect();
        match bound.as_slice() {
            [] => Holding::NoFolder,
            [only] => Holding::Only(*only),
            _ => Holding::Choose {
                chosen: holding.filter(|h| bound.contains(h)),
                folders: bound,
            },
        }
    }

    /// What the choice means for your builds.
    pub fn note(&self) -> &'static str {
        match self {
            Holding::NoFolder => {
                "No library folder is built from this source set, so none holds its games."
            }
            Holding::Only(_) => {
                "It's the only library folder built from this source set, so with single copy on, its builds place your games there."
            }
            Holding::Choose { chosen: None, .. } => {
                "Several library folders are built from this source set. Choose the one that holds its placed games: until you do, builds place nothing and copy each game instead."
            }
            Holding::Choose {
                chosen: Some(_), ..
            } => {
                "Changing it moves nothing now. The next build of the folder holding your games returns them to the source set, then the next build of the folder you chose places them there."
            }
        }
    }
}

/// Each game an audit found placed, with the library folder it was placed in: a game whose entries name a placed
/// path. A game not listed was in its source set's folder.
pub fn game_placements(entries: &[EntryRow]) -> HashMap<String, i64> {
    let mut placed = HashMap::new();
    for row in entries {
        if let Some((folder, _)) = split_placed(&row.entry.path) {
            for rom in &row.entry.roms {
                placed.insert(rom.game.clone(), folder);
            }
        }
    }
    placed
}

/// Where a built file is kept: a file placed under single copy is the game's only copy.
pub fn built_placement(file: &BuiltFile) -> Option<Placement> {
    (file.kind == ItemKind::Place).then_some(Placement::Placed)
}

/// "1 game needs review", "3 games need review".
pub fn review_line(n: usize) -> String {
    match n {
        1 => "1 game needs review".to_string(),
        n => format!("{n} games need review"),
    }
}

/// The review flags, by game.
pub fn flags_by_game(flags: &[ReviewFlag]) -> BTreeMap<String, ReviewFlag> {
    flags.iter().map(|f| (f.game.clone(), f.clone())).collect()
}

/// Why a game needs review and what found it, for its detail.
pub fn flag_detail(flag: &ReviewFlag) -> String {
    format!(
        "{}. Build #{} found this, {}. A full audit that verifies the file, or returning the game to your source \
         set, clears it.",
        flag.reason.trim_end_matches('.'),
        flag.build,
        when(&flag.raised_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary(latest: serde_json::Value, outside: Option<serde_json::Value>) -> SourceSetSummary {
        let mut s = json!({
            "set": { "name": "Atari", "root": "sets", "path": "Atari", "dat": "No-Intro/Atari", "added_at": "t",
                     "last_verified_against": null },
            "latest": latest,
        });
        if let Some(outside) = outside {
            s["outside_coverage"] = outside;
        }
        serde_json::from_value(s).unwrap()
    }

    fn audit(games: serde_json::Value) -> serde_json::Value {
        json!({
            "id": 1, "source_set": "Atari", "started_at": "t", "finished_at": "t", "dat_revision_sha256": "r",
            "dat_revision_version": "1", "findings": {}, "games": games, "pruned": false,
        })
    }

    fn folder(id: i64, set: &str, path: &str) -> LibraryFolder {
        serde_json::from_value(json!({
            "id": id, "root": "play", "path": path, "system": "No-Intro/Atari", "source_set": set,
            "profile": "Everyday", "generation": 0, "created_at": "t", "writable": true, "build": null, "files": 0,
            "active_job": null, "latest_plan": null,
        }))
        .unwrap()
    }

    #[test]
    fn present_games_count_the_complete_and_incomplete_in_and_outside_the_coverage() {
        let counted = summary(
            audit(json!({"complete": 7, "incomplete": 2, "missing": 40})),
            Some(json!({"games": {"complete": 1, "missing": 9}, "unknown_region": 0})),
        );
        assert_eq!(present_games(&counted), Some(10));
        let whole = summary(audit(json!({"complete": 3})), None);
        assert_eq!(present_games(&whole), Some(3));
        let never = summary(serde_json::Value::Null, None);
        assert_eq!(present_games(&never), None);
    }

    #[test]
    fn the_placed_count_reads_against_the_games_present() {
        assert_eq!(
            placed_line(3, Some(10), "play: atari"),
            "3 of 10 games placed in play: atari"
        );
        assert_eq!(
            placed_line(1, None, "play: atari"),
            "1 game placed in play: atari"
        );
        assert_eq!(
            placed_line(4, Some(2), "play: atari"),
            "4 of 4 games placed in play: atari",
            "an audit older than the placements never counts fewer games than are placed"
        );
    }

    #[test]
    fn a_folder_reads_by_its_root_and_path_or_its_number_once_gone() {
        let folders = [folder(4, "Atari", "atari")];
        assert_eq!(folder_name(&folders, 4), "play: atari");
        assert_eq!(folder_name(&folders, 9), "library folder #9");
    }

    #[test]
    fn you_choose_the_holding_folder_only_when_several_are_bound() {
        let folders = [
            folder(1, "Atari", "a"),
            folder(2, "Other", "o"),
            folder(3, "Atari", "b"),
        ];
        assert_eq!(Holding::of(&folders, "Nothing", None), Holding::NoFolder);
        assert_eq!(
            Holding::of(&folders[..2], "Atari", Some(1)),
            Holding::Only(1)
        );
        assert_eq!(
            Holding::of(&folders, "Atari", None),
            Holding::Choose {
                folders: vec![1, 3],
                chosen: None
            }
        );
        assert_eq!(
            Holding::of(&folders, "Atari", Some(3)),
            Holding::Choose {
                folders: vec![1, 3],
                chosen: Some(3)
            }
        );
        let unchosen = Holding::Choose {
            folders: vec![1, 3],
            chosen: None,
        };
        assert!(unchosen.note().contains("copy"), "{}", unchosen.note());
        let chosen = Holding::Choose {
            folders: vec![1, 3],
            chosen: Some(3),
        };
        assert!(chosen.note().contains("moves nothing"), "{}", chosen.note());
    }

    #[test]
    fn a_game_is_placed_where_its_entries_name_a_placed_path() {
        let entry = |seq: u64, path: &str, game: &str| -> EntryRow {
            serde_json::from_value(json!({
                "seq": seq, "path": path, "finding": "matched",
                "roms": [{"game": game, "name": format!("{game}.a26")}],
            }))
            .unwrap()
        };
        let placed = game_placements(&[
            entry(1, "Pong (USA).zip", "Pong (USA)"),
            entry(2, ".title-select/placed/7/Combat (USA).zip", "Combat (USA)"),
            entry(
                3,
                ".title-select/placed/7/disc/Orbit (Track 1).bin",
                "Orbit",
            ),
            entry(
                4,
                ".title-select/placed/7/disc/Orbit (Track 2).bin",
                "Orbit",
            ),
        ]);
        assert_eq!(
            placed,
            HashMap::from([("Combat (USA)".to_string(), 7), ("Orbit".to_string(), 7)])
        );
    }

    #[test]
    fn a_placed_file_is_marked_the_only_copy_and_a_built_copy_is_not() {
        let built = |kind: &str| -> BuiltFile {
            serde_json::from_value(json!({
                "target": "Pong (USA).zip", "game": "Pong (USA)", "kind": kind, "build": 1, "selection": 1,
                "audit": 1, "dat_revision": "r", "sources": [], "roms": [], "file": {"size": 1, "mtime_ns": 0},
            }))
            .unwrap()
        };
        assert_eq!(built_placement(&built("place")), Some(Placement::Placed));
        assert_eq!(built_placement(&built("copy")), None);
        assert_eq!(built_placement(&built("repack")), None);
    }

    #[test]
    fn a_flagged_game_says_why_and_which_build_found_it() {
        let flag = ReviewFlag {
            game: "Orbit".into(),
            path: ".title-select/placed/7/Orbit.chd".into(),
            reason: "track 2's SHA-1 differs from the DAT's".into(),
            build: 12,
            raised_at: "2026-11-01T10:00:00Z".into(),
        };
        let detail = flag_detail(&flag);
        assert!(
            detail.starts_with("track 2's SHA-1 differs from the DAT's. Build #12 found this"),
            "{detail}"
        );
        assert_eq!(flags_by_game(std::slice::from_ref(&flag))["Orbit"], flag);
        assert_eq!(review_line(1), "1 game needs review");
        assert_eq!(review_line(3), "3 games need review");
    }
}
