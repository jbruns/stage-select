//! A Library Folder's cleanup plan, reviewed in the plan shape: every candidate the latest check and the kept files
//! offer, what the plan does with each, and the cleanup you plan again from your choices.

use std::collections::BTreeMap;

use stage_select_api::additions::NewAddition;
use stage_select_api::build::{
    BuildDetail, BuildState, CheckedFile, Cleanup, CleanupAction, DriftEntry, DriftKind,
    FileOutcome, KeptFile, PlanCleanup,
};

use crate::build_model::{self, cleanup_actions};
use crate::parts::plan_model::{self, Does, Picks};
use crate::parts::status::{CleanupKind, ItemOutcome};

/// What you choose for one candidate of a cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupChoice {
    Leave,
    SetAside,
    Adopt,
    Acknowledge,
    AddAddition,
    Repack,
    PutBack,
}

impl CleanupChoice {
    pub fn label(self) -> &'static str {
        match self {
            CleanupChoice::Leave => "Leave it",
            CleanupChoice::SetAside => "Set aside",
            CleanupChoice::Adopt => "Adopt it",
            CleanupChoice::Acknowledge => "Acknowledge",
            CleanupChoice::AddAddition => "Add as addition",
            CleanupChoice::Repack => "Repack",
            CleanupChoice::PutBack => "Put back",
        }
    }

    fn of(action: CleanupAction) -> Self {
        match action {
            CleanupAction::KeepAside => CleanupChoice::SetAside,
            CleanupAction::Adopt => CleanupChoice::Adopt,
            CleanupAction::Acknowledge => CleanupChoice::Acknowledge,
            CleanupAction::AddAddition => CleanupChoice::AddAddition,
        }
    }

    fn action(self) -> Option<CleanupAction> {
        match self {
            CleanupChoice::SetAside => Some(CleanupAction::KeepAside),
            CleanupChoice::Adopt => Some(CleanupAction::Adopt),
            CleanupChoice::Acknowledge => Some(CleanupAction::Acknowledge),
            CleanupChoice::AddAddition => Some(CleanupAction::AddAddition),
            CleanupChoice::Leave | CleanupChoice::Repack | CleanupChoice::PutBack => None,
        }
    }
}

/// What a cleanup can be asked to do something with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// An entry Stage Select did not build, by name.
    Entry(String),
    /// A built file that is not TorrentZip, by name.
    Repack(String),
    /// A kept file, by id.
    Kept(i64),
}

/// What the folder holds that a cleanup can change, as found when you planned: the latest check's drift and built
/// files, and the folder's kept files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub drift: Vec<DriftEntry>,
    pub files: Vec<CheckedFile>,
    pub kept: Vec<KeptFile>,
}

/// One candidate of a cleanup: what it is, what the plan shown does with it, and what you may choose instead.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupItem {
    pub key: String,
    pub candidate: Candidate,
    pub kind: CleanupKind,
    /// Its name now: in the folder, or, for a kept file, in the retention area.
    pub name: String,
    pub game: String,
    pub offers: Vec<CleanupChoice>,
    pub planned: CleanupChoice,
    /// Where the plan puts it, when it moves it: an adoption's or a put-back's name.
    pub target: Option<String>,
    pub detail: Option<String>,
}

