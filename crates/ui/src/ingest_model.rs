//! What an Ingest Folder item is to the workbench, and the decisions you make on many items at once: which items a
//! decision applies to, what the strip of counts offers, and the plan request each makes.

use std::collections::BTreeMap;

use stage_select_api::additions::NewAddition;
use stage_select_api::ingest::{
    Assessment, IngestAction, IngestFolder, IngestItem, IngestOutcome, IngestPlan, IngestPlanItem,
    ItemState, Need, PlanIngest, Proposal,
};

use crate::model::when;
use crate::parts::icon::Glyph;
use crate::parts::outcome::Link;
use crate::parts::status::{IngestStatus, PlanState, Status};
use crate::screens::additions::new_addition;
use crate::screens::source_sets::tab_href;
use crate::source_set_model::PageTab;

/// The checksum to show for an assessed item: a game file's own SHA-256, but none for a DAT, whose SHA-256 is its
/// DAT Revision's identity and which is shown by name and version instead.
pub fn item_checksum(a: &Assessment) -> Option<&str> {
    a.sha256.as_deref().filter(|_| a.dat.is_none())
}

/// What an item that matches a source set's game proposes, in a few words: the game and where it goes, or, when the
/// source set's coverage leaves the game out or can't be applied, that it proposes nothing.
pub fn source_set_match(source_set: &str, game: &str, need: Need) -> String {
    match need {
        Need::OutsideCoverage => "Outside your coverage".into(),
        Need::CoverageUnavailable => "Coverage unknown".into(),
        _ => format!("{game} in {source_set}"),
    }
}

/// The checksum to show for a plan item: none for a DAT file, whatever the plan does with it, as [`item_checksum`].
pub fn plan_checksum(item: &IngestPlanItem) -> Option<&str> {
    item.sha256
        .as_deref()
        .filter(|_| !item.dat_file && item.dat.is_none())
}

/// Whether a move of the item out of the folder is under way.
pub fn moving(item: &IngestItem) -> bool {
    item.last_move.as_ref().is_some_and(|m| m.under_way)
}

/// When an arriving item in `folder` is next looked at: the follow-up scan the Service has pending, else the folder's
/// own scan cadence, else the next scan, whoever starts it.
pub fn arriving_note(folder: &IngestFolder) -> String {
    let next = match (&folder.next_check_at, folder.scan_every_minutes) {
        (Some(at), _) => format!("next check {}", when(at)),
        (None, Some(1)) => "scanned every minute".into(),
        (None, Some(m)) => format!("scanned every {m} minutes"),
        (None, None) => "at the next scan".into(),
    };
    format!("Assessed once settled · {next}")
}

/// The status the item shows, or none for an item no longer in the folder that no plan moved.
pub fn status(item: &IngestItem) -> Option<IngestStatus> {
    if moving(item) {
        return Some(IngestStatus::Moving);
    }
    Some(match item.state {
        ItemState::Arriving | ItemState::Stale => IngestStatus::Arriving,
        ItemState::AlreadyHeld => IngestStatus::Duplicate,
        ItemState::Unreadable | ItemState::Failed | ItemState::Unsupported => {
            IngestStatus::Unreadable
        }
        ItemState::Dismissed => IngestStatus::Dismissed,
        ItemState::Moved => IngestStatus::Done,
        ItemState::Gone => return None,
        ItemState::Assessed => match &item.assessment.proposal {
            Some(Proposal::SourceSet { .. }) => IngestStatus::MatchesSourceSet,
            Some(Proposal::DatImport | Proposal::SupportFileImport) => IngestStatus::ImportFile,
            Some(Proposal::Addition { .. }) | None => IngestStatus::NoDatMatch,
        },
    })
}

/// A decision you make on items: one plan for the first three, nothing moved for the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Add,
    Import,
    Discard,
    Dismiss,
}

impl Decision {
    pub const ALL: [Decision; 4] = [
        Decision::Add,
        Decision::Import,
        Decision::Discard,
        Decision::Dismiss,
    ];

    /// The plan it makes, if it makes one.
    pub fn action(self) -> Option<IngestAction> {
        match self {
            Decision::Add => Some(IngestAction::Addition),
            Decision::Import => Some(IngestAction::DatImport),
            Decision::Discard => Some(IngestAction::Discard),
            Decision::Dismiss => None,
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            Decision::Add => Glyph::CirclePlus,
            Decision::Import => Glyph::Upload,
            Decision::Discard => Glyph::Archive,
            Decision::Dismiss => Glyph::CircleOff,
        }
    }

