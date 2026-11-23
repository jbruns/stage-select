//! An ingest plan, for you to review and approve, in the shared plan shape (`parts::plan`): what it does with each
//! item, as rows reading item → destination with the kind of move beside them and its outcome once moved, and each
//! item's reason and result in its detail. A plan does one thing with every item and you approve it whole, so it
//! offers no choices to set.

use std::cmp::Ordering;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use stage_select_api::build::ApprovePlan;
use stage_select_api::ingest::{IngestAction, IngestOutcome, IngestPlan, IngestPlanItem};

use crate::api;
use crate::app::Loading;
use crate::events::use_live;
use crate::ingest_model::{self, Outcome};
use crate::model::{short_sha, when};
use crate::parts::list::{Column, ListRow, Width, last_good_rows};
use crate::parts::message::{Live, Message, MessageKind, Problem};
use crate::parts::outcome::{Link, Made};
use crate::parts::pending::ActionPending;
use crate::parts::plan::{self, ApproveBar, Change, PlanReview, PlanRow};
use crate::parts::plan_model::Approval;
use crate::parts::status::{IngestPlanKind, ItemOutcome, Status, StatusBadge, StatusIcon};

/// One item of the plan under review, with how its move ended once you approved the plan.
#[derive(Debug, Clone, PartialEq)]
struct IngestRow {
    item: IngestPlanItem,
    kind: IngestPlanKind,
    outcome: Option<IngestOutcome>,
    result: Outcome,
    made: Option<Link>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanCol {
    Kind,
    Change,
    Outcome,
}

impl PlanRow for IngestRow {
    type Kind = IngestPlanKind;
    fn kind(&self) -> IngestPlanKind {
        self.kind
    }

    fn outcome(&self) -> Option<ItemOutcome> {
        self.result.item_outcome()
    }
}

impl ListRow for IngestRow {
    type Col = PlanCol;
    const COLUMNS: &'static [PlanCol] = &[PlanCol::Kind, PlanCol::Change, PlanCol::Outcome];

    fn column(col: PlanCol) -> Column {
        match col {
            PlanCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the plan does with the item"),
            PlanCol::Change => Column::new(
                "item",
                "Item → destination",
                Width::Flex {
                    min: 18.0,
                    share: 4,
                },
            )
            .sortable(),
            PlanCol::Outcome => plan::outcome_column(),
        }
    }

    fn key(&self) -> String {
        self.item.item.to_string()
    }

    fn title(&self) -> String {
        self.item.name.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {}",
            self.item.name,
            self.item.destination,
            self.kind.look().label,
            self.result.label()
        )
    }

    fn cell(&self, col: PlanCol) -> AnyView {
        match col {
            PlanCol::Kind => view! { <StatusIcon status=self.kind tab_stop=false /> }.into_any(),
            PlanCol::Change => view! {
                <Change current=self.item.name.clone() proposed=self.item.destination.clone() />
            }
            .into_any(),
            PlanCol::Outcome => plan::outcome_cell(self.outcome()),
        }
    }

    fn compare(&self, other: &Self, col: PlanCol) -> Ordering {
        match col {
            PlanCol::Kind => self.kind.rank().cmp(&other.kind.rank()),
            PlanCol::Change => self.item.name.cmp(&other.item.name),
            PlanCol::Outcome => plan::compare_outcome(self.outcome(), other.outcome()),
        }
    }
}

fn rows(plan: &IngestPlan) -> Vec<IngestRow> {
    let kind = IngestPlanKind::from(plan.content.action);
    plan.content
        .items
        .iter()
        .map(|item| {
            let outcome = plan.outcomes.iter().find(|o| o.item == item.item).cloned();
            IngestRow {
                result: ingest_model::outcome(plan, outcome.as_ref()),
                made: outcome.as_ref().and_then(|o| {
                    ingest_model::made_link(plan.content.action, item.addition.as_ref(), o)
                }),
                item: item.clone(),
                kind,
                outcome,
            }
        })
        .collect()
}

fn row_detail(row: IngestRow) -> AnyView {
    let IngestRow {
        item,
        kind,
        outcome,
        result,
        made,
    } = row;
    let ended = outcome.and_then(|o| o.error.or(o.detail));
    view! {
        <dl class="facts">
            <dt>"Does"</dt>
            <dd>
                <StatusBadge status=kind />
            </dd>
            <dt>"Item"</dt>
            <dd><code>{item.name.clone()}</code></dd>
            <dt>"Goes to"</dt>
            <dd><code>{item.destination.clone()}</code></dd>
            <dt>"Why"</dt>
            <dd>{item.reason.clone()}</dd>
            {item.dat.clone().map(|d| view! {
                <dt>"DAT"</dt>
                <dd>{d}</dd>
            })}
            {item.addition.clone().map(|a| view! {
                <dt>"Added as"</dt>
                <dd>
                    {format!("{} in {}", a.name, a.system)}
                    {a.parent.map(|p| format!(", a clone of {p}"))}
                </dd>
            })}
            {item.addition_target.clone().map(|t| view! {
                <dt>"Published as"</dt>
                <dd>
                    <code>{t.file_name}</code>
                    {t.loose.then_some(", loose")}
                </dd>
            })}
            {crate::ingest_model::plan_checksum(&item).map(|sha| view! {
                <dt>"SHA-256"</dt>
                <dd><code class="hash">{sha.to_string()}</code></dd>
            })}
            <dt>"Result"</dt>
            <dd>
                {result.label()}
                {ended.map(|e| format!(": {e}"))}
                {made.map(|l| view! { " · " <A href=l.href>{l.label}</A> })}
            </dd>
        </dl>
    }
    .into_any()
}

