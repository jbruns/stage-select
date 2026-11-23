//! A Library Folder's latest plan, for you to review and approve: a build plan, or a cleanup plan. Both are reviewed
//! in the shared plan shape (`parts::plan`): the summary line, the counts by kind, Approve, the outcomes once applied,
//! the list and its notes. Your choices, for a row, the rows you check or every item of a kind, wait until you plan
//! again, since approving approves the plan as shown.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use stage_select_api::Job;
use stage_select_api::additions::NewAddition;
use stage_select_api::build::{
    ApprovePlan, BlockerKind, BuildDetail, BuildSummary, FolderCheck, KeptFile, LibraryFolder,
    Plan, PlanChoices, PlanCleanup, PlanContent, RETENTION, RemovalKind,
};

use crate::api;
use crate::app::Loading;
use crate::build_model::{
    self, ConflictChoice, Placing, approve_label, blocker_label, cause_label, group_href,
    item_change, keep_reason_label, place_facts, plan_kinds, return_facts, return_games,
    space_lines, with_conflict_choices,
};
use crate::cleanup_model::{self, CleanupChoice, CleanupItem, Found};
use crate::events::use_live;
use crate::model::when;
use crate::parts::icon::Glyph;
use crate::parts::list::{Clip, Column, ListRow, Width};
use crate::parts::message::{Live, Message, MessageKind, Problem};
use crate::parts::outcome::{JobOutcome, job_href};
use crate::parts::plan::{
    self, ApproveBar, Change, PlanReview, PlanRow, apply_to_all, set_actions,
};
use crate::parts::plan_model::{Approval, Picks, offered_by_any};
use crate::parts::status::{BuildKind, CleanupKind, ItemOutcome, Status, StatusIcon};
use crate::screens::additions::{AdditionFields, new_addition};
use crate::screens::library_folders::use_planner;

/// What approving a plan that places or returns games does to your source set.
const CHANGES_SOURCE_SET: &str = "Approving this plan also changes your source set: each game placed is taken out \
of it, and each game returned is put back. A file that changed since the plan was made is never taken: it stays in \
your source set, or in its kept files.";

/// The plan `plan` of the folder `folder`, loaded. `refresh` reloads the folder once you approve or plan again.
#[component]
pub fn LibraryPlan(plan: i64, folder: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async move { api::get::<Plan>(&format!("/plans/{plan}")).await }
    });
    move || match loaded.get() {
        None => view! { <Loading what="the plan" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(plan)) if plan.content.cleanup.is_some() => {
            let plan = Some(plan);
            let on_planned = Callback::new(move |()| refresh.update(|n| *n += 1));
            view! { <CleanupLoad plan folder refresh on_planned /> }.into_any()
        }
        Some(Ok(plan)) => view! { <PlanView plan folder refresh /> }.into_any(),
    }
}

/// Approving a plan by the digest shown.
#[derive(Clone, Copy)]
struct Approver {
    id: i64,
    digest: StoredValue<String>,
    busy: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
    refresh: RwSignal<u64>,
}

impl Approver {
    fn new(plan: &Plan, refresh: RwSignal<u64>) -> Self {
        Approver {
            id: plan.id,
            digest: StoredValue::new(plan.digest.clone()),
            busy: RwSignal::new(false),
            problem: RwSignal::new(None),
            refresh,
        }
    }

    fn approve(self) {
        self.busy.set(true);
        self.problem.set(None);
        let request = ApprovePlan {
            digest: self.digest.get_value(),
        };
        let id = self.id;
        leptos::task::spawn_local(async move {
            let approved = api::post::<_, Job>(&format!("/plans/{id}/approval"), &request).await;
            self.busy.set(false);
            match approved {
                // The plan, loaded again, names its build job, so its progress outlives this view.
                Ok(_) => self.refresh.update(|n| *n += 1),
                Err(message) => self.problem.set(Some(message)),
            }
        });
    }

    fn problem(self) -> impl IntoView {
        move || {
            self.problem
                .get()
                .map(|message| view! { <Problem message /> })
        }
    }
}

/// The build that carried out `plan`, once approved, for what became of each item: none before.
fn use_build(folder: i64, plan: &Plan) -> LocalResource<Option<BuildDetail>> {
    let live = use_live();
    let (id, job) = (plan.id, plan.job);
    LocalResource::new(move || {
        live.settled.track();
        async move {
            job?;
            let builds = api::get::<Vec<BuildSummary>>(&format!("/libraries/{folder}/builds"))
                .await
                .ok()?;
            let build = builds.into_iter().find(|b| b.plan == id)?;
            api::get::<BuildDetail>(&format!("/builds/{}", build.id))
                .await
                .ok()
        }
    })
}

/// The plan's heading and where its approval stands.
fn heading(plan: &Plan, what: &str) -> impl IntoView + use<> {
    let state = match &plan.approval {
        Some(a) => format!("Approved by {} · {}", a.actor, when(&a.at)),
        None => "Waiting on your approval".to_string(),
    };
    view! {
        <h2 id="latest-plan-title">{format!("{what} #{}", plan.id)}</h2>
        <p class="note">{state}</p>
    }
}