    /// The toolbar's label, for the checked rows.
    pub fn verb(self) -> &'static str {
        match self {
            Decision::Add => "Add as additions",
            Decision::Import => "Import",
            Decision::Discard => "Discard",
            Decision::Dismiss => "Dismiss",
        }
    }

    /// The label of the decision on `n` items.
    pub fn label(self, n: usize) -> String {
        match (self, n) {
            (Decision::Add, 1) => "Add 1 as an addition".into(),
            (Decision::Add, n) => format!("Add {n} as additions"),
            (Decision::Import, 1) => "Import 1 file".into(),
            (Decision::Import, n) => format!("Import {n} files"),
            (Decision::Discard, n) => format!("Discard {n}"),
            (Decision::Dismiss, n) => format!("Dismiss {n}"),
        }
    }

    /// The label of the decision on one item, as a row's action.
    pub fn single(self) -> &'static str {
        match self {
            Decision::Add => "Add as an addition",
            Decision::Import => "Import it",
            Decision::Discard => "Discard",
            Decision::Dismiss => "Dismiss until it changes",
        }
    }

    /// What shows while it is under way.
    pub fn pending(self) -> &'static str {
        match self {
            Decision::Add => "Planning additions…",
            Decision::Import => "Planning imports…",
            Decision::Discard => "Planning discards…",
            Decision::Dismiss => "Dismissing…",
        }
    }

    /// Why an item it was asked for was left out.
    pub fn left_out(self, n: usize) -> String {
        let (one, many) = match self {
            Decision::Add => (
                "1 item can't be added as an addition: it matched a DAT, holds more than one file, or has no system yet",
                "items can't be added as additions: they matched a DAT, hold more than one file, or have no system yet",
            ),
            Decision::Import => (
                "1 item isn't a DAT or support file to import",
                "items aren't DATs or support files to import",
            ),
            Decision::Discard => (
                "1 item can't be discarded yet: it's still arriving, being moved or gone",
                "items can't be discarded yet: they're still arriving, being moved or gone",
            ),
            Decision::Dismiss => (
                "1 item can't be dismissed: it's arriving, dismissed already, being moved or gone",
                "items can't be dismissed: they're arriving, dismissed already, being moved or gone",
            ),
        };
        match n {
            1 => format!("{one}."),
            n => format!("{n} {many}."),
        }
    }
}

/// What you chose beyond the decision: what each item is added as, where you changed it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choices {
    pub additions: BTreeMap<i64, NewAddition>,
}

impl Choices {
    /// What `item` is added as: what you chose, or its suggested system and name.
    pub fn addition(&self, item: &IngestItem) -> Option<NewAddition> {
        let Some(Proposal::Addition { system }) = &item.assessment.proposal else {
            return None;
        };
        Some(
            self.additions
                .get(&item.id)
                .cloned()
                .unwrap_or_else(|| new_addition(&item.name, system.as_deref())),
        )
    }
}

/// Whether `decision` applies to `item`.
pub fn applies(decision: Decision, item: &IngestItem, choices: &Choices) -> bool {
    if moving(item) {
        return false;
    }
    let state = item.state;
    match decision {
        Decision::Add => {
            state == ItemState::Assessed
                && item.assessment.payloads.len() == 1
                && choices
                    .addition(item)
                    .is_some_and(|a| !a.system.trim().is_empty() && !a.name.trim().is_empty())
        }
        Decision::Import => {
            state == ItemState::Assessed
                && (item.assessment.dat.as_ref().is_some_and(|d| !d.imported)
                    || item
                        .assessment
                        .support_file
                        .as_ref()
                        .is_some_and(|f| !f.imported))
        }
        Decision::Discard => matches!(
            state,
            ItemState::Assessed
                | ItemState::AlreadyHeld
                | ItemState::Unreadable
                | ItemState::Unsupported
                | ItemState::Dismissed
        ),
        Decision::Dismiss => matches!(
            state,
            ItemState::Assessed
                | ItemState::AlreadyHeld
                | ItemState::Unreadable
                | ItemState::Unsupported
        ),
    }
}

/// The items a decision applies to, among those it was asked for, and how many it leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bulk {
    pub items: Vec<i64>,
    pub left_out: usize,
}

/// Chooses, among `items`, those `decision` applies to.
pub fn choose<'a>(
    decision: Decision,
    items: impl IntoIterator<Item = &'a IngestItem>,
    choices: &Choices,
) -> Bulk {
    let mut bulk = Bulk::default();
    for item in items {
        if applies(decision, item, choices) {
            bulk.items.push(item.id);
        } else {
            bulk.left_out += 1;
        }
    }
    bulk
}

