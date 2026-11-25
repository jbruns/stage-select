//! The repair screen's pure logic: which repair to show when it opens, when to fetch a plan again, what to say
//! about one left unfinished, what you changed since the plan shown was made, and setting one choice for many items.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use stage_select_api::additions::NewAddition;
use stage_select_api::repair::{RepairChoice, RepairItem, RepairPlan, RepairStep};
use stage_select_api::report::{Finding, GameStatus};

use crate::parts::plan_model::{self, BulkOutcome, Does};
use crate::parts::status::{ItemOutcome, PlanKind, Status};
use crate::screens::additions::new_addition;

/// The loads of the plan to show, in the order they were asked for. One rule keeps the screen true to the server:
/// only the last load's answer is shown, and the answer to a change made to a plan (approving it, planning again,
/// refreshing a running repair) only while no load has been asked for since. A change whose answer is dropped
/// [reloads](Loads::reload) the plan wanted, so the screen still ends up showing what the server holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Loads {
    /// The plan last asked to be shown, once one has been: `Some(None)` for the open repair.
    requested: Option<Option<i64>>,
    /// The source set's open repair, the last time it had one.
    last_open: Option<i64>,
    seq: u64,
}

/// A load asked for, to check its answer against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadTicket {
    seq: u64,
    /// The plan to show: none for whichever approved repair hasn't finished.
    pub wanted: Option<i64>,
}

/// When a change to a plan was made, to check its answer against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeMark(u64);

impl Loads {
    /// Asks for the plan the URL `named`, or else the source set's `open` repair, while `shown` (approved or not) is
    /// shown: a ticket to fetch it with, or none when it was asked for last or is already shown. Asking for another
    /// plan than last time drops what is under way, loads and changes alike. An approved repair that was the open
    /// one stays shown once it finishes, with what happened to each item, until another plan is asked for.
    pub fn ask(
        &mut self,
        named: Option<i64>,
        open: Option<i64>,
        shown: Option<(i64, bool)>,
    ) -> Option<LoadTicket> {
        if open.is_some() {
            self.last_open = open;
        }
        let finished = shown
            .filter(|&(id, approved)| approved && Some(id) == self.last_open)
            .map(|(id, _)| id);
        let wanted = named.or(open).or(finished);
        if self.requested == Some(wanted) {
            return None;
        }
        self.requested = Some(wanted);
        self.seq += 1;
        (wanted.is_none() || wanted != shown.map(|(id, _)| id)).then_some(LoadTicket {
            seq: self.seq,
            wanted,
        })
    }

    /// Loads the plan last asked for again, even if it is shown, in place of whatever is under way: after the answer
    /// to a change was dropped. None before any was asked for.
    pub fn reload(&mut self) -> Option<LoadTicket> {
        let wanted = self.requested?;
        self.seq += 1;
        Some(LoadTicket {
            seq: self.seq,
            wanted,
        })
    }

    /// Whether the answer to `ticket` is still wanted.
    pub fn applies(&self, ticket: LoadTicket) -> bool {
        ticket.seq == self.seq
    }

    /// Marks a change made now to the plan shown.
    pub fn mark(&self) -> ChangeMark {
        ChangeMark(self.seq)
    }

    /// Whether the answer to the change `mark`ed is still to be shown: when no load was asked for since.
    pub fn change_applies(&self, mark: ChangeMark) -> bool {
        mark.0 == self.seq
    }

    /// The answer to a change is shown, giving back the plan `proposed` when it was a proposal (which the URL then
    /// names): a load asked for earlier no longer counts, and asking for that plan next fetches nothing.
    pub fn accepted(&mut self, proposed: Option<i64>) {
        self.seq += 1;
        if let Some(id) = proposed {
            self.requested = Some(Some(id));
        }
    }
}

/// What an audit found, counted on the Repair tab before any repair is proposed: entries with a finding, or games
/// with a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    Entries(Finding),
    Games(GameStatus),
}

/// How many of one thing the latest audit found that a repair could change, and the kind of change it would be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CouldChange {
    pub found: Found,
    pub n: usize,
    pub kind: PlanKind,
}

impl CouldChange {
    /// What its tile says under its count.
    pub fn what(self) -> &'static str {
        match self.found {
            Found::Entries(Finding::Misnamed) => "Misnamed entries to rename",
            Found::Entries(Finding::Conflicting) => "Conflicting entries to rename",
            Found::Entries(Finding::Mismatched) => "Mismatched entries to replace",
            Found::Entries(Finding::Unreadable) => "Unreadable entries to replace",
            Found::Entries(Finding::Unmatched) => "Unmatched entries to set aside",
            Found::Entries(Finding::Matched) => "Matched entries",
            Found::Games(GameStatus::Missing) => "Missing games to fill",
            Found::Games(GameStatus::Incomplete) => "Incomplete games to complete",
            Found::Games(_) => "Games",
        }
    }

    /// The query pairs that narrow the source set's Entries or Games tab to what it counts. Where its Coverage
    /// applies (`covered`), its games are counted only in it, and so are those the tab lists.
    pub fn filter(self, covered: bool) -> Vec<(&'static str, &'static str)> {
        match self.found {
            Found::Entries(f) => vec![(crate::model::FINDING, f.as_str())],
            Found::Games(s) => {
                let mut pairs = vec![(crate::model::STATUS, s.as_str())];
                if covered {
                    pairs.push((crate::model::COVERED, crate::model::COVERED_YES));
                }
                pairs
            }
        }
    }
}

/// What of an audit's `findings` and `games` a repair could change, in the order a plan lists its kinds: misnamed and
/// conflicting entries are renamed, mismatched and unreadable ones replaced from an Ingest Folder, unmatched ones set
/// aside, and missing and incomplete games filled. Those it found none of are left out. What an Audit doesn't count
/// ahead of a plan, spare copies of matched ROMs to set aside and games to repack or convert per the storage policy,
/// [`repair_lead`] names instead.
pub fn could_change(
    findings: &BTreeMap<Finding, usize>,
    games: &BTreeMap<GameStatus, usize>,
) -> Vec<CouldChange> {
    let entries = [
        (Finding::Misnamed, PlanKind::Rename),
        (Finding::Conflicting, PlanKind::Rename),
        (Finding::Mismatched, PlanKind::Replace),
        (Finding::Unreadable, PlanKind::Replace),
        (Finding::Unmatched, PlanKind::SetAside),
    ]
    .map(|(f, kind)| (Found::Entries(f), findings.get(&f).copied(), kind));
    let games = [GameStatus::Missing, GameStatus::Incomplete]
        .map(|s| (Found::Games(s), games.get(&s).copied(), PlanKind::Fill));
    entries
        .into_iter()
        .chain(games)
        .filter_map(|(found, n, kind)| n.filter(|n| *n > 0).map(|n| CouldChange { found, n, kind }))
        .collect()
}

