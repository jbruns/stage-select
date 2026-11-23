//! The plan review's pure logic, whatever the plan: counts of its items by kind, a kind's name in the URL, and setting
//! one choice for many items at once. `parts::plan` shows it; `repair_model` holds what is particular to repairs.

use std::collections::BTreeMap;

use super::status::{ItemOutcome, Status};

/// How many items of each kind a plan holds, in the vocabulary's order, leaving out kinds it has none of.
pub fn kind_counts<K: Status>(kinds: impl IntoIterator<Item = K>) -> Vec<(K, usize)> {
    let mut counts = vec![0; K::ALL.len()];
    for kind in kinds {
        counts[kind.rank()] += 1;
    }
    K::ALL
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| (*k, n))
        .collect()
}

/// `counts` with the kind filtered on kept in place at zero when none are left, so the filter that hides every row
/// can still be cleared from the strip.
pub fn with_active<K: Status>(mut counts: Vec<(K, usize)>, active: Option<K>) -> Vec<(K, usize)> {
    if let Some(kind) = active
        && !counts.iter().any(|(k, _)| *k == kind)
    {
        let at = counts.partition_point(|(k, _)| k.rank() < kind.rank());
        counts.insert(at, (kind, 0));
    }
    counts
}

/// A kind as the URL's `kind` names it: its label, lowercase, with hyphens for spaces.
pub fn kind_slug<K: Status>(kind: K) -> String {
    kind.look().label.to_lowercase().replace(' ', "-")
}

/// The kind the URL's `kind` names, if it names one.
pub fn kind_from_slug<K: Status>(slug: &str) -> Option<K> {
    K::ALL.iter().copied().find(|k| kind_slug(*k) == slug)
}

/// What setting one choice for many items did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BulkOutcome {
    /// The items it was set for.
    pub asked: usize,
    /// Of those, the items that offer it.
    pub offered: usize,
    /// Of those, the items whose choice it changed: the rest had it already.
    pub changed: usize,
}

impl BulkOutcome {
    /// What to tell you, naming the choice as `label`.
    pub fn message(&self, label: &str) -> String {
        let BulkOutcome {
            asked,
            offered,
            changed,
        } = *self;
        let items = |n: usize| if n == 1 { "item" } else { "items" };
        let mut said = match changed {
            0 => format!("No item changed to “{label}”"),
            n => format!("Changed {n} {} to “{label}”", items(n)),
        };
        if offered > changed {
            said.push_str(&format!("; {} had it already", offered - changed));
        }
        if offered < asked {
            let left = asked - offered;
            said.push_str(&format!(
                "; {left} {} {} offer it, so {} unchanged",
                items(left),
                if left == 1 { "doesn't" } else { "don't" },
                if left == 1 { "it's" } else { "they're" },
            ));
        }
        said.push('.');
        said
    }
}

/// Sets `choice` for every item of `items` that offers it, in `picked` by key, where an item without a pick takes
/// `current`. `items` are each item's key, the choice it takes now and the choices it offers.
pub fn apply_choice<'a, C: Copy + Eq + 'a>(
    items: impl IntoIterator<Item = (&'a str, C, &'a [C])>,
    choice: C,
    picked: &mut BTreeMap<String, C>,
) -> BulkOutcome {
    let mut outcome = BulkOutcome::default();
    for (key, current, offers) in items {
        outcome.asked += 1;
        if !offers.contains(&choice) {
            continue;
        }
        outcome.offered += 1;
        let now = picked.get(key).copied().unwrap_or(current);
        if now != choice {
            outcome.changed += 1;
        }
        picked.insert(key.to_string(), choice);
    }
    outcome
}

/// The choices every one of `offers` offers, in the first's order: what "apply to all" can set for a kind.
pub fn offered_by_all<'a, C: Copy + Eq + 'a>(offers: impl IntoIterator<Item = &'a [C]>) -> Vec<C> {
    let mut offers = offers.into_iter();
    let Some(first) = offers.next() else {
        return Vec::new();
    };
    let mut common = first.to_vec();
    for o in offers {
        common.retain(|c| o.contains(c));
    }
    common
}