/// The plan request a decision makes for the items `bulk` chose, or none for a dismissal or when it chose none.
pub fn request(
    decision: Decision,
    bulk: &Bulk,
    items: &[IngestItem],
    choices: &Choices,
) -> Option<PlanIngest> {
    let action = decision.action()?;
    if bulk.items.is_empty() {
        return None;
    }
    Some(PlanIngest {
        action,
        items: bulk.items.clone(),
        additions: match action {
            IngestAction::Addition => items
                .iter()
                .filter(|i| bulk.items.contains(&i.id))
                .filter_map(|i| choices.addition(i).map(|a| (i.id, a)))
                .collect(),
            _ => BTreeMap::new(),
        },
    })
}

/// The decisions the strip offers for every item of a status.
pub fn offered(status: IngestStatus) -> &'static [Decision] {
    match status {
        IngestStatus::MatchesSourceSet => &[Decision::Dismiss],
        IngestStatus::Duplicate => &[Decision::Discard, Decision::Dismiss],
        IngestStatus::NoDatMatch => &[Decision::Add, Decision::Discard, Decision::Dismiss],
        IngestStatus::ImportFile => &[Decision::Import, Decision::Dismiss],
        IngestStatus::Unreadable => &[Decision::Discard, Decision::Dismiss],
        IngestStatus::Dismissed => &[Decision::Discard],
        IngestStatus::Arriving | IngestStatus::Moving | IngestStatus::Done => &[],
    }
}

/// One status on the strip: how many items show it, and each decision you can apply to all of them, with the items
/// it applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Count {
    pub status: IngestStatus,
    pub items: usize,
    pub decisions: Vec<(Decision, Bulk)>,
}

/// The strip of counts, one per status present, in the legend's order.
pub fn strip(items: &[IngestItem], choices: &Choices) -> Vec<Count> {
    IngestStatus::ALL
        .iter()
        .filter_map(|&status| {
            let of: Vec<&IngestItem> = items
                .iter()
                .filter(|i| self::status(i) == Some(status))
                .collect();
            if of.is_empty() {
                return None;
            }
            let decisions = offered(status)
                .iter()
                .map(|&d| (d, choose(d, of.iter().copied(), choices)))
                .filter(|(_, bulk)| !bulk.items.is_empty())
                .collect();
            Some(Count {
                status,
                items: of.len(),
                decisions,
            })
        })
        .collect()
}

/// A file's size, in bytes or the largest decimal unit it fills.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return if bytes == 1 {
            "1 byte".into()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// The `status` URL value that shows only items of a status.
pub fn slug(status: IngestStatus) -> &'static str {
    match status {
        IngestStatus::Arriving => "arriving",
        IngestStatus::MatchesSourceSet => "source-set",
        IngestStatus::Duplicate => "duplicate",
        IngestStatus::NoDatMatch => "no-dat-match",
        IngestStatus::ImportFile => "dat",
        IngestStatus::Unreadable => "unreadable",
        IngestStatus::Dismissed => "dismissed",
        IngestStatus::Moving => "moving",
        IngestStatus::Done => "done",
    }
}

/// The status a `status` URL value names.
pub fn parse_slug(value: &str) -> Option<IngestStatus> {
    IngestStatus::ALL
        .iter()
        .copied()
        .find(|s| slug(*s) == value)
}

/// Whether the list shows an item of `status` with `only` chosen: that status alone, or, with none chosen, everything
/// still in the folder.
pub fn shown(only: Option<IngestStatus>, status: IngestStatus) -> bool {
    match only {
        Some(only) => only == status,
        None => status != IngestStatus::Done,
    }
}

/// The "Repair {source set}" action of an item that matches a source set: its label, and the source set's repair tab,
/// where a repair that fills from the folder is proposed or reviewed.
pub fn repair_link(status: IngestStatus, item: &IngestItem) -> Option<(String, String)> {
    match (status, &item.assessment.proposal) {
        (IngestStatus::MatchesSourceSet, Some(Proposal::SourceSet { source_set, .. })) => Some((
            format!("Repair {source_set}"),
            tab_href(source_set, PageTab::Repair),
        )),
        _ => None,
    }
}

/// Where one item of a plan stands: waiting for you to approve the plan, being moved, or how its move ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Waiting,
    Moving,
    Moved,
    Left,
    Failed,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Waiting => "Not yet approved",
            Outcome::Moving => "Moving",
            Outcome::Moved => "Moved",
            Outcome::Left => "Left as it is",
            Outcome::Failed => "Failed",
        }
    }
}

