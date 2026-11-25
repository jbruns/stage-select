//! The one summary of an audit: its games and its entries' findings as two strips of counts that filter, drawn alike
//! on a source set's overview and on the audit's own page, and the next step each page offers from it.

use std::collections::BTreeMap;

use stage_select_api::{AuditSummary, Finding, GameStatus, Job, JobKind, SourceSetSummary};

use crate::model::{
    ALGORITHM, COVERED, COVERED_YES, FINDING, STATE, STATUS, needs_review_from_counts,
};
use crate::parts::status::{AuditState, FindingStatus, Status};
use crate::repair_model::{Found, could_change};
use crate::source_set_model::{facts, rests_on_claims};

/// Where a page keeps the lists the summary filters: the URL's `tab` of its games and of its entries, `None` for the
/// page's first tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lists {
    pub games: Option<&'static str>,
    pub entries: Option<&'static str>,
}

/// A source set's page: its Games and Entries tabs.
pub const ON_SOURCE_SET: Lists = Lists {
    games: Some("games"),
    entries: Some("entries"),
};

/// An audit's page: its entries first, its games on the `games` tab.
pub const ON_AUDIT: Lists = Lists {
    games: Some("games"),
    entries: None,
};

/// The audit's games by status: complete, incomplete and missing always, nothing to check only when there are some.
pub fn game_counts(games: &BTreeMap<GameStatus, usize>) -> Vec<(GameStatus, usize)> {
    GameStatus::ALL
        .into_iter()
        .map(|status| (status, games.get(&status).copied().unwrap_or(0)))
        .filter(|&(status, n)| n > 0 || status != GameStatus::NothingToCheck)
        .collect()
}

/// The audit's entries by finding, leaving out the findings it has none of.
pub fn finding_counts(findings: &BTreeMap<Finding, usize>) -> Vec<(FindingStatus, usize)> {
    let mut counts: Vec<(FindingStatus, usize)> = findings
        .iter()
        .filter(|&(_, &n)| n > 0)
        .map(|(&finding, &n)| (finding.into(), n))
        .collect();
    counts.sort_by_key(|&(status, _)| {
        FindingStatus::ALL
            .iter()
            .position(|&s| s == status)
            .unwrap_or(usize::MAX)
    });
    counts
}

/// Changes to the URL's query, each key set to its value or removed for none.
pub type Edit = Vec<(&'static str, Option<String>)>;

/// What picking `status` in the games' counts does: opens the games narrowed to it, and to the coverage when the
/// counts are `covered` games only, clearing what narrowed or ordered the list before; picking none clears the
/// narrowing and stays where you are.
pub fn pick_game(lists: Lists, status: Option<GameStatus>, covered: bool) -> Edit {
    match status {
        None => vec![(STATUS, None), (COVERED, None)],
        Some(status) => {
            let mut edit = opened(lists.games, STATUS, status.as_str());
            if covered {
                edit.retain(|(k, _)| *k != COVERED);
                edit.push((COVERED, Some(COVERED_YES.to_string())));
            }
            edit
        }
    }
}

/// What picking `finding` in the entries' counts does, as [`pick_game`] does for games.
pub fn pick_finding(lists: Lists, finding: Option<FindingStatus>) -> Edit {
    match finding {
        None => vec![(FINDING, None)],
        Some(finding) => opened(lists.entries, FINDING, finding_of(finding).as_str()),
    }
}

/// The game status the counts show as picked: the URL's `status` while the games are open, given the URL's `tab`.
pub fn active_game(lists: Lists, tab: Option<&str>, status: Option<&str>) -> Option<GameStatus> {
    (tab == lists.games)
        .then(|| status.and_then(GameStatus::parse))
        .flatten()
}

/// The finding the counts show as picked: the URL's `finding` while the entries are open.
pub fn active_finding(
    lists: Lists,
    tab: Option<&str>,
    finding: Option<&str>,
) -> Option<FindingStatus> {
    let open = match lists.entries {
        Some(_) => tab == lists.entries,
        None => tab != lists.games,
    };
    open.then(|| finding.and_then(Finding::parse).map(FindingStatus::from))
        .flatten()
}

/// The one next step a source set's page leads with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    /// Audit it with the defaults.
    Audit,
    /// Propose a repair from its latest audit, and open the plan.
    Repair,
    /// Audit it, and offer a repair second.
    MaybeRepair,
    /// Open its open repair plan, by id.
    Review(i64),
    /// Follow its audit, queued or running, by job id: a repair proposed now would rest on the audit it supersedes.
    Auditing(i64),
}

