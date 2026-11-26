//! The permissions view, a source set's and a library folder's Permissions tab: what its latest check found of its
//! files' permissions against your permission policy, apart from stale and drift. What Stage Select can restore is a
//! plan you approve, reviewed in the plan shape; what it can't comes with the command to run on the host.

use std::cmp::Ordering;
use std::sync::Arc;

use leptos::prelude::*;
use leptos_router::components::A;
use stage_select_api::Job;
use stage_select_api::permissions::{
    PermissionCause, PermissionDifference, PermissionsApproval, PermissionsRestored,
    PermissionsView,
};

use crate::api;
use crate::events::use_live;
use crate::model::when;
use crate::parts::list::{Clip, Column, ListRow, Width};
use crate::parts::message::{Live, Message, MessageKind, Said};
use crate::parts::outcome::{Reports, ShowReports};
use crate::parts::pending::Pending;
use crate::parts::plan::{self, ApproveBar, Change, PlanReview, PlanRow};
use crate::parts::plan_model::Approval;
use crate::parts::status::{ItemOutcome, Status, StatusBadge, StatusIcon};
use crate::permissions_model::{
    LastRestore, approve_label, change, found_line, listed, plan_line, policy_line, restorable,
    restored_line,
};

/// Where the permissions shown are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    SourceSet(String),
    Library(i64),
}

impl Place {
    /// Its path in the API: the source set's or the library folder's.
    fn base(&self) -> String {
        match self {
            Place::SourceSet(name) => format!("/source-sets/{}", api::segment(name)),
            Place::Library(id) => format!("/libraries/{id}"),
        }
    }

    /// Where a check of it is started: a stat walk of a source set, a library folder's check.
    fn check(&self) -> String {
        match self {
            Place::SourceSet(_) => format!("{}/stat-walks", self.base()),
            Place::Library(_) => format!("{}/checks", self.base()),
        }
    }
}

/// Where you set the permission policy.
const POLICY: &str = "/settings/roots#permission-policy";

/// A file or folder that differs from the policy, with what the last restore did to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    difference: PermissionDifference,
    /// `Some(true)` when the last restore failed for it, `Some(false)` when it restored it.
    failed: Option<bool>,
}

/// The list's columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Kind,
    Path,
    Change,
    Outcome,
}

impl PlanRow for Row {
    type Kind = PermissionCause;

    fn kind(&self) -> PermissionCause {
        self.difference.cause
    }

    fn outcome(&self) -> Option<ItemOutcome> {
        self.failed.map(|failed| {
            if failed {
                ItemOutcome::Failed
            } else {
                ItemOutcome::Done
            }
        })
    }
}

impl ListRow for Row {
    type Col = Col;
    const COLUMNS: &'static [Col] = &[Col::Kind, Col::Path, Col::Change, Col::Outcome];

    fn column(col: Col) -> Column {
        match col {
            Col::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("Who can give it your policy: Stage Select, through the plan, or you, on the host"),
            Col::Path => Column::new("path", "Path", Width::Flex { min: 12.0, share: 3 }).sortable(),
            Col::Change => Column::new("change", "Now → your policy", Width::Flex { min: 14.0, share: 2 }),
            Col::Outcome => plan::outcome_column(),
        }
    }

    fn key(&self) -> String {
        self.difference.path.clone()
    }

    fn title(&self) -> String {
        shown_path(&self.difference)
    }

    fn search(&self) -> String {
        format!(
            "{} {}",
            self.difference.path,
            self.difference.cause.look().label
        )
    }

    fn cell(&self, col: Col) -> AnyView {
        match col {
            Col::Kind => {
                view! { <StatusIcon status=self.difference.cause tab_stop=false /> }.into_any()
            }
            Col::Path => view! { <Clip text=shown_path(&self.difference) /> }.into_any(),
            Col::Change => {
                let (current, proposed) = change(&self.difference);
                view! { <Change current proposed /> }.into_any()
            }
            Col::Outcome => plan::outcome_cell(self.outcome()),
        }
    }

    fn compare(&self, other: &Self, col: Col) -> Ordering {
        match col {
            Col::Kind => self
                .difference
                .cause
                .rank()
                .cmp(&other.difference.cause.rank()),
            Col::Outcome => plan::compare_outcome(self.outcome(), other.outcome()),
            Col::Path | Col::Change => self.difference.path.cmp(&other.difference.path),
        }
    }
}

/// A difference's path, the folder itself named as such.
fn shown_path(d: &PermissionDifference) -> String {
    if d.path.is_empty() {
        "(this folder)".into()
    } else if d.folder {
        format!("{}/", d.path)
    } else {
        d.path.clone()
    }
}

/// A row's detail: what it is now, what your policy gives it, and who can restore it.
fn detail(row: Row) -> AnyView {
    let d = row.difference;
    let (current, proposed) = change(&d);
    view! {
        <dl class="facts">
            <dt>"Kind"</dt>
            <dd><StatusBadge status=d.cause /></dd>
            <dt>"Now"</dt>
            <dd>{format!("{current}, owned by uid {}", d.uid)}</dd>
            <dt>"Your policy"</dt>
            <dd>{proposed}</dd>
        </dl>
        <p>{d.cause.look().hint}</p>
    }
    .into_any()
}