/// Where the folder's plans are, for its Plan tab's icon, from its plans newest first: a plan you approved still
/// moving items, or else the newest plan waiting for your approval.
pub fn plan_state(plans: &[IngestPlan]) -> Option<PlanState> {
    if plans
        .iter()
        .any(|p| p.approval.is_some() && p.finished_at.is_none())
    {
        Some(PlanState::Running)
    } else if plans.first().is_some_and(|p| p.approval.is_none()) {
        Some(PlanState::Open)
    } else {
        None
    }
}

/// The plan the Plan tab opens: the one the URL names (`wanted`), or else the newest of `plans`, which come newest
/// first.
pub fn shown_plan(plans: &[IngestPlan], wanted: Option<i64>) -> Option<i64> {
    wanted.or_else(|| plans.first().map(|p| p.id))
}

/// Where the item of `plan` whose outcome, if it has one yet, is `ended` stands.
pub fn outcome(plan: &IngestPlan, ended: Option<&IngestOutcome>) -> Outcome {
    match ended {
        Some(o) if o.error.is_some() => Outcome::Failed,
        Some(o) if o.moved => Outcome::Moved,
        Some(_) => Outcome::Left,
        None if plan.approval.is_none() => Outcome::Waiting,
        None if plan.finished_at.is_none() => Outcome::Moving,
        None => Outcome::Left,
    }
}

/// Where what an item of a plan of `action` made, as `ended` says, can be seen: the DAT revision a DAT import
/// recorded, even when moving the file afterwards failed, or the additions an addition moved into.
pub fn made_link(action: IngestAction, ended: &IngestOutcome) -> Option<Link> {
    match action {
        IngestAction::DatImport => ended
            .dat_revision
            .as_ref()
            .map(|sha| Link::new(format!("/dats?row={sha}"), "Open the DAT revision")),
        IngestAction::Addition => (ended.moved && ended.error.is_none())
            .then(|| Link::new("/additions", "Open the additions")),
        IngestAction::Discard => None,
    }
}

/// Where what `plan` made can be seen, once it made anything: the one DAT revision or additions an item made, or
/// every DAT revision when it imported several.
pub fn plan_made_link(plan: &IngestPlan) -> Option<Link> {
    let mut made = plan
        .outcomes
        .iter()
        .filter_map(|o| made_link(plan.content.action, o));
    let first = made.next()?;
    match (plan.content.action, made.next()) {
        (IngestAction::DatImport, Some(_)) => Some(Link::new("/dats", "Open the DAT revisions")),
        _ => Some(first),
    }
}

/// The folder page's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderTab {
    Items,
    Plan,
    Kept,
}

impl FolderTab {
    pub const ALL: [FolderTab; 3] = [FolderTab::Items, FolderTab::Plan, FolderTab::Kept];

    /// The tab the URL's `tab` names, or else, as an older link has it, the Plan tab when `plan` names a plan.
    pub fn parse(tab: Option<&str>, plan: Option<&str>) -> Self {
        match tab {
            Some("plan") => FolderTab::Plan,
            Some("kept") => FolderTab::Kept,
            None if plan.is_some_and(|p| p.parse::<i64>().is_ok()) => FolderTab::Plan,
            _ => FolderTab::Items,
        }
    }