/// What the plan was planned from, for its notes.
fn planned_from(plan: &Plan) -> impl IntoView + use<> {
    let selection = plan.content.selection;
    view! {
        <dl class="facts">
            <dt>"Selection"</dt>
            <dd>
                <A href=format!("/selections/{selection}")>{format!("#{selection}")}</A>
                {plan.previous_selection.map(|p| format!(", replacing #{p} of the current build"))}
            </dd>
            <dt>"Audit"</dt>
            <dd>
                {match plan.content.audit {
                    Some(a) => view! { <A href=format!("/audits/{a}")>{format!("#{a}")}</A> }.into_any(),
                    None => "None".into_any(),
                }}
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                <super::dats::RevisionName sha256=plan.content.dat_revision.clone() />
            </dd>
            <dt>"Storage"</dt>
            <dd>{crate::screens::repair::storage_line(&plan.content.storage)}</dd>
            <dt>"Planned"</dt>
            <dd>{when(&plan.created_at)}</dd>
            <dt>"Digest"</dt>
            <dd>
                <code class="hash">{plan.digest.clone()}</code>
            </dd>
            {plan.job.map(|j| view! {
                <dt>"Build job"</dt>
                <dd><A href=job_href(j)>{format!("#{j}")}</A></dd>
            })}
        </dl>
    }
}

/// Your picks for a plan's choices, staged until you plan again, and what the last bulk choice did.
#[derive(Clone, Copy)]
struct Chooser<C: Send + Sync + 'static> {
    picks: RwSignal<Picks<C>>,
    said: RwSignal<Option<String>>,
}

// A plan's rows share its one chooser.
impl<C: Send + Sync + 'static> PartialEq for Chooser<C> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<C: Copy + Eq + Send + Sync + 'static> Chooser<C> {
    fn new() -> Self {
        Chooser {
            picks: RwSignal::new(Picks::default()),
            said: RwSignal::new(None),
        }
    }

    /// Picks `choice` for each of `items` (key, planned choice, offers) that offers it, and says what it did.
    fn set<'a>(
        self,
        items: impl IntoIterator<Item = (&'a str, C, &'a [C])>,
        choice: C,
        label: fn(C) -> &'static str,
    ) {
        let outcome = self.picks.try_update(|p| p.set(items, choice));
        self.said.set(outcome.map(|o| o.message(label(choice))));
    }

    /// The choice now of the item `key`, which the plan gives `planned`.
    fn now(self, key: &str, planned: C) -> Signal<C> {
        let key = key.to_string();
        Signal::derive(move || self.picks.with(|p| p.now(&key, planned)))
    }

    /// The choice of the item `key`, as the plan's shared `<select>`.
    fn select(
        self,
        key: &str,
        named: String,
        offers: Vec<C>,
        planned: C,
        label: fn(C) -> &'static str,
    ) -> AnyView {
        let picked = key.to_string();
        plan::choice_select(
            format!("What to do with {named}"),
            offers,
            planned,
            self.now(key, planned),
            label,
            Callback::new(move |c| self.picks.update(|p| p.pick(&picked, c))),
        )
    }

    /// How many of `planned` (key, planned choice) you picked otherwise.
    fn changed(self, planned: &[(String, C)]) -> usize {
        self.picks
            .with(|p| p.changed(planned.iter().map(|(k, c)| (k.as_str(), *c))))
    }
}

/// One item of a build plan: a file it writes or skips, or a built file it removes.
#[derive(Clone, PartialEq)]
struct ItemRow {
    kind: BuildKind,
    /// What else it does, such as setting a file aside.
    also: Option<BuildKind>,
    key: String,
    /// The file its build item is found by.
    target: String,
    game: String,
    current: String,
    proposed: String,
    /// What the detail says of it besides, by label.
    facts: Vec<(&'static str, String)>,
    offers: Vec<ConflictChoice>,
    chosen: ConflictChoice,
    chooser: Option<Chooser<ConflictChoice>>,
    done: Option<ItemOutcome>,
}

impl ItemRow {
    /// A row named `key`, which is what it is now, becoming `proposed`. `plan_rows` sets its kinds.
    fn new(key: &str, game: &str, proposed: impl Into<String>) -> Self {
        ItemRow {
            kind: BuildKind::Unchanged,
            also: None,
            key: key.to_string(),
            target: key.to_string(),
            game: game.to_string(),
            current: key.to_string(),
            proposed: proposed.into(),
            facts: Vec::new(),
            offers: Vec::new(),
            chosen: ConflictChoice::Leave,
            chooser: None,
            done: None,
        }
    }

    fn fact(mut self, label: &'static str, text: impl Into<Option<String>>) -> Self {
        self.facts.extend(text.into().map(|t| (label, t)));
        self
    }