/// The choices any of `offers` offers, in the order first met: what can be set for rows of mixed offers.
pub fn offered_by_any<'a, C: Copy + Eq + 'a>(offers: impl IntoIterator<Item = &'a [C]>) -> Vec<C> {
    let mut any = Vec::new();
    for o in offers {
        for c in o {
            if !any.contains(c) {
                any.push(*c);
            }
        }
    }
    any
}

/// What a plan does to items of one kind, for its summary line: the verb, and what it does it to, one and many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Does {
    pub verb: &'static str,
    pub one: &'static str,
    pub many: &'static str,
}

impl Does {
    pub const fn new(verb: &'static str, one: &'static str, many: &'static str) -> Self {
        Does { verb, one, many }
    }
}

/// A plan's one-line summary in numbers ("Renames 12 entries, fills 3 games"): what it does, each in the order first
/// met, with the counts of the same doing added up; `nothing` when it does nothing.
pub fn plan_line(doing: impl IntoIterator<Item = (Does, usize)>, nothing: &str) -> String {
    let mut totals: Vec<(Does, usize)> = Vec::new();
    for (does, n) in doing {
        match totals.iter_mut().find(|(d, _)| *d == does) {
            Some((_, total)) => *total += n,
            None => totals.push((does, n)),
        }
    }
    let line = totals
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(d, n)| format!("{} {n} {}", d.verb, if n == 1 { d.one } else { d.many }))
        .collect::<Vec<_>>()
        .join(", ");
    let mut chars = line.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => nothing.to_string(),
    }
}

/// What a plan's items came to once it was applied, from their counts by outcome: how many were done, then the rest,
/// failures first.
pub fn outcome_line(counts: &[(ItemOutcome, usize)]) -> String {
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    let done = counts
        .iter()
        .find(|(o, _)| *o == ItemOutcome::Done)
        .map_or(0, |(_, n)| *n);
    if total == 0 {
        return "Nothing to do.".to_string();
    }
    if done == total {
        return format!("All {total} done.");
    }
    let rest = counts
        .iter()
        .filter(|(o, n)| *o != ItemOutcome::Done && *n > 0)
        .map(|(o, n)| format!("{n} {}", o.look().label.to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{done} of {total} done: {rest}.")
}

/// The choices you picked on a plan since it was shown, by item key: staged until you plan again, since approving
/// approves the plan as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picks<C> {
    picked: BTreeMap<String, C>,
}

impl<C> Default for Picks<C> {
    fn default() -> Self {
        Picks {
            picked: BTreeMap::new(),
        }
    }
}

impl<C: Copy + Eq> Picks<C> {
    /// The choice the item `key` takes now: your pick, or else `planned`, what the plan shown gives it.
    pub fn now(&self, key: &str, planned: C) -> C {
        self.picked.get(key).copied().unwrap_or(planned)
    }

    pub fn pick(&mut self, key: &str, choice: C) {
        self.picked.insert(key.to_string(), choice);
    }

    /// Sets `choice` for every one of `items` that offers it: each item's key, the choice the plan gives it and the
    /// choices it offers.
    pub fn set<'a>(
        &mut self,
        items: impl IntoIterator<Item = (&'a str, C, &'a [C])>,
        choice: C,
    ) -> BulkOutcome
    where
        C: 'a,
    {
        apply_choice(items, choice, &mut self.picked)
    }

    /// How many of `planned` (each item's key and the choice the plan gives it) you picked otherwise.
    pub fn changed<'a>(&self, planned: impl IntoIterator<Item = (&'a str, C)>) -> usize {
        planned
            .into_iter()
            .filter(|(key, c)| self.now(key, *c) != *c)
            .count()
    }

    /// The choices to plan again with: every item's choice now, from `planned`, where it isn't `default`, the choice an
    /// item takes when none is asked for.
    pub fn requested<'a>(
        &self,
        planned: impl IntoIterator<Item = (&'a str, C)>,
        default: C,
    ) -> BTreeMap<String, C> {
        planned
            .into_iter()
            .map(|(key, c)| (key, self.now(key, c)))
            .filter(|(_, c)| *c != default)
            .map(|(key, c)| (key.to_string(), c))
            .collect()
    }
}

/// Where approving a plan stands: how many of its choices you changed since it was shown, and whether something
/// blocks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Approval {
    pub changed: usize,
    pub blocked: bool,
}