    /// The `tab` URL value; the items, the default, have none.
    pub fn key(self) -> Option<&'static str> {
        match self {
            FolderTab::Items => None,
            FolderTab::Plan => Some("plan"),
            FolderTab::Kept => Some("kept"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FolderTab::Items => "Items",
            FolderTab::Plan => "Plan",
            FolderTab::Kept => "Kept files",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::ingest::ItemMove;

    #[test]
    fn a_dat_shows_its_name_not_its_checksum_but_a_game_file_keeps_its_own() {
        let game = Assessment {
            sha256: Some("ab".repeat(32)),
            ..Assessment::default()
        };
        assert_eq!(item_checksum(&game), Some("ab".repeat(32).as_str()));
        let dat: Assessment = serde_json::from_value(serde_json::json!({
            "sha256": "cd".repeat(32),
            "dat": {"name": "Atari - 2600", "version": "1", "sha256": "cd".repeat(32),
                    "dat": "No-Intro/Atari - 2600", "known": false, "imported": false},
        }))
        .unwrap();
        assert_eq!(item_checksum(&dat), None);

        let plan_item = |dat: Option<&str>, dat_file: bool| -> IngestPlanItem {
            serde_json::from_value(serde_json::json!({
                "item": 1, "name": "x", "identity": {"dev": 1, "ino": 1},
                "file": {"size": 1, "mtime_ns": 1, "nlink": 1}, "ctime_ns": 1,
                "sha256": "ef".repeat(32), "destination": "d", "reason": "r", "dat": dat,
                "dat_file": dat_file,
            }))
            .unwrap()
        };
        let game = plan_item(None, false);
        assert_eq!(plan_checksum(&game), Some("ef".repeat(32).as_str()));
        let import = plan_item(Some("No-Intro/Atari - 2600"), true);
        assert_eq!(plan_checksum(&import), None);
        let discarded_dat = plan_item(None, true);
        assert_eq!(plan_checksum(&discarded_dat), None);
    }

    fn item(id: i64, name: &str, state: &str, assessment: serde_json::Value) -> IngestItem {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "state": state,
            "file": {"size": 4, "mtime_ns": 0},
            "identity": {"dev": 1, "ino": id},
            "first_seen_at": "2026-01-01T00:00:00Z",
            "assessment": assessment,
        }))
        .unwrap()
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({"size": 4, "crc": "c", "md5": "m", "sha1": "s", "sha256": "h"})
    }

    fn homebrew(id: i64, system: Option<&str>) -> IngestItem {
        item(
            id,
            &format!("hack {id}.a26"),
            "assessed",
            serde_json::json!({"payloads": [payload()], "proposal": {"kind": "addition", "system": system}}),
        )
    }

    fn fill(id: i64) -> IngestItem {
        item(
            id,
            "pong.a26",
            "assessed",
            serde_json::json!({"payloads": [payload()], "proposal": {
                "kind": "source_set", "source_set": "Atari", "game": "Pong", "need": "missing"}}),
        )
    }

    #[test]
    fn an_item_matching_a_source_set_offers_its_repair_tab() {
        assert_eq!(
            repair_link(IngestStatus::MatchesSourceSet, &fill(1)),
            Some((
                "Repair Atari".to_string(),
                "/source-sets/Atari?tab=repair".to_string()
            ))
        );
        assert_eq!(repair_link(IngestStatus::Moving, &fill(1)), None);
        assert_eq!(
            repair_link(IngestStatus::ImportFile, &homebrew(2, None)),
            None
        );
    }

    fn dat(id: i64, provider: Option<&str>, imported: bool) -> IngestItem {
        let key = format!("{}/Atari", provider.unwrap_or("unknown-logiqx"));
        item(
            id,
            "new.dat",
            if imported { "already_held" } else { "assessed" },
            serde_json::json!({"dat": {"name": "Atari", "version": "2", "sha256": "d",
                "dat": key, "provider": provider, "imported": imported},
                "proposal": {"kind": "dat_import"}}),
        )
    }

    fn duplicate(id: i64) -> IngestItem {
        item(id, "pong copy.a26", "already_held", serde_json::json!({}))
    }

    fn being_moved(mut item: IngestItem, under_way: bool) -> IngestItem {
        item.last_move = Some(ItemMove {
            plan: Some(9),
            repair: None,
            job: Some(4),
            under_way,
            finished_at: None,
            error: None,
            note: None,
        });
        item
    }

    #[test]
    fn an_item_shows_what_it_is_and_a_move_under_way_shows_over_its_state() {
        let mut moved = homebrew(6, None);
        moved.state = ItemState::Moved;
        let cases = [
            (fill(1), Some(IngestStatus::MatchesSourceSet)),
            (homebrew(2, None), Some(IngestStatus::NoDatMatch)),
            (dat(3, None, false), Some(IngestStatus::ImportFile)),
            (duplicate(4), Some(IngestStatus::Duplicate)),
            (
                item(5, "x.7z", "unsupported", serde_json::json!({})),
                Some(IngestStatus::Unreadable),
            ),
            (
                item(5, "x.zip", "failed", serde_json::json!({})),
                Some(IngestStatus::Unreadable),
            ),
            (
                item(5, "x.a26", "stale", serde_json::json!({})),
                Some(IngestStatus::Arriving),
            ),
            (
                item(5, "x.a26", "dismissed", serde_json::json!({})),
                Some(IngestStatus::Dismissed),
            ),
            (moved, Some(IngestStatus::Done)),
            (item(7, "y.a26", "gone", serde_json::json!({})), None),
            (
                being_moved(homebrew(8, Some("Atari")), true),
                Some(IngestStatus::Moving),
            ),
            (
                being_moved(homebrew(9, Some("Atari")), false),
                Some(IngestStatus::NoDatMatch),
            ),
        ];
        for (item, expected) in cases {
            assert_eq!(status(&item), expected, "{item:?}");
        }
    }

    #[test]
    fn adding_as_additions_takes_only_items_with_one_file_and_a_system() {
        let mut two_files = homebrew(3, Some("Atari"));
        two_files
            .assessment
            .payloads
            .push(two_files.assessment.payloads[0].clone());
        let items = [
            homebrew(1, Some("Atari")),
            homebrew(2, None),
            two_files,
            fill(4),
            being_moved(homebrew(5, Some("Atari")), true),
        ];
        let bulk = choose(Decision::Add, &items, &Choices::default());
        assert_eq!(
            bulk,
            Bulk {
                items: vec![1],
                left_out: 4
            }
        );

        let mut choices = Choices::default();
        choices.additions.insert(
            2,
            NewAddition {
                system: "Atari/2600".into(),
                name: "My Hack".into(),
                parent: None,
            },
        );
        let bulk = choose(Decision::Add, &items, &choices);
        assert_eq!(bulk.items, [1, 2], "a system you chose makes it addable");
        let plan = request(Decision::Add, &bulk, &items, &choices).unwrap();
        assert_eq!(plan.action, IngestAction::Addition);
        assert_eq!(plan.items, [1, 2]);
        assert_eq!(plan.additions[&1].system, "Atari");
        assert_eq!(plan.additions[&1].name, "hack 1", "named after its file");
        assert_eq!(plan.additions[&2].name, "My Hack");
    }

    #[test]
    fn importing_dats_asks_for_no_provider_whether_or_not_the_header_names_one() {
        let items = [
            dat(1, Some("No-Intro"), false),
            dat(2, None, false),
            dat(3, Some("No-Intro"), true),
            homebrew(4, Some("Atari")),
        ];
        let bulk = choose(Decision::Import, &items, &Choices::default());
        assert_eq!(bulk.items, [1, 2]);
        let plan = request(Decision::Import, &bulk, &items, &Choices::default()).unwrap();
        assert_eq!(
            (plan.action, plan.items),
            (IngestAction::DatImport, vec![1, 2])
        );
    }

    #[test]
    fn discarding_takes_anything_settled_and_dismissing_anything_waiting_on_you() {
        let items = [
            duplicate(1),
            item(2, "x.7z", "unsupported", serde_json::json!({})),
            item(3, "y.a26", "dismissed", serde_json::json!({})),
            item(4, "z.a26", "arriving", serde_json::json!({})),
            item(5, "w.zip", "failed", serde_json::json!({})),
            being_moved(duplicate(6), true),
            being_moved(duplicate(7), false),
        ];
        let discard = choose(Decision::Discard, &items, &Choices::default());
        assert_eq!(discard.items, [1, 2, 3, 7]);
        assert_eq!(discard.left_out, 3);
        let dismiss = choose(Decision::Dismiss, &items, &Choices::default());
        assert_eq!(dismiss.items, [1, 2, 7]);
        assert_eq!(
            request(Decision::Dismiss, &dismiss, &items, &Choices::default()),
            None,
            "a dismissal is no plan"
        );
        let none = choose(Decision::Discard, &items[3..4], &Choices::default());
        assert_eq!(
            request(Decision::Discard, &none, &items, &Choices::default()),
            None
        );
    }

    #[test]
    fn the_strip_counts_each_status_and_offers_what_applies_to_all_of_it() {
        let items = [
            homebrew(1, Some("Atari")),
            homebrew(2, None),
            duplicate(3),
            duplicate(4),
            being_moved(homebrew(5, Some("Atari")), true),
            fill(6),
        ];
        let strip = strip(&items, &Choices::default());
        type Shown = (IngestStatus, usize, Vec<(Decision, Vec<i64>)>);
        let shown: Vec<Shown> = strip
            .into_iter()
            .map(|c| {
                (
                    c.status,
                    c.items,
                    c.decisions.into_iter().map(|(d, b)| (d, b.items)).collect(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    IngestStatus::MatchesSourceSet,
                    1,
                    vec![(Decision::Dismiss, vec![6])]
                ),
                (
                    IngestStatus::Duplicate,
                    2,
                    vec![
                        (Decision::Discard, vec![3, 4]),
                        (Decision::Dismiss, vec![3, 4])
                    ]
                ),
                (
                    IngestStatus::NoDatMatch,
                    2,
                    vec![
                        (Decision::Add, vec![1]),
                        (Decision::Discard, vec![1, 2]),
                        (Decision::Dismiss, vec![1, 2])
                    ]
                ),
                (IngestStatus::Moving, 1, vec![]),
            ]
        );
    }

    #[test]
    fn a_support_file_is_imported_as_a_dat_is_until_it_is_held() {
        let support = |imported: bool| {
            item(
                1,
                "catver.ini",
                if imported { "already_held" } else { "assessed" },
                serde_json::json!({"support_file": {"kind": "catver", "mame_version": "0.289",
                    "imported": imported}, "proposal": {"kind": "support_file_import"}}),
            )
        };
        let choices = Choices::default();
        let new = support(false);
        assert_eq!(status(&new), Some(IngestStatus::ImportFile));
        assert!(applies(Decision::Import, &new, &choices));
        assert!(!applies(Decision::Add, &new, &choices));
        let held = support(true);
        assert_eq!(status(&held), Some(IngestStatus::Duplicate));
        assert!(!applies(Decision::Import, &held, &choices));
    }

    #[test]
    fn labels_count_the_items() {
        assert_eq!(Decision::Add.label(1), "Add 1 as an addition");
        assert_eq!(Decision::Add.label(3), "Add 3 as additions");
        assert_eq!(Decision::Import.label(2), "Import 2 files");
        assert_eq!(
            Decision::Discard.left_out(2),
            "2 items can't be discarded yet: they're still arriving, being moved or gone."
        );
    }

    #[test]
    fn the_list_shows_what_is_still_in_the_folder_unless_you_choose_a_status() {
        assert!(shown(None, IngestStatus::Moving));
        assert!(!shown(None, IngestStatus::Done));
        assert!(shown(Some(IngestStatus::Done), IngestStatus::Done));
        assert!(!shown(Some(IngestStatus::Done), IngestStatus::Duplicate));
        for status in IngestStatus::ALL {
            assert_eq!(parse_slug(slug(*status)), Some(*status));
        }
        assert_eq!(parse_slug("everything"), None);
    }

    #[test]
    fn sizes_read_in_the_largest_unit_they_fill() {
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_000), "1.0 KB");
        assert_eq!(size(999_960), "1.0 MB");
        assert_eq!(size(4_700_000_000), "4.7 GB");
    }
    #[test]
    fn a_plan_item_waits_for_approval_then_moves_then_ends_as_its_outcome_says() {
        let mut plan: IngestPlan = serde_json::from_value(serde_json::json!({
            "id": 3, "digest": "d", "created_at": "2026-01-01T00:00:00Z",
            "content": {"ingest_folder": 1, "root": "in", "action": "discard", "generation": 1,
                        "items": [], "fingerprint": []},
        }))
        .unwrap();
        let ended = |moved: bool, error: Option<&str>| IngestOutcome {
            item: 1,
            name: "x".into(),
            moved,
            detail: None,
            error: error.map(Into::into),
            dat_revision: None,
        };
        assert_eq!(outcome(&plan, None), Outcome::Waiting);
        plan.approval = serde_json::from_value(
            serde_json::json!({"digest": "d", "actor": "Operator", "at": "t"}),
        )
        .unwrap();
        assert_eq!(outcome(&plan, None), Outcome::Moving);
        assert_eq!(outcome(&plan, Some(&ended(true, None))), Outcome::Moved);
        assert_eq!(outcome(&plan, Some(&ended(false, None))), Outcome::Left);
        assert_eq!(
            outcome(&plan, Some(&ended(false, Some("gone")))),
            Outcome::Failed
        );
        plan.finished_at = Some("t".into());
        assert_eq!(outcome(&plan, None), Outcome::Left, "never reached");
    }

    #[test]
    fn a_moved_item_links_to_what_it_made() {
        let ended = |moved: bool, dat_revision: Option<&str>| IngestOutcome {
            item: 1,
            name: "x".into(),
            moved,
            detail: None,
            error: None,
            dat_revision: dat_revision.map(Into::into),
        };
        assert_eq!(
            made_link(IngestAction::DatImport, &ended(true, Some("abc"))),
            Some(Link::new("/dats?row=abc", "Open the DAT revision"))
        );
        assert_eq!(
            made_link(IngestAction::DatImport, &ended(true, None)),
            None,
            "a support file makes no DAT revision"
        );
        let mut unmoved = ended(false, Some("abc"));
        unmoved.error = Some("the move failed".into());
        assert_eq!(
            made_link(IngestAction::DatImport, &unmoved),
            Some(Link::new("/dats?row=abc", "Open the DAT revision")),
            "a DAT recorded before its move failed is still imported"
        );
        assert_eq!(
            made_link(IngestAction::Addition, &ended(true, None)),
            Some(Link::new("/additions", "Open the additions"))
        );
        assert_eq!(made_link(IngestAction::Addition, &ended(false, None)), None);
        assert_eq!(made_link(IngestAction::Discard, &ended(true, None)), None);

        let mut plan: IngestPlan = serde_json::from_value(serde_json::json!({
            "id": 3, "digest": "d", "created_at": "2026-01-01T00:00:00Z",
            "content": {"ingest_folder": 1, "root": "in", "action": "dat_import", "generation": 1,
                        "items": [], "fingerprint": []},
        }))
        .unwrap();
        assert_eq!(plan_made_link(&plan), None, "nothing moved yet");
        plan.outcomes = vec![ended(false, None), ended(true, Some("abc"))];
        assert_eq!(
            plan_made_link(&plan),
            Some(Link::new("/dats?row=abc", "Open the DAT revision"))
        );
        plan.outcomes.push(ended(true, Some("def")));
        assert_eq!(
            plan_made_link(&plan),
            Some(Link::new("/dats", "Open the DAT revisions"))
        );
    }

    #[test]
    fn arriving_items_say_when_they_are_next_checked() {
        let folder = |next: Option<&str>, every: Option<u64>| -> IngestFolder {
            serde_json::from_value(serde_json::json!({
                "root": "drop", "path": "/drop", "settle_secs": 60,
                "scan_every_minutes": every, "next_check_at": next,
            }))
            .unwrap()
        };
        assert_eq!(
            arriving_note(&folder(Some("2026-11-12T04:13:05.25Z"), Some(5))),
            "Assessed once settled · next check 2026-11-12 04:13:05 UTC"
        );
        assert_eq!(
            arriving_note(&folder(None, Some(1))),
            "Assessed once settled · scanned every minute"
        );
        assert_eq!(
            arriving_note(&folder(None, Some(5))),
            "Assessed once settled · scanned every 5 minutes"
        );
        assert_eq!(
            arriving_note(&folder(None, None)),
            "Assessed once settled · at the next scan"
        );
    }

    #[test]
    fn a_match_outside_the_coverage_proposes_nothing_and_says_why() {
        assert_eq!(
            source_set_match("GB", "Pong (Japan)", Need::OutsideCoverage),
            "Outside your coverage"
        );
        assert_eq!(
            source_set_match("GB", "Pong (Japan)", Need::CoverageUnavailable),
            "Coverage unknown"
        );
        assert_eq!(
            source_set_match("GB", "Pong (USA)", Need::Missing),
            "Pong (USA) in GB"
        );
    }

    fn plan(id: i64, approved: bool, finished: bool) -> IngestPlan {
        serde_json::from_value(serde_json::json!({
            "id": id, "digest": "d", "created_at": "2026-01-01T00:00:00Z",
            "content": {"ingest_folder": 1, "root": "in", "action": "discard", "generation": 1,
                        "items": [], "fingerprint": []},
            "approval": approved.then(|| serde_json::json!({"digest": "d", "actor": "Operator", "at": "t"})),
            "finished_at": finished.then_some("2026-01-02T00:00:00Z"),
        }))
        .unwrap()
    }

    #[test]
    fn the_plan_tab_shows_a_plan_moving_items_or_the_newest_waiting_on_you() {
        assert_eq!(plan_state(&[]), None);
        assert_eq!(
            plan_state(&[plan(2, true, true), plan(1, false, false)]),
            None
        );
        assert_eq!(
            plan_state(&[plan(2, false, false), plan(1, true, true)]),
            Some(PlanState::Open)
        );
        assert_eq!(
            plan_state(&[plan(2, false, false), plan(1, true, false)]),
            Some(PlanState::Running)
        );
    }

    #[test]
    fn the_plan_tab_opens_the_plan_the_url_names_or_else_the_newest() {
        let plans = [plan(5, false, false), plan(4, true, true)];
        assert_eq!(shown_plan(&plans, Some(4)), Some(4));
        assert_eq!(
            shown_plan(&plans, Some(9)),
            Some(9),
            "an older plan than the list holds still opens"
        );
        assert_eq!(shown_plan(&plans, None), Some(5));
        assert_eq!(shown_plan(&[], None), None);
    }

    #[test]
    fn an_old_link_to_a_plan_opens_the_plan_tab() {
        assert_eq!(FolderTab::parse(None, None), FolderTab::Items);
        assert_eq!(FolderTab::parse(Some("kept"), None), FolderTab::Kept);
        assert_eq!(FolderTab::parse(Some("plan"), None), FolderTab::Plan);
        assert_eq!(FolderTab::parse(None, Some("7")), FolderTab::Plan);
        assert_eq!(
            FolderTab::parse(None, Some("x")),
            FolderTab::Items,
            "only a plan's id names a plan"
        );
        assert_eq!(
            FolderTab::parse(Some("kept"), Some("7")),
            FolderTab::Kept,
            "a tab the URL names wins"
        );
    }
}