    fn facts(mut self, facts: Vec<(&'static str, String)>) -> Self {
        self.facts.extend(facts);
        self
    }

    /// The choice, where you may change it.
    fn choosing(&self) -> Option<Chooser<ConflictChoice>> {
        self.chooser.filter(|_| !self.offers.is_empty())
    }

    fn choice(&self) -> AnyView {
        match self.choosing() {
            Some(c) => c.select(
                &self.key,
                self.key.clone(),
                self.offers.clone(),
                self.chosen,
                ConflictChoice::label,
            ),
            None => (!self.offers.is_empty())
                .then_some(self.chosen.label())
                .into_any(),
        }
    }
}

/// A build plan's rows: its returns, items and removals.
fn plan_rows(content: &PlanContent, chooser: Option<Chooser<ConflictChoice>>) -> Vec<ItemRow> {
    let items = content.items.iter().map(|i| {
        let (current, proposed) = item_change(i);
        ItemRow {
            offers: ConflictChoice::offered(i),
            chosen: ConflictChoice::of(i, &content.choices),
            chooser,
            current,
            ..ItemRow::new(&i.target, &i.game, proposed)
                .fact("Why", i.reason.clone())
                .fact(
                    "Set aside first",
                    i.keep
                        .as_ref()
                        .map(|k| format!("{} ({})", k.name, keep_reason_label(k.reason))),
                )
                .fact(
                    "Converts",
                    i.converts.as_ref().map(|c| {
                        format!(
                            "{}, stored the other way, set aside once this is written",
                            c.name
                        )
                    }),
                )
                .fact("ROMs", i.roms.len().to_string())
                .facts(i.place.as_ref().map(place_facts).unwrap_or_default())
        }
    });
    // Returns run first in the build.
    let returns = return_games(content).into_iter().map(|r| {
        // Apart from any item for the same file, such as the built copy a still selected game leaves.
        let key = format!("{} (return)", r.current.first().unwrap_or(&r.game));
        ItemRow {
            target: r.current.first().cloned().unwrap_or_default(),
            current: r.current.join(", "),
            ..ItemRow::new(&key, &r.game, r.proposed.join(", ")).facts(return_facts(&r))
        }
    });
    let removals = content.removals.iter().map(|r| {
        let to = match r.kind {
            RemovalKind::Keep => "kept files",
            RemovalKind::Forget => "forgotten",
            RemovalKind::Leave => "left as it is",
        };
        ItemRow::new(&r.target, &r.game, to).fact("Why", r.reason.clone())
    });
    with_kinds(content, returns.chain(items).chain(removals))
}

/// `rows`, listed as `plan_kinds` lists them, with their kinds.
fn with_kinds(content: &PlanContent, rows: impl Iterator<Item = ItemRow>) -> Vec<ItemRow> {
    rows.zip(plan_kinds(content))
        .map(|(row, (kind, also))| ItemRow { kind, also, ..row })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemCol {
    Kind,
    Game,
    Change,
    Choice,
    Outcome,
}

impl PlanRow for ItemRow {
    type Kind = BuildKind;
    fn kind(&self) -> BuildKind {
        self.kind
    }

    fn also(&self) -> Option<BuildKind> {
        self.also
    }

    fn outcome(&self) -> Option<ItemOutcome> {
        self.done
    }
}

impl ListRow for ItemRow {
    type Col = ItemCol;
    const COLUMNS: &'static [ItemCol] = &[
        ItemCol::Kind,
        ItemCol::Game,
        ItemCol::Change,
        ItemCol::Choice,
        ItemCol::Outcome,
    ];

    fn column(col: ItemCol) -> Column {
        match col {
            ItemCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the plan does with the file"),
            ItemCol::Game => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 10.0,
                    share: 1,
                },
            )
            .sortable(),
            ItemCol::Change => Column::new(
                "change",
                "Current → proposed",
                Width::Flex {
                    min: 16.0,
                    share: 2,
                },
            )
            .sortable(),
            ItemCol::Choice => Column::new("choice", "Choice", Width::Fixed(11.0))
                .sortable()
                .hint("What you choose to do with a conflict"),
            ItemCol::Outcome => plan::outcome_column(),
        }
    }

    fn key(&self) -> String {
        self.key.clone()
    }

    fn title(&self) -> String {
        [&self.game, &self.key]
            .into_iter()
            .find(|t| !t.is_empty())
            .cloned()
            .unwrap_or_default()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {}",
            self.game,
            self.current,
            self.proposed,
            self.kind.look().label
        )
    }

    fn cell(&self, col: ItemCol) -> AnyView {
        match col {
            ItemCol::Kind => view! { <StatusIcon status=self.kind tab_stop=false /> }.into_any(),
            ItemCol::Game => view! { <Clip text=self.game.clone() /> }.into_any(),
            ItemCol::Change => {
                view! { <Change current=self.current.clone() proposed=self.proposed.clone() /> }
                    .into_any()
            }
            ItemCol::Choice => self.choice(),
            ItemCol::Outcome => plan::outcome_cell(self.done),
        }
    }

    fn compare(&self, other: &Self, col: ItemCol) -> Ordering {
        match col {
            ItemCol::Kind => self.kind.rank().cmp(&other.kind.rank()),
            ItemCol::Game => self.game.cmp(&other.game),
            ItemCol::Change => self.current.cmp(&other.current),
            ItemCol::Choice => self
                .offers
                .is_empty()
                .cmp(&other.offers.is_empty())
                .then(self.chosen.label().cmp(other.chosen.label())),
            ItemCol::Outcome => plan::compare_outcome(self.done, other.done),
        }
    }
}