/// The line the Repair tab leads with before any repair is proposed, whether the source set is `audited` and what of
/// its latest audit a repair `could` change.
pub fn repair_lead(audited: bool, could: &[CouldChange]) -> &'static str {
    match (audited, could.is_empty()) {
        (false, _) => {
            "Not audited yet. A repair is proposed from an audit, so audit the source set first."
        }
        (true, true) => {
            "The latest audit found no entries to rename, replace or set aside, and no missing or incomplete games. \
             A repair may still set aside spare copies or repack games to your storage policy, which an audit \
             doesn't count: propose one to see what it would do."
        }
        (true, false) => {
            "The latest audit found these, which a repair could change. It may also set aside spare copies or \
             repack games to your storage policy, which an audit doesn't count: propose one to see everything it \
             would do."
        }
    }
}

/// Whether a new repair may be proposed while `shown` (approved, finished) is shown and the source set is or isn't
/// `running` an approved repair: when none is running, and no plan is shown or the one shown has finished. A plan
/// still open is planned again instead.
pub fn may_propose(shown: Option<(bool, bool)>, running: bool) -> bool {
    !running && shown.is_none_or(|(_, finished)| finished)
}

/// The repair to show when the repair screen opens, from the source set's plans: the one `wanted` names, if any;
/// otherwise the latest approved repair that hasn't finished, which is shown instead of proposing a new one.
pub fn repair_to_show(plans: &[RepairPlan], wanted: Option<i64>) -> Option<&RepairPlan> {
    match wanted {
        Some(id) => plans.iter().find(|p| p.id == id),
        None => plans
            .iter()
            .filter(|p| p.approval.is_some() && p.finished_at.is_none())
            .max_by_key(|p| p.id),
    }
}

/// Whether to fetch the plan again once its job `job` is seen to end: only while the plan shown is unfinished, and
/// only once for each job, so a repair left unfinished isn't fetched over and over. `fetched` is the job the plan
/// was last fetched for.
pub fn fetch_after(job: i64, ended: bool, unfinished: bool, fetched: Option<i64>) -> bool {
    ended && unfinished && fetched != Some(job)
}

/// What to tell you about an approved repair left unfinished: what stopped it, and that it's tried again when the
/// service starts.
pub fn unfinished_note(plan: &RepairPlan) -> Option<String> {
    if plan.approval.is_none() || plan.finished_at.is_some() {
        return None;
    }
    let why = plan.recovery_error.as_deref()?.trim_end_matches('.');
    Some(format!(
        "This repair isn't finished. What stopped it: {why}. Stage Select tries it again when the service \
         starts, and until it finishes, the source set's audits and builds wait for it. Fix what's blocking it, \
         then restart the service."
    ))
}

/// The choices `plan` was made with that differ from each item's default, by key: what the screen starts from when
/// it shows a plan it didn't make, so that setting one back to its default is a change.
pub fn chosen(plan: &RepairPlan) -> BTreeMap<String, RepairChoice> {
    plan.content
        .items
        .iter()
        .filter(|item| item.chosen != item.default)
        .map(|item| (item.key.clone(), item.chosen))
        .collect()
}

/// The choices to plan again with: those `plan` was asked for with (`asked`), with what you picked on it since. A
/// pick of an item's default needs no asking, as an item without a choice takes its default.
pub fn requested(
    plan: &RepairPlan,
    asked: &BTreeMap<String, RepairChoice>,
    picked: &BTreeMap<String, RepairChoice>,
) -> BTreeMap<String, RepairChoice> {
    let mut choices = asked.clone();
    for item in &plan.content.items {
        match picked.get(&item.key) {
            Some(c) if *c == item.default => {
                choices.remove(&item.key);
            }
            Some(c) => {
                choices.insert(item.key.clone(), *c);
            }
            None => {}
        }
    }
    choices
}

/// The additions to ask for when planning again from `plan` with `choices`: the fields of each item chosen
/// to be added as one. An item without a choice takes its default. Fields you haven't opened are what the plan adds
/// it as, or else its suggested name in `system`, the source set's DAT.
pub fn to_add(
    plan: &RepairPlan,
    choices: &BTreeMap<String, RepairChoice>,
    adding: &BTreeMap<String, NewAddition>,
    system: &str,
) -> BTreeMap<String, NewAddition> {
    plan.content
        .items
        .iter()
        .filter(|item| {
            choices.get(&item.key).copied().unwrap_or(item.default) == RepairChoice::AddAddition
        })
        .map(|item| {
            let fields = adding
                .get(&item.key)
                .or(item.addition.as_ref())
                .cloned()
                .unwrap_or_else(|| suggested_addition(item, system));
            (item.key.clone(), fields)
        })
        .collect()
}

/// What `item` is added as until you say otherwise: an addition named from its file, in `system`.
pub fn suggested_addition(item: &RepairItem, system: &str) -> NewAddition {
    let path = item.path.as_deref().unwrap_or(&item.key);
    new_addition(path, Some(system))
}

/// The items you changed since `plan` was shown, by key: one you picked otherwise than the plan does, or an
/// addition it adds, edited. Approving approves the plan as it was made, so it waits until you plan again with them.
pub fn unplanned(
    plan: &RepairPlan,
    picked: &BTreeMap<String, RepairChoice>,
    adding: &BTreeMap<String, NewAddition>,
) -> Vec<String> {
    plan.content
        .items
        .iter()
        .filter(|item| {
            picked.get(&item.key).is_some_and(|c| *c != item.chosen)
                || (item.chosen == RepairChoice::AddAddition
                    && item
                        .addition
                        .as_ref()
                        .is_some_and(|added| adding.get(&item.key).is_some_and(|a| a != added)))
        })
        .map(|item| item.key.clone())
        .collect()
}

