//! The permissions view, a source set's and a library folder's Permissions tab: what its latest check found of its
//! files' permissions against your permission policy, apart from stale and drift. What Stage Select can restore is a
//! plan you approve; what it can't comes with the command to run on the host.

use std::cmp::Ordering;
use std::sync::Arc;

use leptos::prelude::*;
use leptos_router::components::A;
use stage_select_api::Job;
use stage_select_api::permissions::{
    PermissionDifference, PermissionsApproval, PermissionsRestored, PermissionsView,
};

use crate::api;
use crate::events::use_live;
use crate::model::when;
use crate::parts::help::Help;
use crate::parts::list::{Column, List, ListRow, Width};
use crate::parts::message::{Live, Message, MessageKind, Said};
use crate::parts::outcome::{Reports, ShowReports};
use crate::parts::pending::Pending;
use crate::parts::plan::{ApproveBar, Change};
use crate::parts::plan_model::Approval;
use crate::permissions_model::{
    approve_label, cause_label, cause_reason, change, found_line, plan_line, policy_line,
    restored_line,
};

/// Where the permissions shown are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    SourceSet(String),
    Library(i64),
}

impl Place {
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

/// A file or folder that differs from the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row(PermissionDifference);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Path,
    Change,
    Who,
}

impl ListRow for Row {
    type Col = Col;
    const COLUMNS: &'static [Col] = &[Col::Path, Col::Change, Col::Who];

    fn column(col: Col) -> Column {
        match col {
            Col::Path => Column::new("path", "Path", Width::Flex { min: 12.0, share: 3 }).sortable(),
            Col::Change => Column::new("change", "Now → your policy", Width::Flex { min: 14.0, share: 2 }),
            Col::Who => Column::new("who", "Restored by", Width::Fixed(9.0))
                .sortable()
                .hint("Who can give it your policy: Stage Select, through the plan, or you, on the host"),
        }
    }

    fn key(&self) -> String {
        self.0.path.clone()
    }

    fn title(&self) -> String {
        shown_path(&self.0)
    }

    fn search(&self) -> String {
        format!("{} {}", self.0.path, cause_label(self.0.cause))
    }

    fn cell(&self, col: Col) -> AnyView {
        match col {
            Col::Path => view! { <crate::parts::list::Clip text=shown_path(&self.0) /> }.into_any(),
            Col::Change => {
                let (current, proposed) = change(&self.0);
                view! { <Change current proposed /> }.into_any()
            }
            Col::Who => cause_label(self.0.cause).into_any(),
        }
    }

    fn compare(&self, other: &Self, col: Col) -> Ordering {
        match col {
            Col::Who => self.0.cause.cmp(&other.0.cause),
            _ => self.0.path.cmp(&other.0.path),
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

fn detail(row: Row) -> AnyView {
    let d = row.0;
    let (current, proposed) = change(&d);
    view! {
        <dl class="facts">
            <dt>"Now"</dt>
            <dd>{format!("{current}, owned by uid {}", d.uid)}</dd>
            <dt>"Your policy"</dt>
            <dd>{proposed}</dd>
        </dl>
        <p>{cause_reason(d.cause)}</p>
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
    let check = move |_| {
        reports.clear();
        said.set(None);
        let path = place.get_value().check();
        leptos::task::spawn_local(async move {
            let started = api::post::<_, Job>(&path, &()).await;
            reports.started(started, "Couldn't check it");
        });
    };
    let restore = Callback::new(move |digest: String| {
        busy.set(true);
        let path = format!("{}/permissions/restore", place.get_value().base());
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, PermissionsRestored>(&path, &PermissionsApproval { digest }).await;
            said.set(Some(match done {
                Ok(restored) => Ok(restored_line(&restored)),
                Err(problem) => Err(format!("Couldn't restore the permissions: {problem}")),
            }));
            busy.set(false);
            refresh.update(|n| *n += 1);
        });
    });
    let problem = Signal::derive(move || found.get().and_then(Result::err));
    view! {
        <div class="actions">
            <button type="button" on:click=check>"Check again"</button>
        </div>
        <ShowReports reports />
        <Said said />
        <Pending pending=Signal::derive(move || found.get().is_none()) status="Loading its permissions…" problem>
            {move || {
                found.get().and_then(Result::ok).map(|found| shown(found, restore, busy))
            }}
        </Pending>
        <Help summary="What your permission policy does">
            <p>
                "Your permission policy is the mode and group you want files and folders to have. Stage Select gives \
                 it to everything it writes, and compares it with what each check sees. A difference never makes a \
                 source set stale or a library folder drift, and never stops an audit or a build."
            </p>
            <p>
                "Stage Select changes the mode only of files it owns, and the group only to one it is in. For the \
                 rest it says what to run on the host. It never changes a source set it may not change."
            </p>
        </Help>
    }
}

/// What the check found, with the plan to restore and what to run on the host.
fn shown(found: PermissionsView, restore: Callback<String>, busy: RwSignal<bool>) -> AnyView {
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
    let plan = report.plan_digest.clone().and_then(|digest| {
        let line = plan_line(&report.differences)?;
        let label = approve_label(&report.differences);
        let changed = report
            .differences
            .iter()
            .filter(|d| d.cause == stage_select_api::permissions::PermissionCause::Restorable)
            .count();
        Some(view! {
            <h2>"Restore"</h2>
            <p>{line}</p>
            <ApproveBar
                label=label
                approval=Approval {
                    changed: 0,
                    blocked: changed == 0,
                    stale: false,
                }
                on_approve=Callback::new(move |()| restore.run(digest.clone()))
                busy
            />
        })
    });
    let rows = Arc::new(
        report
            .differences
            .iter()
            .cloned()
            .map(Row)
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
        <p>{checked}</p>
        {fixes}
        {plan}
        <List
            id="permissions"
            label="Differences from your permission policy"
            rows=Signal::stored(rows)
            empty="Nothing differs from your permission policy."
            placeholder="Filter by path or who restores it"
            detail=Callback::new(detail)
        />
    }
    .into_any()
}