impl Approval {
    /// Approving approves the plan as shown, so it waits while choices differ from it.
    pub fn may_approve(self) -> bool {
        self.changed == 0 && !self.blocked
    }

    pub fn plan_again(self) -> bool {
        self.changed > 0
    }

    pub fn notice(self) -> Option<String> {
        match self.changed {
            0 => None,
            1 => Some(
                "You changed 1 choice. Plan again to see what it does: approving approves the plan as shown."
                    .to_string(),
            ),
            n => Some(format!(
                "You changed {n} choices. Plan again to see what they do: approving approves the plan as shown."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::status::PlanKind;

    #[test]
    fn counts_follow_the_vocabularys_order_and_leave_out_kinds_absent() {
        let kinds = [PlanKind::SetAside, PlanKind::Rename, PlanKind::SetAside];
        assert_eq!(
            kind_counts(kinds),
            [(PlanKind::Rename, 1), (PlanKind::SetAside, 2)]
        );
    }

    #[test]
    fn the_kind_filtered_on_stays_in_the_strip_at_zero_so_it_can_be_cleared() {
        use crate::parts::status::SourceSetStatus;
        let counts = kind_counts([SourceSetStatus::Verified]);
        assert_eq!(
            with_active(counts.clone(), Some(SourceSetStatus::Running)),
            [
                (SourceSetStatus::Running, 0),
                (SourceSetStatus::Verified, 1)
            ]
        );
        assert_eq!(
            with_active(counts.clone(), Some(SourceSetStatus::Verified)),
            counts
        );
        assert_eq!(with_active(counts.clone(), None), counts);
    }

    #[test]
    fn every_kind_round_trips_through_the_url() {
        for kind in PlanKind::ALL {
            let slug = kind_slug(*kind);
            assert!(!slug.contains(' '), "{slug}");
            assert_eq!(kind_from_slug::<PlanKind>(&slug), Some(*kind));
        }
        assert_eq!(kind_from_slug::<PlanKind>("nonsense"), None);
    }

    #[test]
    fn a_choice_is_set_only_where_offered_and_counted_only_where_it_changed() {
        let mut picked = BTreeMap::from([("b".to_string(), 2)]);
        let items: [(&str, u8, &[u8]); 3] = [("a", 1, &[1, 2]), ("b", 1, &[1, 2]), ("c", 1, &[1])];
        let outcome = apply_choice(items, 2, &mut picked);
        assert_eq!(
            outcome,
            BulkOutcome {
                asked: 3,
                offered: 2,
                changed: 1
            }
        );
        assert_eq!(picked, BTreeMap::from([("a".into(), 2), ("b".into(), 2)]));
    }

    #[test]
    fn the_message_says_how_many_changed_and_why_the_rest_didnt() {
        let all = BulkOutcome {
            asked: 3,
            offered: 3,
            changed: 3,
        };
        assert_eq!(all.message("leave"), "Changed 3 items to “leave”.");
        let some = BulkOutcome {
            asked: 5,
            offered: 3,
            changed: 2,
        };
        assert_eq!(
            some.message("leave"),
            "Changed 2 items to “leave”; 1 had it already; 2 items don't offer it, so they're unchanged."
        );
        let none = BulkOutcome {
            asked: 1,
            offered: 0,
            changed: 0,
        };
        assert_eq!(
            none.message("apply"),
            "No item changed to “apply”; 1 item doesn't offer it, so it's unchanged."
        );
    }

    #[test]
    fn all_offers_what_every_item_does_and_any_what_one_does() {
        let offers: [&[u8]; 2] = [&[1, 2, 3], &[3, 2]];
        assert_eq!(offered_by_all(offers), [2, 3]);
        assert_eq!(offered_by_any(offers), [1, 2, 3]);
        assert!(offered_by_all::<u8>([]).is_empty());
    }

    const RENAMES: Does = Does::new("renames", "entry", "entries");
    const FILLS: Does = Does::new("fills", "game", "games");
    const SETS_ASIDE: Does = Does::new("sets aside", "file", "files");

    #[test]
    fn the_summary_line_reads_what_the_plan_does_in_numbers() {
        let line = plan_line(
            [(RENAMES, 12), (FILLS, 3), (SETS_ASIDE, 1), (FILLS, 1)],
            "Changes nothing",
        );
        assert_eq!(line, "Renames 12 entries, fills 4 games, sets aside 1 file");
    }

    #[test]
    fn the_summary_line_leaves_out_what_it_does_none_of_and_says_when_it_does_nothing() {
        assert_eq!(
            plan_line([(RENAMES, 0), (SETS_ASIDE, 2)], "Changes nothing"),
            "Sets aside 2 files"
        );
        assert_eq!(
            plan_line([(RENAMES, 0)], "Changes nothing"),
            "Changes nothing"
        );
    }

    #[test]
    fn outcomes_count_with_failures_first_and_say_how_many_were_done() {
        use crate::parts::status::ItemOutcome::{Done, Failed, NotDone, Uncertain};
        let counts = kind_counts([Done, NotDone, Failed, Done]);
        assert_eq!(counts, [(Failed, 1), (NotDone, 1), (Done, 2)]);
        assert_eq!(outcome_line(&counts), "2 of 4 done: 1 failed, 1 not done.");
        assert_eq!(outcome_line(&[(Done, 3)]), "All 3 done.");
        assert_eq!(
            outcome_line(&[(Uncertain, 1), (NotDone, 2)]),
            "0 of 3 done: 1 uncertain, 2 not done."
        );
        assert_eq!(outcome_line(&[]), "Nothing to do.");
    }

    fn rows() -> [(&'static str, u8, &'static [u8]); 3] {
        [("a", 0, &[0, 1, 2]), ("b", 0, &[0, 1]), ("c", 1, &[0, 1])]
    }

    #[test]
    fn a_pick_is_what_an_item_takes_now_and_counts_as_changed_while_it_differs_from_the_plan() {
        let mut picks = Picks::default();
        assert_eq!(picks.now("a", 0), 0);
        assert_eq!(picks.changed(rows().map(|(k, c, _)| (k, c))), 0);
        picks.pick("a", 2);
        picks.pick("c", 1);
        assert_eq!(picks.now("a", 0), 2);
        assert_eq!(picks.changed(rows().map(|(k, c, _)| (k, c))), 1);
        picks.pick("a", 0);
        assert_eq!(picks.changed(rows().map(|(k, c, _)| (k, c))), 0);
    }

    #[test]
    fn setting_many_rows_sets_those_that_offer_the_choice() {
        let mut picks = Picks::default();
        let outcome = picks.set(rows(), 2);
        assert_eq!(
            outcome,
            BulkOutcome {
                asked: 3,
                offered: 1,
                changed: 1
            }
        );
        assert_eq!(picks.now("a", 0), 2);
        assert_eq!(picks.now("b", 0), 0);
    }

    #[test]
    fn planning_again_asks_for_every_choice_that_isnt_the_default() {
        let mut picks = Picks::default();
        picks.pick("a", 1);
        picks.pick("c", 0);
        // b keeps what the plan gave it, which isn't the default.
        let planned = [("a", 0), ("b", 2), ("c", 1)];
        assert_eq!(
            picks.requested(planned, 0),
            BTreeMap::from([("a".to_string(), 1), ("b".to_string(), 2)])
        );
    }

    #[test]
    fn approving_waits_while_choices_differ_from_the_plan_shown() {
        let ready = Approval {
            changed: 0,
            blocked: false,
        };
        assert!(ready.may_approve());
        assert!(!ready.plan_again());
        assert_eq!(ready.notice(), None);
        let changed = Approval {
            changed: 2,
            blocked: false,
        };
        assert!(!changed.may_approve());
        assert!(changed.plan_again());
        assert_eq!(
            changed.notice().as_deref(),
            Some(
                "You changed 2 choices. Plan again to see what they do: approving approves the plan as shown."
            )
        );
        let one = Approval {
            changed: 1,
            blocked: false,
        };
        assert_eq!(
            one.notice().as_deref(),
            Some(
                "You changed 1 choice. Plan again to see what it does: approving approves the plan as shown."
            )
        );
        let blocked = Approval {
            changed: 0,
            blocked: true,
        };
        assert!(!blocked.may_approve());
        assert!(!blocked.plan_again());
    }
}