fn action_text(action: IngestAction) -> &'static str {
    match action {
        IngestAction::Discard => "Discard: each item is set aside in the folder's kept files.",
        IngestAction::DatImport => {
            "Import: each DAT or support file is imported, then set aside in the folder's kept files."
        }
        IngestAction::Addition => {
            "Add as additions: each is repacked into its system's additions set, its original set aside once the \
             copy reads back exactly."
        }
    }
}

/// The plan `plan`, loaded and listed, with approving it. `refresh` is the shared ingest counter: approving bumps it,
/// and the plan reloads with it.
#[component]
pub fn IngestPlanReview(plan: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move { api::get::<IngestPlan>(&format!("/ingest-plans/{plan}")).await }
    });
    // The last plan loaded, so a reload doesn't redraw the list.
    let latest = Memo::new(
        move |prev: Option<&Option<IngestPlan>>| match loaded.get() {
            Some(Ok(p)) => Some(p),
            _ => prev.cloned().flatten(),
        },
    );
    let approving = Action::new_local(move |p: &IngestPlan| {
        let (id, body) = (
            p.id,
            ApprovePlan {
                digest: p.digest.clone(),
            },
        );
        async move {
            let done =
                api::post::<_, IngestPlan>(&format!("/ingest-plans/{id}/approval"), &body).await;
            if done.is_ok() {
                refresh.update(|n| *n += 1);
            }
            done
        }
    });
    let list_rows = last_good_rows(move || loaded.get().map(|p| p.map(|p| rows(&p))));

    let summary = Signal::derive(move || {
        latest.with(|p| {
            p.as_ref().map_or_else(String::new, |p| {
                ingest_model::plan_line(p.content.action, p.content.items.len())
            })
        })
    });
    // Approve sits above the list; once approved, where the moves stand takes its place.
    let approve = move || {
        latest.get().map(|p| {
            let n = p.content.items.len();
            // Once finished, what the plan made is one click away.
            let state = match (&p.approval, &p.finished_at) {
                (None, _) => None,
                (Some(a), None) => Some(view! {
                    <Message kind=MessageKind::NOTICE live=Live::Off>
                        {format!("You approved it {}. It's moving the items now.", when(&a.at))}
                    </Message>
                }
                .into_any()),
                (Some(_), Some(at)) => Some(view! {
                    <Made text=format!("Finished {}.", when(at)) link=ingest_model::plan_made_link(&p) />
                }
                .into_any()),
            };
            let label = if n == 1 {
                "Approve and move 1 item".to_string()
            } else {
                format!("Approve and move {n} items")
            };
            let approve = p.approval.is_none().then(|| {
                let p = p.clone();
                view! {
                    <ApproveBar
                        label
                        approval=Approval::default()
                        busy=approving.pending()
                        on_approve=Callback::new(move |()| { approving.dispatch_local(p.clone()); })
                    />
                }
            });
            view! {
                {state}
                {approve}
            }
        })
    };
    let help = move || {
        latest.get().map(|p| {
            view! {
                <p>{action_text(p.content.action)}</p>
                <p>
                    "Digest " <code class="hash">{short_sha(&p.digest).to_string()}</code>
                    {format!(
                        ". It binds the {} entries of the folder as they are now; if anything there changes before \
                         you approve, plan again.",
                        p.content.fingerprint.len()
                    )}
                </p>
            }
        })
    };

    view! {
        <section aria-labelledby="plan-title">
            <h2 id="plan-title">{format!("Plan #{plan}")}</h2>
            {move || match loaded.get() {
                None if latest.with(Option::is_none) => view! { <Loading what="the plan" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                _ => ().into_any(),
            }}
            <PlanReview
                id="ingest-plan"
                label=format!("Plan #{plan}")
                rows=list_rows
                empty="The plan moves nothing."
                summary
                placeholder="Filter by item, destination or result"
                detail=Callback::new(row_detail)
                approve=move || view! {
                    <ActionPending action=approving status="Approving…">
                        {approve}
                    </ActionPending>
                }
                help
            />
        </section>
    }
}