/// The choice `item` takes now: your pick, or else what the plan shown does.
pub fn current(item: &RepairItem, picked: &BTreeMap<String, RepairChoice>) -> RepairChoice {
    picked.get(&item.key).copied().unwrap_or(item.chosen)
}

/// The choices of `item` you can set for many items at once: adding as an addition needs each item's own name, so
/// it's set one item at a time.
pub fn bulk_offers(item: &RepairItem) -> Vec<RepairChoice> {
    item.choices
        .iter()
        .copied()
        .filter(|c| *c != RepairChoice::AddAddition)
        .collect()
}

fn of_kind(plan: &RepairPlan, kind: PlanKind) -> impl Iterator<Item = &RepairItem> {
    plan.content
        .items
        .iter()
        .filter(move |i| PlanKind::from(i.kind) == kind)
}

/// The choices "apply to all" offers for `kind`: those any item of that kind offers. One that only some offer sets
/// only those.
pub fn kind_choices(plan: &RepairPlan, kind: PlanKind) -> Vec<RepairChoice> {
    let offers: Vec<Vec<RepairChoice>> = of_kind(plan, kind).map(bulk_offers).collect();
    plan_model::offered_by_any(offers.iter().map(Vec::as_slice))
}

/// The choices you can set for the items `keys` names: those any of them offers.
pub fn choices_for(plan: &RepairPlan, keys: &[String]) -> Vec<RepairChoice> {
    let keys: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
    let offers: Vec<Vec<RepairChoice>> = plan
        .content
        .items
        .iter()
        .filter(|i| keys.contains(i.key.as_str()))
        .map(bulk_offers)
        .collect();
    plan_model::offered_by_any(offers.iter().map(Vec::as_slice))
}

fn apply<'a>(
    items: impl Iterator<Item = &'a RepairItem>,
    choice: RepairChoice,
    picked: &mut BTreeMap<String, RepairChoice>,
) -> BulkOutcome {
    let offers: Vec<(&RepairItem, Vec<RepairChoice>)> =
        items.map(|i| (i, bulk_offers(i))).collect();
    plan_model::apply_choice(
        offers
            .iter()
            .map(|(i, o)| (i.key.as_str(), i.chosen, o.as_slice())),
        choice,
        picked,
    )
}

/// Sets `choice` for every item of `kind` that offers it; no choice ("no change") changes nothing.
pub fn apply_to_kind(
    plan: &RepairPlan,
    kind: PlanKind,
    choice: Option<RepairChoice>,
    picked: &mut BTreeMap<String, RepairChoice>,
) -> Option<BulkOutcome> {
    Some(apply(of_kind(plan, kind), choice?, picked))
}

/// Sets `choice` for every item `keys` names that offers it: the selected rows, or every row matching a filter.
pub fn apply_to_keys(
    plan: &RepairPlan,
    keys: &[String],
    choice: RepairChoice,
    picked: &mut BTreeMap<String, RepairChoice>,
) -> BulkOutcome {
    let keys: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
    apply(
        plan.content
            .items
            .iter()
            .filter(|i| keys.contains(i.key.as_str())),
        choice,
        picked,
    )
}

/// What a repair does to an item of `kind`, for the plan's summary line.
fn does(kind: PlanKind) -> Does {
    match kind {
        PlanKind::Rename => Does::new("renames", "entry", "entries"),
        PlanKind::Repack => Does::new("repacks", "entry", "entries"),
        PlanKind::Fill => Does::new("fills", "game", "games"),
        PlanKind::Replace => Does::new("replaces", "entry", "entries"),
        PlanKind::SetAside => Does::new("sets aside", "file", "files"),
        PlanKind::Convert => Does::new("converts", "entry", "entries"),
    }
}

/// The plan's one-line summary in numbers: what each item is chosen to do, in the order of the kinds, then the
/// additions it adds and the items it leaves as they are.
pub fn plan_line(plan: &RepairPlan) -> String {
    let others = PlanKind::ALL.len();
    let mut doing: Vec<(usize, Does)> = plan
        .content
        .items
        .iter()
        .map(|item| match item.chosen {
            RepairChoice::Apply => {
                let kind = PlanKind::from(item.kind);
                (kind.rank(), does(kind))
            }
            RepairChoice::KeepAside => (PlanKind::SetAside.rank(), does(PlanKind::SetAside)),
            RepairChoice::AddAddition => (others, Does::new("adds", "addition", "additions")),
            RepairChoice::Leave => (
                others + 1,
                Does::new("leaves", "item as it is", "items as they are"),
            ),
        })
        .collect();
    doing.sort_by_key(|(rank, _)| *rank);
    plan_model::plan_line(doing.into_iter().map(|(_, d)| (d, 1)), "Changes nothing")
}

/// What became of `item` of `plan` once approved: its outcome as the repair recorded it; or, once the repair
/// finished without one, not done. An item left as it is has none.
pub fn item_outcome(plan: &RepairPlan, item: &RepairItem) -> Option<ItemOutcome> {
    match plan.outcomes.iter().find(|o| o.item == item.key) {
        Some(o) if o.error.is_some() => Some(ItemOutcome::Failed),
        Some(o) if o.done => Some(ItemOutcome::Done),
        Some(_) => Some(ItemOutcome::NotDone),
        None if plan.finished_at.is_some() && item.chosen != RepairChoice::Leave => {
            Some(ItemOutcome::NotDone)
        }
        None => None,
    }
}

/// What `plan`'s finished repair says of the incremental audit it queued. The audit's own outcome shows once, where
/// the source set's audits show, so this only says it is queued.
pub fn follow_up_audit(plan: &RepairPlan) -> Option<String> {
    match (plan.audit_job, plan.audit_owed) {
        (Some(job), _) => Some(format!(
            "The repair queued an incremental audit, job #{job}."
        )),
        (None, true) => Some(
            "An incremental audit is queued once the job running for the source set ends."
                .to_string(),
        ),
        (None, false) => None,
    }
}