/// The audit job queued or running for the source set `name`, newest first in `jobs`.
pub fn audit_job<'a>(jobs: impl IntoIterator<Item = &'a Job>, name: &str) -> Option<i64> {
    jobs.into_iter()
        .find(|j| j.kind == JobKind::Audit && j.source_set == name && !j.state.is_final())
        .map(|j| j.id)
}

/// The audit job a source set's page shows the outcome of: the newer of the one `seen` queued or running there, kept
/// once it ends, and the follow-up audit of the repair shown, `repair`, which shows even once it has ended, as after a
/// reload.
pub fn followed_audit(seen: Option<i64>, repair: Option<i64>) -> Option<i64> {
    seen.max(repair)
}

/// The next step for the source set `summary`: its open repair to review; the audit job `auditing` to follow, while
/// one is queued or running; an audit, when it has none or files changed since; a repair, when its latest audit
/// found what a repair changes; otherwise an audit, with a repair second, since spare copies and the storage policy
/// aren't counted ahead of a repair.
pub fn next_step(summary: &SourceSetSummary, auditing: Option<i64>) -> NextStep {
    if let Some(repair) = &summary.repair {
        return NextStep::Review(repair.id);
    }
    if let Some(job) = auditing {
        return NextStep::Auditing(job);
    }
    let facts = facts(summary, false);
    match &summary.latest {
        Some(latest) if !facts.stale => {
            // With a coverage, only its entries count; the games already count only covered ones.
            let findings = summary
                .covered_entries
                .as_ref()
                .map_or(&latest.findings, |c| &c.findings);
            // An unreadable file is replaced only if another copy turns up, so it alone promises no change.
            let sure = could_change(findings, &latest.games)
                .iter()
                .any(|c| c.found != Found::Entries(Finding::Unreadable));
            if sure {
                NextStep::Repair
            } else {
                NextStep::MaybeRepair
            }
        }
        _ => NextStep::Audit,
    }
}

/// What an audit's page offers next, given its source set's `summary`: the open repair to review, or a repair while
/// this audit is the latest (second to an audit when it found nothing a repair surely changes); the audit job `auditing` to follow, while one is queued or running;
/// nothing otherwise.
pub fn after_audit(
    audit: i64,
    summary: &SourceSetSummary,
    auditing: Option<i64>,
) -> Option<NextStep> {
    let latest = summary.latest.as_ref().is_some_and(|a| a.id == audit);
    match next_step(summary, auditing) {
        NextStep::Review(id) => Some(NextStep::Review(id)),
        NextStep::Auditing(job) => Some(NextStep::Auditing(job)),
        step @ (NextStep::Repair | NextStep::MaybeRepair) if latest => Some(step),
        _ => None,
    }
}

/// What `audit` found, as its own page names it.
pub fn audit_state(audit: &AuditSummary) -> AuditState {
    if needs_review_from_counts(&audit.findings) > 0 {
        AuditState::NeedsReview
    } else if rests_on_claims(audit) {
        AuditState::OnClaims
    } else {
        AuditState::Verified
    }
}

/// What narrows or orders a list in the URL, cleared when a count opens one.
const NARROWING: [&str; 10] = [
    STATUS, FINDING, STATE, ALGORITHM, COVERED, "q", "sort", "row", "sel", "all",
];

/// The edit that opens `tab` with only `key` narrowing it, to `value`.
fn opened(tab: Option<&'static str>, key: &'static str, value: &str) -> Edit {
    let mut edit: Edit = vec![("tab", tab.map(str::to_string))];
    edit.extend(
        NARROWING
            .into_iter()
            .map(|k| (k, (k == key).then(|| value.to_string()))),
    );
    edit
}