/// Every candidate of `cleanup`, with `found`: the entries it lists, then the built files to repack, then the kept
/// files, each with the choice the plan gives it. With no cleanup planned yet, every entry `found` and every candidate
/// is left as it is.
pub fn items(cleanup: Option<&Cleanup>, found: &Found) -> Vec<CleanupItem> {
    type Listed<'a> = (
        &'a str,
        DriftKind,
        Option<&'a String>,
        Option<CleanupAction>,
        Option<&'a String>,
    );
    let listed: Vec<Listed> = match cleanup {
        Some(c) => c
            .entries
            .iter()
            .map(|e| {
                (
                    e.name.as_str(),
                    e.kind,
                    e.game.as_ref(),
                    e.action,
                    e.target.as_ref(),
                )
            })
            .collect(),
        None => found
            .drift
            .iter()
            .map(|d| (d.name.as_str(), d.kind, d.game.as_ref(), None, None))
            .collect(),
    };
    let repacks_planned = cleanup.map_or(&[][..], |c| &c.repacks);
    let restores = cleanup.map_or(&[][..], |c| &c.restores);
    let entries = listed
        .into_iter()
        .map(|(name, kind, game, action, target)| {
            let drift = found.drift.iter().find(|d| d.name == name);
            let planned = action.map_or(CleanupChoice::Leave, CleanupChoice::of);
            // An entry the latest check no longer reports is offered only what's safe anywhere.
            let actions = match drift {
                Some(d) => cleanup_actions(d),
                None => vec![CleanupAction::KeepAside],
            };
            let mut offers: Vec<CleanupChoice> = std::iter::once(CleanupChoice::Leave)
                .chain(actions.into_iter().map(CleanupChoice::of))
                .collect();
            if !offers.contains(&planned) {
                offers.push(planned);
            }
            let kind = match (drift.is_some_and(|d| d.conflict), kind) {
                (true, _) => CleanupKind::Conflict,
                (_, DriftKind::Selected) => CleanupKind::NotBuilt,
                (_, DriftKind::Renamed) => CleanupKind::Renamed,
                (_, DriftKind::Unselected) => CleanupKind::Unselected,
                (_, DriftKind::Unknown) => CleanupKind::Unknown,
                (_, DriftKind::Directory) => CleanupKind::Directory,
            };
            CleanupItem {
                key: format!("entry:{name}"),
                candidate: Candidate::Entry(name.to_string()),
                kind,
                name: name.to_string(),
                game: game.cloned().unwrap_or_default(),
                offers,
                planned,
                target: (planned == CleanupChoice::Adopt)
                    .then(|| target.cloned())
                    .flatten(),
                detail: drift.and_then(|d| d.detail.clone()),
            }
        });
    let mut repackable: Vec<(String, String)> = found
        .files
        .iter()
        .filter(|f| {
            !f.torrentzip
                && matches!(
                    f.outcome,
                    FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
                )
        })
        .map(|f| (f.target.clone(), f.game.clone()))
        .collect();
    for r in repacks_planned {
        if !repackable.iter().any(|(t, _)| *t == r.target) {
            repackable.push((r.target.clone(), r.game.clone()));
        }
    }
    let repacks = repackable.into_iter().map(|(target, game)| {
        let planned = if repacks_planned.iter().any(|r| r.target == target) {
            CleanupChoice::Repack
        } else {
            CleanupChoice::Leave
        };
        CleanupItem {
            key: format!("repack:{target}"),
            candidate: Candidate::Repack(target.clone()),
            kind: CleanupKind::Repack,
            name: target,
            game,
            offers: vec![CleanupChoice::Leave, CleanupChoice::Repack],
            planned,
            target: None,
            detail: None,
        }
    });
    let mut kept: Vec<&KeptFile> = found.kept.iter().collect();
    for r in restores {
        if !kept.iter().any(|k| k.id == r.kept.id) {
            kept.push(&r.kept);
        }
    }
    let kept = kept.into_iter().map(|k| {
        let restore = restores.iter().find(|r| r.kept.id == k.id);
        CleanupItem {
            key: format!("kept:{}", k.id),
            candidate: Candidate::Kept(k.id),
            kind: CleanupKind::Kept,
            name: format!("{}/{}", k.generation, k.kept_name),
            game: k.game.clone().unwrap_or_default(),
            offers: vec![CleanupChoice::Leave, CleanupChoice::PutBack],
            planned: if restore.is_some() {
                CleanupChoice::PutBack
            } else {
                CleanupChoice::Leave
            },
            target: Some(restore.map_or_else(|| k.name.clone(), |r| r.target.clone())),
            detail: None,
        }
    });
    entries.chain(repacks).chain(kept).collect()
}