/// Where `item` is now: for a game, the files of the source set it is in (the first, and how many more), or the game,
/// missing, when it is read only from outside the source set; otherwise its file, and the member in it; or, for a game
/// it lacks, the game. A plan recorded before games named where they are shows a game's file.
pub fn place(item: &RepairItem) -> String {
    match (item.from.as_slice(), &item.game) {
        ([first], _) => return first.clone(),
        ([first, more @ ..], _) => return format!("{first} +{} more", more.len()),
        ([], Some(game)) if !item.from_outside.is_empty() => return format!("{game} (missing)"),
        _ => {}
    }
    match (&item.path, &item.member, &item.game) {
        (Some(path), Some(member), _) => format!("{path} ({member})"),
        (Some(path), None, _) => path.clone(),
        (None, _, Some(game)) => format!("{game} (missing)"),
        (None, _, None) => item.key.clone(),
    }
}

/// The steps of `plan`, in order, by the key of the item each is for: so a row finds its own without reading every
/// step of the plan.
pub fn steps_by_item(plan: &RepairPlan) -> HashMap<&str, Vec<&RepairStep>> {
    let mut by_item: HashMap<&str, Vec<&RepairStep>> = HashMap::new();
    for s in &plan.content.steps {
        by_item.entry(s.item.as_str()).or_default().push(&s.step);
    }
    by_item
}