/// Why and how: the item's kind, what it is now and becomes, what else is known of it, and your choice.
fn item_view(row: ItemRow) -> AnyView {
    let look = row.kind.look();
    let choice = (!row.offers.is_empty()).then(|| {
        view! { <dt>"Choice"</dt> <dd>{row.choice()}</dd> }
    });
    let done = row.done.map(|o| {
        view! { <dt>"Outcome"</dt> <dd><StatusIcon status=o /> " " {o.look().label}</dd> }
    });
    view! {
        <dl class="facts">
            <dt>"Kind"</dt>
            <dd>{look.label} " — " {look.hint}</dd>
            <dt>"Now"</dt>
            <dd>
                <code class="path">{row.current.clone()}</code>
            </dd>
            <dt>"Proposed"</dt>
            <dd>
                <code class="path">{row.proposed.clone()}</code>
            </dd>
            {row
                .facts
                .iter()
                .map(|(label, text)| view! {
                    <dt>{*label}</dt>
                    <dd>{text.clone()}</dd>
                })
                .collect_view()}
            {choice}
            {done}
        </dl>
    }
    .into_any()
}

/// A build plan, in the plan shape.
#[component]
fn PlanView(plan: Plan, folder: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let selection = plan.content.selection;
    let facts = StoredValue::new(plan.clone());
    let approver = Approver::new(&plan, refresh);
    let planner = use_planner();
    let open = plan.approval.is_none();
    let approvable = plan.approvable();
    let choices = plan.content.choices.clone();
    let replanning = planner.pending(folder);
    let busy = Signal::derive(move || approver.busy.get() || replanning.get());
    let chooser = open.then(Chooser::<ConflictChoice>::new);
    let base: Arc<Vec<ItemRow>> = Arc::new(plan_rows(&plan.content, chooser));
    // The rows whose choice you may change, as the plan shown chose for them.
    let planned: Arc<Vec<(String, ConflictChoice)>> = Arc::new(
        base.iter()
            .filter(|r| !r.offers.is_empty())
            .map(|r| (r.key.clone(), r.chosen))
            .collect(),
    );
    let approval = {
        let planned = planned.clone();
        Signal::derive(move || Approval {
            changed: chooser.map_or(0, |c| c.changed(&planned)),
            blocked: !approvable,
        })
    };
    let plan_again = {
        let choices = choices.clone();
        let planned = planned.clone();
        chooser.map(|c| {
            Callback::new(move |()| {
                let picked: Vec<(String, ConflictChoice)> = c.picks.with_untracked(|p| {
                    planned
                        .iter()
                        .map(|(k, chosen)| (k.clone(), p.now(k, *chosen)))
                        .collect()
                });
                let next = with_conflict_choices(&choices, picked.iter().map(|(k, c)| (k, c)));
                planner.start(folder, Some(selection), next);
            })
        })
    };
    let build = use_build(folder, &plan);
    let rows = {
        let base = base.clone();
        Signal::derive(move || {
            let detail = build.get().flatten();
            let Some(detail) = detail else {
                return base.clone();
            };
            Arc::new(
                base.iter()
                    .map(|r| ItemRow {
                        done: detail
                            .items
                            .iter()
                            .find(|i| i.target == r.target)
                            .map(|i| build_model::item_outcome(i.state)),
                        ..r.clone()
                    })
                    .collect(),
            )
        })
    };
    let offered = offered_by_any(base.iter().map(|r| r.offers.as_slice()));
    let actions = match chooser {
        Some(chooser) => set_actions(
            [
                (Glyph::CircleOff, ConflictChoice::Leave),
                (Glyph::FolderCheck, ConflictChoice::Adopt),
                (Glyph::Archive, ConflictChoice::KeepAside),
            ],
            &offered,
            ConflictChoice::label,
            move |rows: Vec<ItemRow>, c| {
                chooser.set(
                    rows.iter()
                        .map(|r| (r.key.as_str(), r.chosen, r.offers.as_slice())),
                    c,
                    ConflictChoice::label,
                )
            },
        ),
        None => Vec::new(),
    };
    let apply = {
        let base = base.clone();
        Callback::new(move |kind: BuildKind| match chooser {
            Some(chooser) => {
                let of_kind: Vec<ItemRow> = base.iter().filter(|r| r.is(kind)).cloned().collect();
                let choices = offered_by_any(of_kind.iter().map(|r| r.offers.as_slice()));
                apply_to_all(kind, choices, ConflictChoice::label, move |c| {
                    chooser.set(
                        of_kind
                            .iter()
                            .map(|r| (r.key.as_str(), r.chosen, r.offers.as_slice())),
                        c,
                        ConflictChoice::label,
                    )
                })
            }
            None => ().into_any(),
        })
    };
    let unresolved = plan
        .content
        .blockers
        .iter()
        .any(|b| b.kind == BlockerKind::Unresolved);
    let resolved_only = choices.resolved_only;
    let partial = move || {
        (open && (unresolved || resolved_only)).then(|| {
            let choices = facts.with_value(|p| p.content.choices.clone());
            let on = !choices.resolved_only;
            let next = PlanChoices {
                resolved_only: on,
                ..choices.clone()
            };
            let label = if on {
                "Build resolved game groups only"
            } else {
                "Plan every game group again"
            };
            view! {
                <div class="form">
                    <button
                        type="button"
                        disabled=busy
                        on:click=move |_| planner.start(folder, Some(selection), next.clone())
                    >
                        {label}
                    </button>
                </div>
            }
        })
    };
    let label = approve_label(&plan.summary, Placing::of(&plan.content));
    let space = space_lines(&plan.summary, Placing::of(&plan.content));
    let changes_set =
        !plan.content.returns.is_empty() || plan.content.items.iter().any(|i| i.changes_source_set);
    let blockers = move || {
        let blockers = facts.with_value(|plan| plan.content.blockers.clone());
        (!blockers.is_empty()).then(|| {
        view! {
            <Message kind=MessageKind::NOTICE live=Live::Off>
                <p>"You can't approve this plan until these are fixed:"</p>
                <ul class="plain">
                    {blockers
                        .into_iter()
                        .map(|b| {
                            let link = b.group.as_ref().map(|g| {
                                view! { " " <A href=group_href(selection, g)>"Confirm a choice"</A> }
                            });
                            view! {
                                <li>
                                    <strong>{blocker_label(b.kind)}</strong>
                                    {format!(": {}", b.message)}
                                    {link}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            </Message>
        }
        })
    };
    let space = StoredValue::new(space);
    let single_copy = move || {
        (!space.with_value(Vec::is_empty) || changes_set).then(|| {
            view! {
                <section class="single-copy" aria-labelledby="single-copy-title">
                    <h3 id="single-copy-title">"Single copy"</h3>
                    {space.get_value().into_iter().map(|l| view! { <p>{l}</p> }).collect_view()}
                    {changes_set.then(|| view! { <p class="note">{CHANGES_SOURCE_SET}</p> })}
                </section>
            }
        })
    };
    let job = plan.job;
    // Approve sits above the list, with what it waits on; once approved, the plan's build job takes its place.
    let approve = {
        move || {
            view! {
                {open.then(|| view! {
                    {blockers()}
                    {single_copy()}
                    {partial()}
                    <ApproveBar
                        label=label.clone()
                        approval
                        busy
                        on_approve=Callback::new(move |()| approver.approve())
                        on_plan_again=plan_again.unwrap_or_else(|| Callback::new(|()| {}))
                    />
                })}
                {job.map(|job| view! { <JobOutcome job cancel=true /> })}
                {approver.problem()}
            }
        }
    };
    let help = move || {
        facts.with_value(|plan| {
            view! {
                <p>
                    "A build plan builds the folder from its selection. Choose what to do with a file in the way, for \
                    one row, the rows you check or every item of a kind, then plan again to review what it changes. \
                    Nothing changes until you approve."
                </p>
                {planned_from(plan)}
                {(!plan.content.notes.is_empty()).then(|| view! {
                    <h3>"Stored differently from the storage policy"</h3>
                    <ul class="plain">
                        {plan.content.notes.iter().map(|n| view! { <li>{n.clone()}</li> }).collect_view()}
                    </ul>
                })}
                {(!plan.changes.is_empty()).then(|| view! {
                    <h3>{format!("What changed since the current build ({})", plan.changes.len())}</h3>
                    <ul class="plain">
                        {plan
                            .changes
                            .iter()
                            .map(|c| {
                                let what = match (c.before, c.after) {
                                    (None, _) => "added",
                                    (_, None) => "gone",
                                    _ => "changed",
                                };
                                view! { <li>{format!("{}: {what}, by {}", c.title, cause_label(c.cause))}</li> }
                            })
                            .collect_view()}
                    </ul>
                })}
            }
        })
    };
    let summary = build_model::plan_line(&plan.content);
    let said = chooser.map_or_else(|| RwSignal::new(None), |c| c.said);

    view! {
        <section aria-labelledby="latest-plan-title">
            {heading(&plan, "Build plan")}
            <PlanReview
                id="plan-items"
                label="Plan items"
                rows
                empty="The plan has no files."
                summary
                placeholder="Filter by game, file, source or kind"
                detail=Callback::new(item_view)
                actions
                apply
                approve
                message=said
                help
            />
        </section>
    }
}

/// A cleanup of what the latest check of folder `folder` found, not yet planned: every candidate, left as it is, for
/// you to choose from and plan. `on_planned` runs once the cleanup is planned.
#[component]
pub fn CleanupDraft(
    folder: i64,
    refresh: RwSignal<u64>,
    #[prop(into)] busy: Signal<bool>,
    on_planned: Callback<()>,
) -> impl IntoView {
    view! { <CleanupLoad plan=None folder refresh on_planned busy /> }
}

/// A cleanup, planned as `plan` or not yet planned, loaded with what the folder holds that it can change: the latest
/// check and the kept files. `refresh` reloads them.
#[component]
fn CleanupLoad(
    plan: Option<Plan>,
    folder: i64,
    refresh: RwSignal<u64>,
    on_planned: Callback<()>,
    #[prop(optional, into)] busy: Signal<bool>,
) -> impl IntoView {
    let draft = plan.is_none();
    let live = use_live();
    // A plan is reloaded with the page; a draft lists what the folder holds now.
    let loaded = LocalResource::new(move || {
        if draft {
            live.settled.track();
            refresh.track();
        }
        async move {
            let check = api::get::<FolderCheck>(&format!("/libraries/{folder}/check"))
                .await
                .ok();
            let kept = api::get::<Vec<KeptFile>>(&format!("/libraries/{folder}/kept-files"))
                .await
                .unwrap_or_default();
            // An entry added as an addition is suggested the folder's system.
            let system = api::get::<LibraryFolder>(&format!("/libraries/{folder}"))
                .await
                .ok()
                .map(|f| f.system);
            let found = Found {
                drift: check.as_ref().map(|c| c.drift.clone()).unwrap_or_default(),
                files: check.map(|c| c.files).unwrap_or_default(),
                kept,
            };
            (found, system)
        }
    });
    let plan = StoredValue::new(plan);
    move || match loaded.get() {
        None => view! { <Loading what="the cleanup" /> }.into_any(),
        Some((found, system)) => view! {
            <CleanupView plan=plan.get_value() found system folder refresh on_planned busy />
        }
        .into_any(),
    }
}

/// One candidate of a cleanup plan.
#[derive(Clone, PartialEq)]
struct CleanupRow {
    item: CleanupItem,
    chooser: Option<Chooser<CleanupChoice>>,
    /// What an entry added as an addition is added as, where it may be.
    addition: Option<RwSignal<NewAddition>>,
    done: Option<ItemOutcome>,
}

impl CleanupRow {
    fn choice(&self) -> AnyView {
        let item = &self.item;
        match self.chooser {
            Some(c) => c.select(
                &item.key,
                item.name.clone(),
                item.offers.clone(),
                item.planned,
                CleanupChoice::label,
            ),
            None => item.planned.label().into_any(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupCol {
    Kind,
    Name,
    Game,
    Choice,
    Outcome,
}

impl PlanRow for CleanupRow {
    type Kind = CleanupKind;
    fn kind(&self) -> CleanupKind {
        self.item.kind
    }

    fn outcome(&self) -> Option<ItemOutcome> {
        self.done
    }
}

impl ListRow for CleanupRow {
    type Col = CleanupCol;
    const COLUMNS: &'static [CleanupCol] = &[
        CleanupCol::Kind,
        CleanupCol::Name,
        CleanupCol::Game,
        CleanupCol::Choice,
        CleanupCol::Outcome,
    ];

    fn column(col: CleanupCol) -> Column {
        match col {
            CleanupCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the latest check found it to be"),
            CleanupCol::Name => Column::new(
                "name",
                "Name",
                Width::Flex {
                    min: 14.0,
                    share: 2,
                },
            )
            .sortable(),
            CleanupCol::Game => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 10.0,
                    share: 1,
                },
            )
            .sortable(),
            CleanupCol::Choice => Column::new("choice", "Choice", Width::Fixed(11.0))
                .sortable()
                .hint("What you choose to do with it"),
            CleanupCol::Outcome => plan::outcome_column(),
        }
    }

    fn key(&self) -> String {
        self.item.key.clone()
    }

    fn title(&self) -> String {
        self.item.name.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {}",
            self.item.name,
            self.item.game,
            self.item.kind.look().label,
            self.item.planned.label()
        )
    }

    fn cell(&self, col: CleanupCol) -> AnyView {
        match col {
            CleanupCol::Kind => {
                view! { <StatusIcon status=self.item.kind tab_stop=false /> }.into_any()
            }
            CleanupCol::Name => view! { <Clip text=self.item.name.clone() /> }.into_any(),
            CleanupCol::Game => view! { <Clip text=self.item.game.clone() /> }.into_any(),
            CleanupCol::Choice => self.choice(),
            CleanupCol::Outcome => plan::outcome_cell(self.done),
        }
    }

    fn compare(&self, other: &Self, col: CleanupCol) -> Ordering {
        match col {
            CleanupCol::Kind => self.item.kind.rank().cmp(&other.item.kind.rank()),
            CleanupCol::Name => self.item.name.cmp(&other.item.name),
            CleanupCol::Game => self.item.game.cmp(&other.item.game),
            CleanupCol::Choice => self.item.planned.label().cmp(other.item.planned.label()),
            CleanupCol::Outcome => plan::compare_outcome(self.done, other.done),
        }
    }
}

/// What a cleanup candidate is, what the plan does with it, and your choice.
fn cleanup_detail(row: CleanupRow) -> AnyView {
    let item = row.item.clone();
    let look = item.kind.look();
    let now = row
        .chooser
        .map(|c| c.now(&item.key, item.planned))
        .unwrap_or_else(|| Signal::stored(item.planned));
    let place = match item.candidate {
        cleanup_model::Candidate::Kept(_) => format!("{RETENTION}/{}", item.name),
        _ => item.name.clone(),
    };
    view! {
        <dl class="facts">
            <dt>"Kind"</dt>
            <dd>{look.label} " — " {look.hint}</dd>
            <dt>"Now"</dt>
            <dd><code class="path">{place}</code></dd>
            {(!item.game.is_empty()).then(|| view! { <dt>"Game"</dt> <dd>{item.game.clone()}</dd> })}
            {item.target.clone().map(|t| view! {
                <dt>{if item.planned == CleanupChoice::Adopt { "Adopted as" } else { "Put back as" }}</dt>
                <dd><code class="path">{t}</code></dd>
            })}
            {item.detail.clone().map(|d| view! { <dt>"Found"</dt> <dd>{d}</dd> })}
            <dt>"Choice"</dt>
            <dd>{row.choice()}</dd>
            {row.done.map(|o| view! { <dt>"Outcome"</dt> <dd><StatusIcon status=o /> " " {o.look().label}</dd> })}
        </dl>
        {row.addition.filter(|_| row.chooser.is_some()).map(|value| view! {
            <Show when=move || now.get() == CleanupChoice::AddAddition>
                <p>"Add it as: " <AdditionFields value /></p>
            </Show>
        })}
    }
    .into_any()
}

/// A cleanup in the plan shape: every candidate, what the plan does with each, and your choices, which you plan again
/// from. With no `plan`, a draft: everything is left as it is until you choose and plan it.
#[component]
fn CleanupView(
    plan: Option<Plan>,
    found: Found,
    system: Option<String>,
    folder: i64,
    refresh: RwSignal<u64>,
    on_planned: Callback<()>,
    busy: Signal<bool>,
) -> impl IntoView {
    let cleanup = plan.as_ref().and_then(|p| p.content.cleanup.clone());
    if plan.is_some() && cleanup.is_none() {
        return ().into_any();
    }
    let draft = plan.is_none();
    let approver = plan.as_ref().map(|p| Approver::new(p, refresh));
    let open = plan.as_ref().is_none_or(|p| p.approval.is_none());
    let approvable = plan.as_ref().is_some_and(Plan::approvable);
    let chooser = open.then(Chooser::<CleanupChoice>::new);
    let items = cleanup_model::items(cleanup.as_ref(), &found);
    // What each entry that may be added as an addition is added as: as planned, or suggested.
    let additions: BTreeMap<String, (RwSignal<NewAddition>, Option<NewAddition>)> = items
        .iter()
        .filter(|i| i.offers.contains(&CleanupChoice::AddAddition))
        .filter_map(|i| match &i.candidate {
            cleanup_model::Candidate::Entry(name) => Some(name.clone()),
            _ => None,
        })
        .map(|name| {
            let shown = cleanup
                .as_ref()
                .and_then(|c| c.entries.iter().find(|e| e.name == name))
                .and_then(|e| e.addition.clone());
            let value = shown
                .clone()
                .unwrap_or_else(|| new_addition(&name, system.as_deref()));
            (name, (RwSignal::new(value), shown))
        })
        .collect();
    let additions = StoredValue::new(additions);
    let base: Arc<Vec<CleanupRow>> = Arc::new(
        items
            .iter()
            .map(|item| CleanupRow {
                addition: match &item.candidate {
                    cleanup_model::Candidate::Entry(name) => {
                        additions.with_value(|a| a.get(name).map(|(v, _)| *v))
                    }
                    _ => None,
                },
                item: item.clone(),
                chooser,
                done: None,
            })
            .collect(),
    );
    let items = Arc::new(items);
    let planned: Arc<Vec<(String, CleanupChoice)>> =
        Arc::new(items.iter().map(|i| (i.key.clone(), i.planned)).collect());
    // An addition's name or system you edit counts as a changed choice too.
    let edited = move || {
        additions.with_value(|a| {
            a.values()
                .filter(|(value, shown)| shown.as_ref().is_some_and(|s| value.with(|v| v != s)))
                .count()
        })
    };
    let changes = !items.iter().all(|i| i.planned == CleanupChoice::Leave);
    let approval = {
        let planned = planned.clone();
        Signal::derive(move || Approval {
            changed: chooser.map_or(0, |c| c.changed(&planned)) + edited(),
            blocked: !approvable || !changes,
        })
    };
    let planning = Action::new_local(move |body: &PlanCleanup| {
        let body = body.clone();
        async move { api::post::<_, Plan>(&format!("/libraries/{folder}/cleanups"), &body).await }
    });
    Effect::new(move || {
        if let Some(Ok(_)) = planning.value().get() {
            on_planned.run(());
        }
    });
    let plan_again = {
        let items = items.clone();
        chooser.map(|c| {
            Callback::new(move |()| {
                let added: BTreeMap<String, NewAddition> = additions.with_value(|a| {
                    a.iter()
                        .map(|(name, (value, _))| (name.clone(), value.get_untracked()))
                        .collect()
                });
                let body = c
                    .picks
                    .with_untracked(|p| cleanup_model::request(&items, p, &added));
                planning.dispatch(body);
            })
        })
    };
    let busy = Signal::derive(move || {
        busy.get() || approver.is_some_and(|a| a.busy.get()) || planning.pending().get()
    });
    let build = plan.as_ref().map(|p| use_build(folder, p));
    let rows = {
        let base = base.clone();
        Signal::derive(move || match build.and_then(|b| b.get().flatten()) {
            None => base.clone(),
            Some(detail) => Arc::new(
                base.iter()
                    .map(|r| CleanupRow {
                        done: cleanup_model::item_outcome(&r.item, &detail),
                        ..r.clone()
                    })
                    .collect(),
            ),
        })
    };
    let offered = offered_by_any(base.iter().map(|r| r.item.offers.as_slice()));
    let actions = match chooser {
        Some(chooser) => set_actions(
            [
                (Glyph::CircleOff, CleanupChoice::Leave),
                (Glyph::Archive, CleanupChoice::SetAside),
                (Glyph::FolderCheck, CleanupChoice::Adopt),
                (Glyph::Eye, CleanupChoice::Acknowledge),
                (Glyph::CirclePlus, CleanupChoice::AddAddition),
                (Glyph::Package, CleanupChoice::Repack),
                (Glyph::Undo2, CleanupChoice::PutBack),
            ],
            &offered,
            CleanupChoice::label,
            move |rows: Vec<CleanupRow>, c| {
                chooser.set(
                    rows.iter().map(|r| {
                        (
                            r.item.key.as_str(),
                            r.item.planned,
                            r.item.offers.as_slice(),
                        )
                    }),
                    c,
                    CleanupChoice::label,
                )
            },
        ),
        None => Vec::new(),
    };
    let apply = {
        let base = base.clone();
        Callback::new(move |kind: CleanupKind| match chooser {
            Some(chooser) => {
                let of_kind: Vec<CleanupItem> = base
                    .iter()
                    .filter(|r| r.item.kind == kind)
                    .map(|r| r.item.clone())
                    .collect();
                let choices = offered_by_any(of_kind.iter().map(|i| i.offers.as_slice()));
                apply_to_all(kind, choices, CleanupChoice::label, move |c| {
                    chooser.set(
                        of_kind
                            .iter()
                            .map(|i| (i.key.as_str(), i.planned, i.offers.as_slice())),
                        c,
                        CleanupChoice::label,
                    )
                })
            }
            None => ().into_any(),
        })
    };
    let job = plan.as_ref().and_then(|p| p.job);
    // A draft is planned once you choose something; a plan is approved as shown.
    let chosen = {
        let items = items.clone();
        Signal::derive(move || {
            chooser.is_some_and(|c| {
                c.picks.with(|p| {
                    items
                        .iter()
                        .any(|i| p.now(&i.key, i.planned) != CleanupChoice::Leave)
                })
            })
        })
    };
    let plan_it = plan_again.unwrap_or_else(|| Callback::new(|()| {}));
    let approve = move || {
        let problem = move || {
            planning
                .value()
                .get()
                .and_then(Result::err)
                .map(|message| view! { <Problem message /> })
        };
        if draft {
            return view! {
                <div class="actions plan-approve">
                    <button
                        type="button"
                        class:primary=chosen
                        disabled=move || busy.get() || !chosen.get()
                        on:click=move |_| plan_it.run(())
                    >
                        "Plan the cleanup"
                    </button>
                </div>
                <Show when=move || !chosen.get()>
                    <p class="note">"Choose what to do with at least one item to plan the cleanup."</p>
                </Show>
                {problem}
            }
            .into_any();
        }
        view! {
            {open.then(|| view! {
                {(!changes).then(|| view! {
                    <p class="note">
                        "Choose what to do with at least one item, then plan again: a cleanup that changes nothing \
                        can't be approved."
                    </p>
                })}
                <ApproveBar
                    label="Approve the cleanup".to_string()
                    approval
                    busy
                    on_approve=Callback::new(move |()| {
                        if let Some(a) = approver {
                            a.approve();
                        }
                    })
                    on_plan_again=plan_it
                />
                {problem}
            })}
            {job.map(|job| view! { <JobOutcome job cancel=true /> })}
            {approver.map(|a| a.problem())}
        }
        .into_any()
    };
    let facts = StoredValue::new(plan.clone());
    let help = move || {
        facts.with_value(|plan| {
            view! {
                <p>
                    {format!(
                        "A cleanup lists what the latest check found that Stage Select didn't build, the built files \
                         that aren't TorrentZip, and the folder's kept files. Set an entry aside, and it's moved, \
                         whole, into {RETENTION}; adopt a file that holds a selected game, and it's recorded as built \
                         under that game's file name; acknowledge an entry, and it's no longer reported until it \
                         changes; add a file no DAT knows as an addition, and it's set aside and copied into its \
                         system's additions set. Repack a built file, and it's rewritten as TorrentZip with the file as \
                         it was set aside; put a kept file back, and it returns to the folder."
                    )}
                </p>
                <p>
                    "Choose for one row, the rows you check or every item of a kind, then plan the cleanup, or plan \
                    it again, to review what it changes. Anything you leave stays as it is, and nothing changes until \
                    you approve. If anything in the folder changes before you approve, you're asked to plan again."
                </p>
                {plan.as_ref().map(planned_from)}
            }
        })
    };
    let summary = {
        let items = items.clone();
        Signal::derive(move || match chooser.filter(|_| draft) {
            // A draft's line counts what you chose so far.
            Some(c) => c.picks.with(|p| {
                let chosen: Vec<CleanupItem> = items
                    .iter()
                    .map(|i| CleanupItem {
                        planned: p.now(&i.key, i.planned),
                        ..i.clone()
                    })
                    .collect();
                cleanup_model::plan_line(&chosen)
            }),
            None => cleanup_model::plan_line(&items),
        })
    };
    let said = chooser.map_or_else(|| RwSignal::new(None), |c| c.said);
    let title = match &plan {
        Some(plan) => heading(plan, "Cleanup plan").into_any(),
        None => view! { <h2 id="cleanup-title">"Clean up the folder"</h2> }.into_any(),
    };
    view! {
        <section aria-labelledby=if draft { "cleanup-title" } else { "latest-plan-title" }>
            {title}
            <PlanReview
                id=if draft { "cleanup-items" } else { "plan-items" }
                label="Cleanup items"
                rows
                empty="The latest check found nothing to clean up: everything here was built by Stage Select, every built file is TorrentZip, and no files are set aside."
                summary
                placeholder="Filter by name, game, kind or choice"
                detail=Callback::new(cleanup_detail)
                actions
                apply
                approve
                message=said
                help
            />
        </section>
    }
    .into_any()
}