/// The finding an entry of `status` has; a verified entry's is matched.
fn finding_of(status: FindingStatus) -> Finding {
    match status {
        FindingStatus::Verified | FindingStatus::Matched => Finding::Matched,
        FindingStatus::Misnamed => Finding::Misnamed,
        FindingStatus::Conflicting => Finding::Conflicting,
        FindingStatus::Mismatched => Finding::Mismatched,
        FindingStatus::Unmatched => Finding::Unmatched,
        FindingStatus::Unreadable => Finding::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use stage_select_api::{OpenRepair, SourceSetRecord, SourceSetStale};

    fn summary(review: usize, repair: Option<i64>) -> SourceSetSummary {
        SourceSetSummary {
            set: SourceSetRecord {
                name: "GB".into(),
                root: "sets".into(),
                path: "GB".into(),
                dat: "No-Intro/Nintendo - Game Boy".into(),
                added_at: "2026-01-01T00:00:00Z".into(),
                last_verified_against: None,
                follow: false,
                stale: Vec::new(),
                coverage: None,
                repairs_may_change: true,
                folder_created: None,
            },
            latest: Some(AuditSummary {
                id: 7,
                source_set: "GB".into(),
                started_at: "2026-01-02T00:00:00Z".into(),
                finished_at: "2026-01-02T00:01:00Z".into(),
                dat_revision_sha256: "ab".into(),
                dat_revision_version: "1".into(),
                findings: BTreeMap::from([(Finding::Matched, 5), (Finding::Misnamed, review)]),
                games: BTreeMap::new(),
                nothing_to_check_found: 0,
                pruned: false,
                evidence: Default::default(),
                claimed: Some(0),
                warnings: BTreeMap::new(),
                header_rule: None,
            }),
            repair: repair.map(|id| OpenRepair {
                id,
                audit: 7,
                approved: false,
                job: None,
            }),
            outside_coverage: None,
            covered_entries: None,
            coverage_unavailable: false,
        }
    }

    fn stale(mut summary: SourceSetSummary) -> SourceSetSummary {
        summary.set.stale = vec![SourceSetStale::FilesChanged {
            walk: 1,
            found_at: "2026-01-03T00:00:00Z".into(),
            changed: 1,
            added: 0,
            removed: 0,
        }];
        summary
    }

    fn found(findings: &[(Finding, usize)], games: &[(GameStatus, usize)]) -> SourceSetSummary {
        let mut s = summary(0, None);
        let latest = s.latest.as_mut().unwrap();
        latest.findings = findings.iter().copied().collect();
        latest.games = games.iter().copied().collect();
        s
    }

    #[test]
    fn a_source_set_leads_with_a_repair_only_when_its_latest_audit_found_what_a_repair_changes() {
        assert_eq!(next_step(&summary(2, None), None), NextStep::Repair);
        let incomplete = found(
            &[(Finding::Matched, 5)],
            &[(GameStatus::Complete, 4), (GameStatus::Incomplete, 1)],
        );
        assert_eq!(
            next_step(&incomplete, None),
            NextStep::Repair,
            "an incomplete game is one a repair completes"
        );
        // Files changed since: audit again first.
        assert_eq!(next_step(&stale(summary(2, None)), None), NextStep::Audit);
        let mut never = summary(0, None);
        never.latest = None;
        assert_eq!(next_step(&never, None), NextStep::Audit);
    }

    #[test]
    fn with_nothing_a_repair_surely_changes_an_audit_leads_and_a_repair_is_offered_second() {
        assert_eq!(
            next_step(&summary(0, None), None),
            NextStep::MaybeRepair,
            "spare copies or the storage policy may still give a repair something to do"
        );
        let unreadable = found(
            &[(Finding::Matched, 5), (Finding::Unreadable, 2)],
            &[(GameStatus::Complete, 5)],
        );
        assert_eq!(
            next_step(&unreadable, None),
            NextStep::MaybeRepair,
            "an unreadable file is left alone unless a replacement turns up"
        );
        assert_eq!(
            after_audit(7, &unreadable, None),
            Some(NextStep::MaybeRepair)
        );
        assert_eq!(after_audit(6, &unreadable, None), None);
    }

    #[test]
    fn an_open_repair_is_the_next_step_to_review() {
        assert_eq!(next_step(&summary(2, Some(3)), None), NextStep::Review(3));
        assert_eq!(
            next_step(&stale(summary(0, Some(3))), Some(9)),
            NextStep::Review(3)
        );
    }

    #[test]
    fn an_audit_offers_a_repair_only_while_it_is_the_latest() {
        assert_eq!(
            after_audit(7, &summary(2, None), None),
            Some(NextStep::Repair)
        );
        assert_eq!(after_audit(6, &summary(2, None), None), None);
        assert_eq!(
            after_audit(7, &summary(0, None), None),
            Some(NextStep::MaybeRepair)
        );
        assert_eq!(after_audit(7, &stale(summary(2, None)), None), None);
        assert_eq!(
            after_audit(6, &summary(2, Some(3)), None),
            Some(NextStep::Review(3))
        );
    }

    #[test]
    fn while_an_audit_is_queued_or_running_it_is_followed_rather_than_a_repair_proposed() {
        assert_eq!(next_step(&summary(2, None), Some(9)), NextStep::Auditing(9));
        assert_eq!(next_step(&summary(0, None), Some(9)), NextStep::Auditing(9));
        assert_eq!(
            after_audit(7, &summary(2, None), Some(9)),
            Some(NextStep::Auditing(9))
        );
        // An open repair stays the one to review.
        assert_eq!(
            next_step(&summary(2, Some(3)), Some(9)),
            NextStep::Review(3)
        );
        assert_eq!(
            after_audit(7, &summary(2, Some(3)), Some(9)),
            Some(NextStep::Review(3))
        );
    }

    fn job(id: i64, kind: &str, name: &str, state: &str) -> Job {
        serde_json::from_value(serde_json::json!({
            "id": id, "kind": kind, "state": state, "source_set": name, "dat_revision": "r", "actor": "Operator",
            "attempt": 1, "requeued_from": null, "audit": null, "created_at": "t",
            "workers": {"readers": 1, "hashers": 1}, "version": 1,
        }))
        .unwrap()
    }

    #[test]
    fn a_repairs_follow_up_audit_shows_after_a_reload_unless_a_newer_audit_was_seen() {
        assert_eq!(followed_audit(None, None), None);
        assert_eq!(
            followed_audit(None, Some(7)),
            Some(7),
            "ended before the page opened"
        );
        assert_eq!(followed_audit(Some(9), Some(7)), Some(9), "a newer audit");
        assert_eq!(followed_audit(Some(5), Some(7)), Some(7), "an older audit");
        assert_eq!(followed_audit(Some(5), None), Some(5));
    }

    #[test]
    fn the_audit_job_followed_is_the_newest_queued_or_running_audit_of_that_source_set() {
        let jobs = [
            job(6, "selection", "GB", "running"),
            job(5, "audit", "NES", "running"),
            job(4, "audit", "GB", "queued"),
            job(3, "audit", "GB", "running"),
            job(2, "audit", "GB", "succeeded"),
        ];
        assert_eq!(audit_job(&jobs, "GB"), Some(4));
        assert_eq!(audit_job(&jobs[..2], "GB"), None, "only an audit counts");
        assert_eq!(
            audit_job(&jobs[4..], "GB"),
            None,
            "an audit that ended isn't followed"
        );
    }

    #[test]
    fn an_audit_is_verified_only_with_nothing_to_review_and_nothing_on_claims() {
        let mut audit = summary(2, None).latest.unwrap();
        assert_eq!(audit_state(&audit), AuditState::NeedsReview);
        audit.findings = BTreeMap::from([(Finding::Matched, 5)]);
        assert_eq!(audit_state(&audit), AuditState::Verified);
        audit.claimed = Some(3);
        assert_eq!(audit_state(&audit), AuditState::OnClaims);
        audit.findings.insert(Finding::Unreadable, 1);
        assert_eq!(audit_state(&audit), AuditState::NeedsReview);
    }

    fn value(edit: &Edit, key: &str) -> Option<Option<String>> {
        edit.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn picking_a_game_count_opens_the_games_narrowed_to_it() {
        let edit = pick_game(ON_SOURCE_SET, Some(GameStatus::Missing), false);
        assert_eq!(value(&edit, "tab"), Some(Some("games".into())));
        assert_eq!(value(&edit, "status"), Some(Some("missing".into())));
        for key in [
            "finding",
            "state",
            "algorithm",
            "q",
            "sort",
            "row",
            "sel",
            "all",
        ] {
            assert_eq!(value(&edit, key), Some(None), "{key} is cleared");
        }
    }

    #[test]
    fn picking_a_finding_opens_the_entries_narrowed_to_it() {
        let edit = pick_finding(ON_SOURCE_SET, Some(FindingStatus::Misnamed));
        assert_eq!(value(&edit, "tab"), Some(Some("entries".into())));
        assert_eq!(value(&edit, "finding"), Some(Some("misnamed".into())));
        assert_eq!(
            value(&edit, "status"),
            Some(None),
            "no game status narrows the entries"
        );
        // An audit's entries are its first tab.
        let edit = pick_finding(ON_AUDIT, Some(FindingStatus::Unmatched));
        assert_eq!(value(&edit, "tab"), Some(None));
        assert_eq!(value(&edit, "finding"), Some(Some("unmatched".into())));
    }

    #[test]
    fn a_count_is_picked_only_while_its_list_is_open() {
        assert_eq!(
            active_game(ON_SOURCE_SET, Some("games"), Some("missing")),
            Some(GameStatus::Missing)
        );
        assert_eq!(
            active_game(ON_SOURCE_SET, Some("entries"), Some("missing")),
            None
        );
        assert_eq!(active_game(ON_SOURCE_SET, None, Some("missing")), None);
        assert_eq!(
            active_game(ON_SOURCE_SET, Some("games"), Some("bogus")),
            None
        );
        assert_eq!(
            active_finding(ON_SOURCE_SET, Some("entries"), Some("misnamed")),
            Some(FindingStatus::Misnamed)
        );
        assert_eq!(
            active_finding(ON_SOURCE_SET, Some("games"), Some("misnamed")),
            None
        );
        // An audit's entries are open on any tab but its games.
        assert_eq!(
            active_finding(ON_AUDIT, None, Some("unmatched")),
            Some(FindingStatus::Unmatched)
        );
        assert_eq!(
            active_finding(ON_AUDIT, Some("entries"), Some("unmatched")),
            Some(FindingStatus::Unmatched)
        );
        assert_eq!(
            active_finding(ON_AUDIT, Some("games"), Some("unmatched")),
            None
        );
    }

    #[test]
    fn a_game_count_of_covered_games_opens_only_the_covered_games() {
        let covered = pick_game(ON_SOURCE_SET, Some(GameStatus::Missing), true);
        assert_eq!(value(&covered, "status"), Some(Some("missing".into())));
        assert_eq!(value(&covered, "covered"), Some(Some("yes".into())));
        let whole = pick_game(ON_SOURCE_SET, Some(GameStatus::Missing), false);
        assert_eq!(value(&whole, "covered"), Some(None));
        assert_eq!(
            value(
                &pick_finding(ON_SOURCE_SET, Some(FindingStatus::Matched)),
                "covered"
            ),
            Some(None),
            "the entries aren't narrowed by coverage"
        );
        assert_eq!(
            pick_game(ON_SOURCE_SET, None, true),
            vec![("status", None), ("covered", None)]
        );
    }

    #[test]
    fn picking_none_only_clears_the_narrowing() {
        assert_eq!(
            pick_game(ON_AUDIT, None, false),
            vec![("status", None), ("covered", None)]
        );
        assert_eq!(pick_finding(ON_AUDIT, None), vec![("finding", None)]);
    }

    #[test]
    fn games_always_count_the_three_the_meter_draws() {
        let games = BTreeMap::from([(GameStatus::Missing, 4)]);
        assert_eq!(
            game_counts(&games),
            vec![
                (GameStatus::Complete, 0),
                (GameStatus::Incomplete, 0),
                (GameStatus::Missing, 4)
            ]
        );
        let games = BTreeMap::from([(GameStatus::Complete, 2), (GameStatus::NothingToCheck, 1)]);
        assert_eq!(
            game_counts(&games),
            vec![
                (GameStatus::Complete, 2),
                (GameStatus::Incomplete, 0),
                (GameStatus::Missing, 0),
                (GameStatus::NothingToCheck, 1)
            ]
        );
    }

    #[test]
    fn findings_count_only_those_found() {
        let findings = BTreeMap::from([
            (Finding::Unmatched, 2),
            (Finding::Matched, 10),
            (Finding::Misnamed, 0),
        ]);
        assert_eq!(
            finding_counts(&findings),
            vec![(FindingStatus::Matched, 10), (FindingStatus::Unmatched, 2)]
        );
    }
}