/// The Permissions tab of `place`.
#[component]
pub fn PermissionsTab(place: Place) -> impl IntoView {
    let place = StoredValue::new(place);
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let found = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let path = format!("{}/permissions", place.get_value().base());
        async move { api::get::<PermissionsView>(&path).await }
    });
    let reports = Reports::new();
    let said = RwSignal::new(None::<Result<String, String>>);
    let busy = RwSignal::new(false);
    let last = RwSignal::new(None::<LastRestore>);
    let check = move |_| {
        reports.clear();
        said.set(None);
        last.set(None);
        let path = place.get_value().check();
        leptos::task::spawn_local(async move {
            let started = api::post::<_, Job>(&path, &()).await;
            reports.started(started, "Couldn't check it");
        });
    };
    let restore = Callback::new(
        move |(digest, planned): (String, Vec<PermissionDifference>)| {
            busy.set(true);
            let path = format!("{}/permissions/restore", place.get_value().base());
            leptos::task::spawn_local(async move {
                let done =
                    api::post::<_, PermissionsRestored>(&path, &PermissionsApproval { digest })
                        .await;
                said.set(Some(match &done {
                    Ok(restored) => Ok(restored_line(restored)),
                    Err(problem) => Err(format!("Couldn't restore the permissions: {problem}")),
                }));
                last.set(done.ok().map(|restored| LastRestore { planned, restored }));
                busy.set(false);
                refresh.update(|n| *n += 1);
            });
        },
    );
    let problem = Signal::derive(move || found.get().and_then(Result::err));
    view! {
        <div class="actions">
            <button type="button" on:click=check>"Check again"</button>
        </div>
        <ShowReports reports />
        <Said said />
        <Pending pending=Signal::derive(move || found.get().is_none()) status="Loading its permissions…" problem>
            {move || {
                found
                    .get()
                    .and_then(Result::ok)
                    .map(|found| shown(found, last.get(), restore, busy))
            }}
        </Pending>
    }
}

/// What the check found, reviewed as the plan to restore what Stage Select can, with what to run on the host for
/// the rest.
fn shown(
    found: PermissionsView,
    last: Option<LastRestore>,
    restore: Callback<(String, Vec<PermissionDifference>)>,
    busy: RwSignal<bool>,
) -> AnyView {
    let Some(policy) = found.policy else {
        return view! {
            <p>
                "You haven't set a permission policy, so files keep the host's defaults and nothing is compared. "
                <A href=POLICY>"Set one under Roots"</A>
                "."
            </p>
        }
        .into_any();
    };
    let Some(report) = found.report else {
        return view! {
            <p>
                {format!("Your policy here is {}. ", policy_line(&policy))}
                "It hasn't been checked since Stage Select started: check it to compare its files with your policy."
            </p>
        }
        .into_any();
    };
    let stale = (!report.current).then(|| {
        view! {
            <Message kind=MessageKind::NOTICE live=Live::Off>
                "Your permission policy changed since this check. Check again to compare with it."
            </Message>
        }
    });
    let fixes = report
        .fixes
        .iter()
        .map(|fix| {
            let explanation = fix.explanation.clone();
            let command = fix.command.clone();
            view! {
                <Message kind=MessageKind::NOTICE live=Live::Off>
                    <p>{explanation}</p>
                    <pre class="command"><code>{command}</code></pre>
                </Message>
            }
        })
        .collect_view();
    let summary = plan_line(&report.differences)
        .unwrap_or_else(|| found_line(report.differences.len(), report.compared));
    // Approve, only while there is a plan: what Stage Select owns that differs, bound by its digest.
    let plan = report.plan_digest.clone().map(|digest| {
        let planned: Vec<_> = restorable(&report.differences).cloned().collect();
        (digest, planned, approve_label(&report.differences))
    });
    let approve = ViewFn::from(move || {
        let Some((digest, planned, label)) = plan.clone() else {
            return ().into_any();
        };
        view! {
            <ApproveBar
                label
                approval=Approval {
                    changed: 0,
                    blocked: planned.is_empty(),
                    stale: false,
                }
                on_approve=Callback::new(move |()| restore.run((digest.clone(), planned.clone())))
                busy
            />
        }
        .into_any()
    });
    let rows = Arc::new(
        listed(&report.differences, last.as_ref())
            .into_iter()
            .map(|(difference, failed)| Row { difference, failed })
            .collect::<Vec<_>>(),
    );
    let checked = format!(
        "{} Checked {} against {}.",
        found_line(report.differences.len(), report.compared),
        when(&report.checked_at),
        policy_line(&report.policy),
    );
    view! {
        {stale}
        {fixes}
        <PlanReview
            id="permissions"
            label="Differences from your permission policy"
            rows=Signal::stored(rows)
            empty="Nothing differs from your permission policy."
            summary=Signal::stored(summary)
            placeholder="Filter by path or kind"
            detail=Callback::new(detail)
            approve=approve
            help=move || view! {
                <p>{checked.clone()}</p>
                <p>
                    "Your permission policy is the mode and group you want files and folders to have. Stage Select \
                     gives it to everything it writes, and compares it with what each check sees. A difference never \
                     makes a source set stale or a library folder drift, and never stops an audit or a build."
                </p>
                <p>
                    "Stage Select changes the mode only of files it owns, and the group only to one it is in. For the \
                     rest it says what to run on the host. It never changes a source set it may not change. The plan \
                     binds the files as this check saw them: if any changed since, check again."
                </p>
            }
        />
    }
    .into_any()
}