/// The cleanup to plan again: each of `items` as you `picks`ed it, and what each entry you add as an addition is
/// added as, from `additions`, by name.
pub fn request(
    items: &[CleanupItem],
    picks: &Picks<CleanupChoice>,
    additions: &BTreeMap<String, NewAddition>,
) -> PlanCleanup {
    let mut request = PlanCleanup::default();
    for item in items {
        let choice = picks.now(&item.key, item.planned);
        match (&item.candidate, choice) {
            (_, CleanupChoice::Leave) => {}
            (Candidate::Entry(name), c) => {
                if let Some(action) = c.action() {
                    request.entries.insert(name.clone(), action);
                }
                if let Some(a) = additions
                    .get(name)
                    .filter(|_| c == CleanupChoice::AddAddition)
                {
                    request.additions.insert(name.clone(), a.clone());
                }
            }
            (Candidate::Repack(target), CleanupChoice::Repack) => {
                request.repack.push(target.clone())
            }
            (Candidate::Kept(id), CleanupChoice::PutBack) => request.restore.push(*id),
            _ => {}
        }
    }
    request
}

/// The cleanup's one-line summary in numbers.
pub fn plan_line(items: &[CleanupItem]) -> String {
    let does = |c: CleanupChoice| match c {
        CleanupChoice::SetAside => Some(Does::new("sets aside", "entry", "entries")),
        CleanupChoice::Adopt => Some(Does::new("adopts", "file", "files")),
        CleanupChoice::Acknowledge => Some(Does::new("acknowledges", "entry", "entries")),
        CleanupChoice::AddAddition => Some(Does::new("adds", "addition", "additions")),
        CleanupChoice::Repack => Some(Does::new("repacks", "file", "files")),
        CleanupChoice::PutBack => Some(Does::new("puts back", "kept file", "kept files")),
        CleanupChoice::Leave => None,
    };
    plan_model::plan_line(
        items.iter().filter_map(|i| does(i.planned)).map(|d| (d, 1)),
        "Changes nothing",
    )
}