/// What the plan makes of `item`, given its own `steps`: the file it ends up as, the addition it is added as, or
/// that it is kept aside or left as it is.
pub fn proposed(item: &RepairItem, steps: &[&RepairStep]) -> String {
    if item.chosen == RepairChoice::Leave {
        return "left as it is".to_string();
    }
    if item.chosen == RepairChoice::AddAddition
        && let Some(addition) = &item.addition
    {
        return format!("addition {}", addition.name);
    }
    let steps = || steps.iter().copied();
    let written = steps().find_map(|step| match step {
        RepairStep::Rename { to, .. } => Some(to.clone()),
        RepairStep::Publish { target, .. } | RepairStep::Fill { target, .. } => {
            Some(target.clone())
        }
        _ => None,
    });
    let written = written.or_else(|| {
        steps().find_map(|step| match step {
            RepairStep::Build { target, .. } => Some(target.clone()),
            _ => None,
        })
    });
    match written {
        Some(target) => target,
        None if steps().any(|s| matches!(s, RepairStep::Keep { .. })) => "kept files".to_string(),
        None => "left as it is".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(id: i64, approved: bool, finished: bool, error: Option<&str>) -> RepairPlan {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "digest": "d",
            "created_at": "2026-01-01T00:00:00Z",
            "content": {
                "source_set": "Alpha", "set_id": 1, "root": "sets", "path": "Alpha", "audit": 1,
                "dat_revision": "r", "generation": 1, "creates_marker": false,
                "folder": {"dev": 1, "ino": 2}, "items": [], "steps": [], "fingerprint": []
            },
            "approval": approved.then(|| serde_json::json!({"digest": "d", "actor": "Operator", "at": "t"})),
            "finished_at": finished.then_some("2026-01-02T00:00:00Z"),
            "recovery_error": error,
        }))
        .unwrap()
    }

    #[test]
    fn the_screen_opens_on_the_unfinished_approved_repair_rather_than_a_new_proposal() {
        let plans = [
            plan(4, false, false, None),
            plan(3, true, false, Some("stuck")),
            plan(2, true, true, None),
        ];
        assert_eq!(repair_to_show(&plans, None).map(|p| p.id), Some(3));
        assert_eq!(repair_to_show(&plans[..1], None).map(|p| p.id), None);
        assert_eq!(repair_to_show(&plans, Some(2)).map(|p| p.id), Some(2));
    }

    #[test]
    fn asking_again_for_the_plan_shown_drops_no_change() {
        let mut loads = Loads::default();
        // A proposal shows plan 5, then the source set's summary catches up and asks for it.
        assert_eq!(loads.ask(None, Some(5), Some((5, false))), None);
        let approving = loads.mark();
        assert_eq!(loads.ask(None, Some(5), Some((5, false))), None);
        assert!(loads.change_applies(approving), "approving 5 still lands");
    }

    #[test]
    fn a_dropped_change_reloads_the_plan_wanted() {
        let mut loads = Loads::default();
        assert_eq!(loads.reload(), None, "nothing asked for yet");
        assert_eq!(loads.ask(None, Some(5), Some((5, false))), None);
        let approving = loads.mark();
        // The summary moves on to plan 6 before the approval of 5 comes back.
        let six = loads
            .ask(None, Some(6), Some((5, false)))
            .expect("a load of 6");
        assert!(!loads.change_applies(approving));
        let again = loads.reload().expect("a reload");
        assert_eq!(again.wanted, Some(6));
        assert!(!loads.applies(six), "the reload wins");
        assert!(loads.applies(again));
    }

    #[test]
    fn moving_to_another_plan_drops_what_was_under_way_for_the_last() {
        let mut loads = Loads::default();
        let two = loads.ask(Some(2), None, None).expect("a load of 2");
        let one = loads.ask(Some(1), None, None).expect("a load of 1");
        assert!(!loads.applies(two), "2 was asked for before 1");
        assert!(loads.applies(one));
        assert_eq!(
            loads.ask(Some(1), None, Some((1, false))),
            None,
            "asked for last"
        );
        assert!(
            loads.ask(None, None, Some((1, false))).is_some(),
            "the open repair is looked up"
        );
    }

    #[test]
    fn a_plan_you_open_wins_over_a_proposal_still_under_way() {
        let mut loads = Loads::default();
        assert_eq!(loads.ask(None, Some(1), Some((1, false))), None);
        let proposing = loads.mark();
        // You open plan 2 from the history before the proposal comes back.
        let two = loads
            .ask(Some(2), Some(1), Some((1, false)))
            .expect("a load of 2");
        assert!(!loads.change_applies(proposing), "the proposal is dropped");
        assert!(loads.applies(two), "so 2 is shown");
    }

    #[test]
    fn a_proposal_shown_drops_a_load_asked_for_before_it() {
        let mut loads = Loads::default();
        let a = loads.ask(Some(1), None, None).expect("a load of 1");
        // Proposed while 1 is still loading: plan 2 comes back first.
        let proposing = loads.mark();
        assert!(loads.change_applies(proposing));
        loads.accepted(Some(2));
        assert!(!loads.applies(a), "1 isn't shown over 2");
        // The URL now names 2.
        assert_eq!(loads.ask(Some(2), None, Some((2, false))), None);
        let b = loads
            .ask(Some(1), None, Some((2, false)))
            .expect("opening 1 again loads it");
        assert!(loads.applies(b), "a load asked for afterwards wins");
    }

    #[test]
    fn approving_a_then_opening_b_and_back_shows_a_approved() {
        let mut loads = Loads::default();
        assert_eq!(loads.ask(None, Some(1), Some((1, false))), None);
        let approving = loads.mark();
        let b = loads
            .ask(Some(2), Some(1), Some((1, false)))
            .expect("a load of 2");
        // Back to 1 before either answer: 1 is still shown, so nothing is fetched.
        assert_eq!(loads.ask(Some(1), Some(1), Some((1, false))), None);
        assert!(!loads.applies(b), "2 isn't shown over 1");
        assert!(!loads.change_applies(approving));
        // The approval's answer is dropped, so 1 is fetched again, approved.
        let again = loads.reload().expect("a reload");
        assert_eq!(again.wanted, Some(1));
        assert!(loads.applies(again));
    }

    #[test]
    fn a_running_repair_left_and_returned_to_still_finishes() {
        let mut loads = Loads::default();
        assert_eq!(loads.ask(None, Some(1), Some((1, true))), None);
        // Its job ends and the plan is fetched to show how it finished.
        let fetched = Some(7);
        let refreshing = loads.mark();
        loads.ask(Some(2), Some(1), Some((1, true)));
        assert_eq!(loads.ask(Some(1), Some(1), Some((1, true))), None);
        assert!(!loads.change_applies(refreshing));
        // The refresh is dropped: it is fetched again, and may be once more when the job's end is seen.
        assert_eq!(loads.reload().map(|t| t.wanted), Some(Some(1)));
        assert!(!fetch_after(7, true, true, fetched));
        assert!(fetch_after(7, true, true, None), "once forgotten");
    }

    #[test]
    fn the_open_repair_stays_shown_once_it_finishes() {
        let mut loads = Loads::default();
        assert_eq!(loads.ask(None, Some(3), Some((3, true))), None);
        // It finished, so the source set has no open repair.
        assert_eq!(
            loads.ask(None, None, Some((3, true))),
            None,
            "its outcomes stay"
        );
        // A finished repair opened from its history isn't kept once the link no longer names it.
        let mut loads = Loads::default();
        loads.ask(Some(2), None, None);
        let ticket = loads.ask(None, None, Some((2, true))).expect("a lookup");
        assert_eq!(ticket.wanted, None);
    }

    #[test]
    fn a_repair_is_proposed_only_while_none_runs() {
        assert!(may_propose(None, false));
        assert!(
            !may_propose(Some((false, false)), false),
            "plan it again instead"
        );
        assert!(may_propose(Some((true, true)), false));
        assert!(
            !may_propose(Some((true, true)), true),
            "a finished plan from the history, while another repair runs"
        );
        assert!(!may_propose(None, true));
    }

    #[test]
    fn a_plan_left_unfinished_is_fetched_once_per_job() {
        assert!(fetch_after(7, true, true, None));
        assert!(
            !fetch_after(7, true, true, Some(7)),
            "fetched again in a loop"
        );
        assert!(
            fetch_after(8, true, true, Some(7)),
            "a later job's end is fetched"
        );
        assert!(!fetch_after(7, false, true, None));
        assert!(!fetch_after(7, true, false, None));
    }

    #[test]
    fn an_unfinished_repair_says_what_blocks_it_and_that_it_is_retried_at_start() {
        let note = unfinished_note(&plan(3, true, false, Some("x could not be deleted"))).unwrap();
        assert!(note.contains("x could not be deleted"), "{note}");
        assert!(note.contains("when the service"), "{note}");
        assert_eq!(unfinished_note(&plan(3, true, true, Some("x"))), None);
        assert_eq!(unfinished_note(&plan(3, false, false, None)), None);
    }

    fn item(
        key: &str,
        choices: &[&str],
        default: &str,
        chosen: &str,
    ) -> stage_select_api::repair::RepairItem {
        serde_json::from_value(serde_json::json!({
            "key": key, "kind": "conflict", "summary": "s",
            "choices": choices, "default": default, "chosen": chosen,
        }))
        .unwrap()
    }

    #[test]
    fn a_pick_other_than_the_plans_holds_its_approval_until_it_is_planned_again() {
        let mut p = plan(1, false, false, None);
        p.content.items.push(item(
            "entry:a.zip",
            &["apply", "leave", "keep_aside"],
            "leave",
            "leave",
        ));
        let none = BTreeMap::new();
        assert!(unplanned(&p, &none, &BTreeMap::new()).is_empty());

        let apply = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Apply)]);
        assert_eq!(unplanned(&p, &apply, &BTreeMap::new()), ["entry:a.zip"]);
        assert_eq!(requested(&p, &none, &apply), apply, "planned again with it");
        let back = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Leave)]);
        assert!(
            unplanned(&p, &back, &BTreeMap::new()).is_empty(),
            "picked back to what the plan does"
        );
    }

    #[test]
    fn a_shown_plans_own_choice_set_back_to_its_default_holds_its_approval() {
        let mut p = plan(1, false, false, None);
        p.content.items.push(item(
            "entry:c.zip",
            &["leave", "keep_aside"],
            "leave",
            "keep_aside",
        ));
        let asked = chosen(&p);
        assert_eq!(
            asked,
            BTreeMap::from([("entry:c.zip".to_string(), RepairChoice::KeepAside)])
        );
        let leave = BTreeMap::from([("entry:c.zip".to_string(), RepairChoice::Leave)]);
        assert_eq!(unplanned(&p, &leave, &BTreeMap::new()), ["entry:c.zip"]);
        assert!(
            requested(&p, &asked, &leave).is_empty(),
            "its default needs no asking"
        );
    }

    #[test]
    fn picking_a_default_the_plan_overrode_holds_its_approval() {
        // The plan left a game whose default is to apply, as applying it would keep aside a ROM's last copy.
        let mut p = plan(1, false, false, None);
        p.content
            .items
            .push(item("game:g", &["apply", "leave"], "apply", "leave"));
        let apply = BTreeMap::from([("game:g".to_string(), RepairChoice::Apply)]);
        assert_eq!(unplanned(&p, &apply, &BTreeMap::new()), ["game:g"]);
        assert!(requested(&p, &BTreeMap::new(), &apply).is_empty());
    }

    #[test]
    fn an_addition_set_back_to_its_default_isnt_asked_for_when_planning_again() {
        let added = NewAddition {
            system: "Atari - 2600".into(),
            name: "Hack".into(),
            parent: None,
        };
        let mut p = plan(1, false, false, None);
        p.content.items.push(
            serde_json::from_value(serde_json::json!({
                "key": "entry:h.zip", "kind": "keep_aside", "summary": "s",
                "choices": ["keep_aside", "add_addition"], "default": "keep_aside", "chosen": "add_addition",
                "addition": added,
            }))
            .unwrap(),
        );
        let adding = BTreeMap::from([("entry:h.zip".to_string(), added.clone())]);
        let asked = chosen(&p);
        assert_eq!(
            to_add(
                &p,
                &requested(&p, &asked, &BTreeMap::new()),
                &adding,
                "Atari - 2600"
            ),
            adding
        );
        let back = BTreeMap::from([("entry:h.zip".to_string(), RepairChoice::KeepAside)]);
        assert!(to_add(&p, &requested(&p, &asked, &back), &adding, "Atari - 2600").is_empty());
    }

    #[test]
    fn planning_again_adds_what_the_plan_did_or_a_suggestion_for_an_addition_never_opened() {
        let added = NewAddition {
            system: "Atari - 2600".into(),
            name: "Hack".into(),
            parent: Some("Pong (USA)".into()),
        };
        let mut p = plan(1, false, false, None);
        let mut push = |key: &str, path: &str, chosen: &str, addition: Option<&NewAddition>| {
            p.content.items.push(
                serde_json::from_value(serde_json::json!({
                    "key": key, "kind": "keep_aside", "summary": "s", "path": path,
                    "choices": ["keep_aside", "add_addition"], "default": "keep_aside", "chosen": chosen,
                    "addition": addition,
                }))
                .unwrap(),
            );
        };
        push("entry:h.zip", "h.zip", "add_addition", Some(&added));
        push("entry:My Demo.zip", "My Demo.zip", "keep_aside", None);
        let picked = BTreeMap::from([("entry:My Demo.zip".to_string(), RepairChoice::AddAddition)]);
        let choices = requested(&p, &chosen(&p), &picked);
        let asked = to_add(&p, &choices, &BTreeMap::new(), "Atari - 2600");
        assert_eq!(asked["entry:h.zip"], added, "what the plan adds it as");
        let suggested = &asked["entry:My Demo.zip"];
        assert_eq!(
            (suggested.system.as_str(), suggested.name.as_str()),
            ("Atari - 2600", "My Demo"),
            "a suggestion in the source set's DAT"
        );
    }

    #[test]
    fn an_addition_edited_since_the_plan_was_made_holds_its_approval() {
        let added = NewAddition {
            system: "Atari - 2600".into(),
            name: "Hack".into(),
            parent: None,
        };
        let mut p = plan(1, false, false, None);
        p.content.items.push(
            serde_json::from_value(serde_json::json!({
                "key": "entry:h.zip", "kind": "keep_aside", "summary": "s",
                "choices": ["leave", "add_addition"], "default": "leave", "chosen": "add_addition",
                "addition": added,
            }))
            .unwrap(),
        );
        let choices = BTreeMap::from([("entry:h.zip".to_string(), RepairChoice::AddAddition)]);
        let same = BTreeMap::from([("entry:h.zip".to_string(), added.clone())]);
        assert!(unplanned(&p, &choices, &same).is_empty());
        let renamed = BTreeMap::from([(
            "entry:h.zip".to_string(),
            NewAddition {
                name: "Hack 2".into(),
                ..added
            },
        )]);
        assert_eq!(unplanned(&p, &choices, &renamed), ["entry:h.zip"]);
    }

    fn bulk_plan() -> RepairPlan {
        let mut p = plan(1, false, false, None);
        let kept = |key: &str, choices: &[&str]| {
            let mut i = item(key, choices, "keep_aside", "keep_aside");
            i.kind = stage_select_api::repair::RepairKind::KeepAside;
            i
        };
        p.content.items = vec![
            kept("entry:a.zip", &["leave", "keep_aside", "add_addition"]),
            kept("entry:b.zip", &["leave", "keep_aside", "add_addition"]),
            kept("entry:c.zip", &["keep_aside"]),
            item("game:g", &["apply", "leave"], "apply", "apply"),
        ];
        p
    }

    #[test]
    fn apply_to_all_offers_what_any_item_of_the_kind_offers_but_adding() {
        let p = bulk_plan();
        assert_eq!(
            kind_choices(&p, PlanKind::SetAside),
            [RepairChoice::Leave, RepairChoice::KeepAside]
        );
        let ab = ["entry:a.zip".to_string(), "entry:b.zip".to_string()];
        assert_eq!(
            choices_for(&p, &ab),
            [RepairChoice::Leave, RepairChoice::KeepAside]
        );
        assert_eq!(
            kind_choices(&p, PlanKind::Rename),
            [RepairChoice::Apply, RepairChoice::Leave],
            "a conflict is shown as a rename"
        );
    }

    #[test]
    fn no_change_leaves_every_pick_as_it_was() {
        let p = bulk_plan();
        let mut picked = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Leave)]);
        let before = picked.clone();
        assert_eq!(
            apply_to_kind(&p, PlanKind::SetAside, None, &mut picked),
            None
        );
        assert_eq!(picked, before);
    }

    #[test]
    fn apply_to_all_sets_every_item_of_the_kind_and_only_that_kind() {
        let mut p = bulk_plan();
        p.content.items.truncate(2);
        p.content
            .items
            .push(item("game:g", &["apply", "leave"], "apply", "apply"));
        let mut picked = BTreeMap::new();
        let outcome = apply_to_kind(
            &p,
            PlanKind::SetAside,
            Some(RepairChoice::Leave),
            &mut picked,
        )
        .unwrap();
        assert_eq!(outcome.changed, 2);
        assert_eq!(
            picked.keys().collect::<Vec<_>>(),
            ["entry:a.zip", "entry:b.zip"]
        );
        assert_eq!(
            unplanned(&p, &picked, &BTreeMap::new()).len(),
            2,
            "approval waits"
        );
        picked.insert("entry:a.zip".into(), RepairChoice::KeepAside);
        assert_eq!(
            unplanned(&p, &picked, &BTreeMap::new()),
            ["entry:b.zip"],
            "one item can still be changed afterwards"
        );
    }

    #[test]
    fn a_choice_only_some_items_offer_sets_only_those_and_says_so() {
        let p = bulk_plan();
        let mut picked = BTreeMap::new();
        let keys: Vec<String> = p.content.items.iter().map(|i| i.key.clone()).collect();
        let outcome = apply_to_keys(&p, &keys, RepairChoice::Leave, &mut picked);
        assert_eq!(
            outcome,
            BulkOutcome {
                asked: 4,
                offered: 3,
                changed: 3
            }
        );
        assert!(!picked.contains_key("entry:c.zip"));
        assert_eq!(
            current(&p.content.items[2], &picked),
            RepairChoice::KeepAside
        );
        assert!(outcome.message("leave").contains("1 item doesn't offer it"));
    }

    #[test]
    fn adding_as_an_addition_is_never_set_in_bulk() {
        let p = bulk_plan();
        let mut picked = BTreeMap::new();
        let keys = ["entry:a.zip".to_string()];
        let outcome = apply_to_keys(&p, &keys, RepairChoice::AddAddition, &mut picked);
        assert_eq!(outcome.offered, 0);
        assert!(picked.is_empty());
    }

    fn seen() -> stage_select_api::repair::SeenFile {
        use stage_select_api::build::{FileIdentity, FileStat};
        stage_select_api::repair::SeenFile {
            identity: FileIdentity { dev: 1, ino: 2 },
            file: FileStat {
                size: 3,
                mtime_ns: 4,
            },
            ctime_ns: 5,
        }
    }

    #[test]
    fn a_row_reads_where_the_item_is_and_what_the_plan_makes_of_it() {
        use stage_select_api::repair::PlannedStep;
        let mut p = plan(1, false, false, None);
        let mut renamed = item("entry:a.zip", &["apply", "leave"], "apply", "apply");
        renamed.path = Some("a.zip".into());
        let mut repacked = item("entry:b.zip#b.bin", &["apply", "leave"], "apply", "apply");
        repacked.path = Some("b.zip".into());
        repacked.member = Some("b.bin".into());
        let mut kept = item(
            "entry:c.txt",
            &["keep_aside", "leave"],
            "keep_aside",
            "keep_aside",
        );
        kept.path = Some("c.txt".into());
        let mut left = item("entry:d.zip", &["apply", "leave"], "apply", "leave");
        left.path = Some("d.zip".into());
        let mut filled = item("game:Echo", &["apply", "leave"], "apply", "apply");
        filled.game = Some("Echo".into());
        p.content.steps = vec![
            PlannedStep {
                item: "entry:a.zip".into(),
                step: RepairStep::Rename {
                    from: "a.zip".into(),
                    to: "Alpha.zip".into(),
                    seen: seen(),
                    roms: vec![],
                    loose: false,
                },
            },
            PlannedStep {
                item: "entry:b.zip#b.bin".into(),
                step: RepairStep::Build {
                    target: "Bravo.zip".into(),
                    roms: vec![],
                    sources: BTreeMap::new(),
                    loose: false,
                },
            },
            PlannedStep {
                item: "entry:c.txt".into(),
                step: RepairStep::Keep {
                    path: "c.txt".into(),
                    kept_name: "c.txt".into(),
                    reason: "unmatched".into(),
                    seen: seen(),
                    needs: vec![],
                    affects: vec![],
                },
            },
        ];
        let by_item = steps_by_item(&p);
        let rows: Vec<(String, String)> = [&renamed, &repacked, &kept, &left, &filled]
            .into_iter()
            .map(|i| {
                let own = by_item.get(i.key.as_str()).map_or(&[][..], Vec::as_slice);
                (place(i), proposed(i, own))
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("a.zip".into(), "Alpha.zip".into()),
                ("b.zip (b.bin)".into(), "Bravo.zip".into()),
                ("c.txt".into(), "kept files".into()),
                ("d.zip".into(), "left as it is".into()),
                ("Echo (missing)".into(), "left as it is".into()),
            ]
        );
    }

    fn game(
        kind: &str,
        path: &str,
        from: &[&str],
        outside: &[&str],
    ) -> stage_select_api::repair::RepairItem {
        serde_json::from_value(serde_json::json!({
            "key": "game:Yars (USA)", "kind": kind, "game": "Yars (USA)", "path": path, "from": from,
            "from_outside": outside, "summary": "s", "choices": ["apply", "leave"], "default": "apply",
            "chosen": "apply",
        }))
        .unwrap()
    }

    #[test]
    fn a_game_is_now_where_the_plan_reads_it_from() {
        let converted = game("convert", "Yars (USA).zip", &["Yars (USA).bin"], &[]);
        assert_eq!(place(&converted), "Yars (USA).bin");
        let renamed = game("rename", "Yars (USA).zip", &["yars.zip"], &[]);
        assert_eq!(place(&renamed), "yars.zip");
        let repacked = game(
            "repack",
            "Yars (USA).zip",
            &["a.zip", "b.zip", "c.zip"],
            &[],
        );
        assert_eq!(place(&repacked), "a.zip +2 more");
        let incoming = ["yars.7z in ingest folder Incoming"];
        let filled = game("fill", "Yars (USA).zip", &[], &incoming);
        assert_eq!(place(&filled), "Yars (USA) (missing)");
        let replaced = game("replace", "Yars (USA).zip", &["Yars (USA).zip"], &incoming);
        assert_eq!(place(&replaced), "Yars (USA).zip");
        let cued = game(
            "fill",
            "Yars (USA)",
            &["Yars (USA)/"],
            &["cue pack Cues.zip"],
        );
        assert_eq!(place(&cued), "Yars (USA)/");
        let named_like_an_ingest_item = game(
            "rename",
            "Yars (USA).a26",
            &["Yars in ingest folder Incoming.a26"],
            &[],
        );
        assert_eq!(
            place(&named_like_an_ingest_item),
            "Yars in ingest folder Incoming.a26"
        );
    }

    #[test]
    fn a_plan_recorded_without_where_a_game_is_read_from_reads_as_before() {
        let old = game("convert", "Yars (USA).zip", &[], &[]);
        assert_eq!(place(&old), "Yars (USA).zip");
    }

    #[test]
    fn a_finished_repair_says_it_queued_an_audit_and_leaves_its_outcome_to_the_audit() {
        let mut p = plan(1, true, true, None);
        assert_eq!(follow_up_audit(&p), None);
        p.audit_job = Some(9);
        assert_eq!(
            follow_up_audit(&p).as_deref(),
            Some("The repair queued an incremental audit, job #9.")
        );
        p.audit_job = None;
        p.audit_owed = true;
        assert_eq!(
            follow_up_audit(&p).as_deref(),
            Some("An incremental audit is queued once the job running for the source set ends.")
        );
    }

    fn kinded(key: &str, kind: &str, chosen: &str) -> stage_select_api::repair::RepairItem {
        serde_json::from_value(serde_json::json!({
            "key": key, "kind": kind, "summary": "s",
            "choices": ["apply", "leave", "keep_aside", "add_addition"], "default": "apply", "chosen": chosen,
        }))
        .unwrap()
    }

    #[test]
    fn the_summary_line_reads_what_each_item_is_chosen_to_do() {
        let mut p = plan(1, false, false, None);
        assert_eq!(plan_line(&p), "Changes nothing");
        p.content.items = vec![
            kinded("a", "rename", "apply"),
            kinded("b", "conflict", "apply"),
            kinded("c", "fill", "apply"),
            kinded("d", "rename", "keep_aside"),
            kinded("e", "keep_aside", "apply"),
            kinded("f", "rename", "leave"),
            kinded("g", "rename", "add_addition"),
        ];
        assert_eq!(
            plan_line(&p),
            "Renames 2 entries, fills 1 game, sets aside 2 files, adds 1 addition, leaves 1 item as it is"
        );
    }

    #[test]
    fn a_finished_repair_says_what_became_of_every_item_it_changes() {
        use stage_select_api::repair::RepairOutcome;
        let mut p = plan(1, true, false, None);
        p.content.items = vec![
            kinded("done", "rename", "apply"),
            kinded("failed", "rename", "apply"),
            kinded("skipped", "rename", "apply"),
            kinded("left", "rename", "leave"),
        ];
        let outcome = |item: &str, done: bool, error: Option<&str>| RepairOutcome {
            item: item.into(),
            done,
            detail: Vec::new(),
            error: error.map(Into::into),
        };
        p.outcomes = vec![
            outcome("done", true, None),
            outcome("failed", false, Some("x")),
        ];
        let of = |p: &RepairPlan| -> Vec<Option<ItemOutcome>> {
            p.content.items.iter().map(|i| item_outcome(p, i)).collect()
        };
        assert_eq!(
            of(&p),
            [
                Some(ItemOutcome::Done),
                Some(ItemOutcome::Failed),
                None,
                None
            ],
            "still running"
        );
        p.finished_at = Some("t".into());
        assert_eq!(
            of(&p),
            [
                Some(ItemOutcome::Done),
                Some(ItemOutcome::Failed),
                Some(ItemOutcome::NotDone),
                None
            ]
        );
    }

    #[test]
    fn with_no_repair_the_tab_counts_what_a_repair_could_change() {
        let findings = BTreeMap::from([
            (Finding::Matched, 40),
            (Finding::Misnamed, 3),
            (Finding::Conflicting, 0),
            (Finding::Unmatched, 2),
            (Finding::Unreadable, 1),
        ]);
        let games = BTreeMap::from([
            (GameStatus::Complete, 30),
            (GameStatus::Incomplete, 4),
            (GameStatus::Missing, 5),
        ]);
        let could = could_change(&findings, &games);
        assert_eq!(
            could
                .iter()
                .map(|c| (c.found, c.n, c.kind))
                .collect::<Vec<_>>(),
            [
                (Found::Entries(Finding::Misnamed), 3, PlanKind::Rename),
                (Found::Entries(Finding::Unreadable), 1, PlanKind::Replace),
                (Found::Entries(Finding::Unmatched), 2, PlanKind::SetAside),
                (Found::Games(GameStatus::Missing), 5, PlanKind::Fill),
                (Found::Games(GameStatus::Incomplete), 4, PlanKind::Fill),
            ],
            "a repair fills missing and incomplete games; matched entries and complete games aren't counted, and \
             none counted is left out"
        );
        assert_eq!(could[0].what(), "Misnamed entries to rename");
        assert_eq!(could[3].what(), "Missing games to fill");
        assert_eq!(could[4].what(), "Incomplete games to complete");
        assert_eq!(could[0].filter(true), [("finding", "misnamed")]);
        assert_eq!(could[3].filter(false), [("status", "missing")]);
        assert_eq!(
            could[4].filter(true),
            [("status", "incomplete"), ("covered", "yes")],
            "with a coverage the counts are of covered games, and so is the list a tile opens"
        );
        let lead = repair_lead(true, &could);
        assert!(lead.contains("could change"), "{lead}");
        assert!(
            lead.contains("spare copies") && lead.contains("storage policy"),
            "what the audit can't count is named: {lead}"
        );
    }

    #[test]
    fn the_empty_repair_tab_says_why_nothing_is_counted() {
        let clean = could_change(
            &BTreeMap::from([(Finding::Matched, 9)]),
            &BTreeMap::from([(GameStatus::Complete, 9)]),
        );
        assert!(clean.is_empty());
        let (unaudited, nothing) = (repair_lead(false, &[]), repair_lead(true, &clean));
        assert!(unaudited.starts_with("Not audited yet"), "{unaudited}");
        assert!(
            !nothing.contains("nothing a repair could change"),
            "a matched-only audit can still need spare copies set aside or games repacked: {nothing}"
        );
        assert!(
            nothing.contains("spare copies") && nothing.contains("storage policy"),
            "{nothing}"
        );
    }
}