/// What became of `item` once its cleanup's build `detail` ran: a repack by its own item, anything else by the build,
/// which stops at the first step that fails. Nothing for what it leaves, or while it runs.
pub fn item_outcome(item: &CleanupItem, detail: &BuildDetail) -> Option<ItemOutcome> {
    if item.planned == CleanupChoice::Leave {
        return None;
    }
    if let Candidate::Repack(target) = &item.candidate
        && let Some(built) = detail.items.iter().find(|i| i.target == *target)
    {
        return Some(build_model::item_outcome(built.state));
    }
    match detail.build.state {
        BuildState::Running => None,
        BuildState::Succeeded => Some(ItemOutcome::Done),
        BuildState::Failed | BuildState::Interrupted => Some(ItemOutcome::Uncertain),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stat() -> serde_json::Map<String, serde_json::Value> {
        json!({ "file": { "size": 1, "mtime_ns": 0 }, "identity": { "dev": 1, "ino": 2 } })
            .as_object()
            .unwrap()
            .clone()
    }

    fn with_stat(mut v: serde_json::Value) -> serde_json::Value {
        v.as_object_mut().unwrap().extend(stat());
        v
    }

    fn kept(id: i64, name: &str) -> serde_json::Value {
        with_stat(json!({
            "id": id, "folder": 1, "generation": 3, "name": name, "kept_name": name,
            "reason": "drift", "build": 1, "kept_at": "now",
        }))
    }

    fn drift(name: &str, kind: &str, conflict: bool) -> DriftEntry {
        serde_json::from_value(json!({
            "name": name, "kind": kind, "game": "Pong", "conflict": conflict, "hashed": true,
        }))
        .unwrap()
    }

    /// A cleanup that sets `notes.txt` aside and puts kept file 7 back, leaving the rest, found with two more
    /// entries, a built file to repack, and two kept files.
    fn planned() -> (Cleanup, Found) {
        let entry = |name: &str, kind: &str, action: Option<&str>| {
            with_stat(json!({ "name": name, "kind": kind, "action": action, "game": "Pong" }))
        };
        let cleanup = serde_json::from_value(json!({
            "build": 1, "choices": {},
            "entries": [
                entry("notes.txt", "unknown", Some("keep_aside")),
                entry("Pong (USA).zip", "selected", None),
                entry("roms", "directory", None),
            ],
            "restores": [{ "kept": kept(7, "Combat.zip"), "kind": "put_back", "target": "Combat.zip" }],
        }))
        .unwrap();
        let files = serde_json::from_value(json!([
            { "target": "Outlaw.zip", "game": "Outlaw", "outcome": "unchanged", "hashed": false,
              "torrentzip": false, "selected": true },
            { "target": "Air.zip", "game": "Air", "outcome": "unchanged", "hashed": false,
              "torrentzip": true, "selected": true },
            { "target": "Gone.zip", "game": "Gone", "outcome": "missing", "hashed": false,
              "torrentzip": false, "selected": true },
        ]))
        .unwrap();
        let found = Found {
            drift: vec![
                drift("notes.txt", "unknown", false),
                drift("Pong (USA).zip", "selected", true),
                drift("roms", "directory", false),
            ],
            files,
            kept: vec![
                serde_json::from_value(kept(7, "Combat.zip")).unwrap(),
                serde_json::from_value(kept(8, "Tank.zip")).unwrap(),
            ],
        };
        (cleanup, found)
    }

    #[test]
    fn a_cleanup_lists_every_candidate_with_its_planned_choice_and_what_it_may_take() {
        let (cleanup, found) = planned();
        let listed: Vec<_> = items(Some(&cleanup), &found)
            .into_iter()
            .map(|i| (i.key, i.kind, i.planned, i.offers))
            .collect();
        use CleanupChoice::*;
        assert_eq!(
            listed,
            [
                (
                    "entry:notes.txt".to_string(),
                    CleanupKind::Unknown,
                    SetAside,
                    vec![Leave, SetAside, AddAddition, Acknowledge]
                ),
                // In the way of a selected game: never acknowledged.
                (
                    "entry:Pong (USA).zip".to_string(),
                    CleanupKind::Conflict,
                    Leave,
                    vec![Leave, SetAside, Adopt]
                ),
                (
                    "entry:roms".to_string(),
                    CleanupKind::Directory,
                    Leave,
                    vec![Leave, SetAside, Acknowledge]
                ),
                // Only a built file that is there and isn't TorrentZip is repacked.
                (
                    "repack:Outlaw.zip".to_string(),
                    CleanupKind::Repack,
                    Leave,
                    vec![Leave, Repack]
                ),
                (
                    "kept:7".to_string(),
                    CleanupKind::Kept,
                    PutBack,
                    vec![Leave, PutBack]
                ),
                (
                    "kept:8".to_string(),
                    CleanupKind::Kept,
                    Leave,
                    vec![Leave, PutBack]
                ),
            ]
        );
    }

    #[test]
    fn planning_again_asks_for_every_choice_as_you_left_it() {
        let (cleanup, found) = planned();
        let listed = items(Some(&cleanup), &found);
        let mut picks = Picks::default();
        let outcome = picks.set(
            listed
                .iter()
                .map(|i| (i.key.as_str(), i.planned, i.offers.as_slice())),
            CleanupChoice::SetAside,
        );
        assert_eq!(outcome.changed, 2, "{outcome:?}");
        picks.pick("entry:notes.txt", CleanupChoice::AddAddition);
        picks.pick("repack:Outlaw.zip", CleanupChoice::Repack);
        picks.pick("kept:7", CleanupChoice::Leave);
        let addition = NewAddition {
            system: "No-Intro/Atari - 2600".into(),
            name: "notes".into(),
            parent: None,
        };
        let added = BTreeMap::from([("notes.txt".to_string(), addition.clone())]);

        let asked = request(&listed, &picks, &added);

        assert_eq!(
            asked,
            PlanCleanup {
                entries: BTreeMap::from([
                    ("notes.txt".to_string(), CleanupAction::AddAddition),
                    ("Pong (USA).zip".to_string(), CleanupAction::KeepAside),
                    ("roms".to_string(), CleanupAction::KeepAside),
                ]),
                restore: Vec::new(),
                repack: vec!["Outlaw.zip".to_string()],
                additions: BTreeMap::from([("notes.txt".to_string(), addition)]),
            }
        );
        // Unpicked, the plan is asked for again as it is.
        let again = request(&listed, &Picks::default(), &added);
        assert_eq!(
            again,
            PlanCleanup {
                entries: BTreeMap::from([("notes.txt".to_string(), CleanupAction::KeepAside)]),
                restore: vec![7],
                ..PlanCleanup::default()
            }
        );
    }

    #[test]
    fn the_summary_line_counts_what_the_cleanup_does() {
        let (cleanup, found) = planned();
        assert_eq!(
            plan_line(&items(Some(&cleanup), &found)),
            "Sets aside 1 entry, puts back 1 kept file"
        );
        let nothing = Cleanup {
            restores: Vec::new(),
            entries: Vec::new(),
            ..cleanup
        };
        assert_eq!(
            plan_line(&items(Some(&nothing), &Found::default())),
            "Changes nothing"
        );
    }

    #[test]
    fn a_finished_cleanup_says_what_became_of_every_candidate_it_changes() {
        let (cleanup, found) = planned();
        let mut listed = items(Some(&cleanup), &found);
        listed[3].planned = CleanupChoice::Repack;
        let detail = |state: &str, repacked: &str| -> BuildDetail {
            serde_json::from_value(json!({
                "build": {
                    "id": 1, "folder": 1, "plan": 1, "job": 1, "state": state, "generation": 2,
                    "selection": 1, "audit": 1, "dat_revision": "r", "started_at": "now", "finished_at": null,
                    "applied": 0, "failed": 0, "pending": 0, "uncertain": 0,
                    "planned": serde_json::to_value(stage_select_api::build::PlanSummary::default()).unwrap(),
                },
                "items": [{ "target": "Outlaw.zip", "game": "Outlaw", "kind": "repack", "state": repacked }],
            }))
            .unwrap()
        };
        let outcomes =
            |d: &BuildDetail| -> Vec<_> { listed.iter().map(|i| item_outcome(i, d)).collect() };
        let done = Some(ItemOutcome::Done);
        assert_eq!(
            outcomes(&detail("succeeded", "applied")),
            [done, None, None, done, done, None]
        );
        let unsure = Some(ItemOutcome::Uncertain);
        assert_eq!(
            outcomes(&detail("failed", "failed")),
            [unsure, None, None, Some(ItemOutcome::Failed), unsure, None]
        );
        assert_eq!(
            outcomes(&detail("running", "pending")),
            [None, None, None, Some(ItemOutcome::NotDone), None, None]
        );
    }

    #[test]
    fn a_cleanup_not_yet_planned_lists_everything_found_left_as_it_is_and_asks_for_nothing() {
        let (_, found) = planned();
        let draft = items(None, &found);
        use CleanupChoice::*;
        assert_eq!(
            draft
                .iter()
                .map(|i| (i.key.as_str(), i.planned, i.offers.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    "entry:notes.txt",
                    Leave,
                    vec![Leave, SetAside, AddAddition, Acknowledge]
                ),
                ("entry:Pong (USA).zip", Leave, vec![Leave, SetAside, Adopt]),
                ("entry:roms", Leave, vec![Leave, SetAside, Acknowledge]),
                ("repack:Outlaw.zip", Leave, vec![Leave, Repack]),
                ("kept:7", Leave, vec![Leave, PutBack]),
                ("kept:8", Leave, vec![Leave, PutBack]),
            ]
        );
        assert_eq!(draft[0].game, "Pong");
        assert_eq!(plan_line(&draft), "Changes nothing");
        assert!(request(&draft, &Picks::default(), &BTreeMap::new()).is_empty());
        let mut picks = Picks::default();
        picks.pick("entry:roms", Acknowledge);
        assert_eq!(
            request(&draft, &picks, &BTreeMap::new()).entries,
            BTreeMap::from([("roms".to_string(), CleanupAction::Acknowledge)])
        );
    }
}
