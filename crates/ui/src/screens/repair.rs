//! A source set's Repair tab: the plan proposed from its latest audit, reviewed in bulk, and its approval and job;
//! and its Kept files tab. The plan review is `parts::plan::PlanReview`, the reference for reviewing any plan.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use leptos_router::params::ParamsMap;
use stage_select_api::SourceSetSummary;
use stage_select_api::additions::NewAddition;
use stage_select_api::build::ApprovePlan;
use stage_select_api::repair::{PlanRepair, RepairChoice, RepairItem, RepairPlan, RepairStep};
use stage_select_api::storage::{Storage, StorageFormat};

use crate::api;
use crate::events::use_live;
use crate::parts::help::Help;
use crate::parts::icon::Glyph;
use crate::parts::list::{Column, ListRow, Width};
use crate::parts::message::{Live, Message, MessageKind, Problem};
use crate::parts::outcome::JobOutcome;
use crate::parts::pending::Pending;
use crate::parts::plan::{self, ApproveBar, Change, PlanReview, PlanRow};
use crate::parts::plan_model::{Approval, PlanBlocker};
use crate::parts::status::{FindingStatus, ItemOutcome, PlanKind, Status, StatusCount, StatusIcon};
use crate::parts::url_state::use_query_edit;
use crate::repair_model::{
    ChangeMark, Found, LoadTicket, Loads, apply_to_keys, apply_to_kind, choices_for, chosen,
    could_change, current, fetch_after, item_outcome, kind_choices, may_propose, place, plan_line,
    proposed, repair_lead, repair_to_show, requested, steps_by_item, suggested_addition, to_add,
    unfinished_note, unplanned,
};
use crate::screens::additions::AdditionFields;
use crate::screens::source_sets::tab_href;
use crate::source_set_model::PageTab;

/// What the pending status says while a plan is made.
pub const MAKING_PLAN: &str = "Making the repair plan… a large source set can take a while.";

/// What the storage policy says for the plan's system, as the plan follows it.
pub fn storage_line(storage: &Storage) -> String {
    format!(
        "The storage policy says this system stores games {}{}{}, and it must still say so when you approve.",
        match storage.format {
            StorageFormat::TorrentZip => "as TorrentZip",
            StorageFormat::Loose => "loose where a game has one ROM",
            StorageFormat::LooseFolder => "as loose folders, a folder per game holding its ROMs",
            StorageFormat::Chd =>
                "as CHD, which a source set never holds, so a repair writes TorrentZip",
        },
        if storage.delete_originals {
            "; delete originals after conversion is on, so each original is deleted once its converted file is verified"
        } else {
            "; each original a conversion replaces goes to kept files"
        },
        match (storage.single_copy, storage.place_converted) {
            (false, _) => "",
            (true, false) =>
                "; single copy is on, so a build places each selected game as its only copy",
            (true, true) => {
                "; single copy and place converted games are on, so a build places each selected game as its only \
                 copy, CHD included"
            }
        }
    )
}

fn step_line(step: &RepairStep) -> (String, &'static str) {
    let class = match step {
        RepairStep::Keep { .. } | RepairStep::Delete { .. } => "removal",
        _ => "",
    };
    (step.describe(), class)
}

/// A plan made, with the choices it was asked for with.
type Proposed = (RepairPlan, BTreeMap<String, RepairChoice>);

/// The choices a plan was asked for with, and those you picked on it.
type Staged = (
    BTreeMap<String, RepairChoice>,
    BTreeMap<String, RepairChoice>,
);

/// A source set's repair, kept by its page so that a plan being made, and your picks, outlive a change of tab.
#[derive(Clone, Copy)]
pub struct RepairDesk {
    name: StoredValue<String>,
    /// What you picked on the plan shown, by item; approving approves the plan as it was made, so a pick it doesn't
    /// hold waits until you plan again with it.
    picked: RwSignal<BTreeMap<String, RepairChoice>>,
    /// The choices the plan shown was asked for with.
    asked: RwSignal<BTreeMap<String, RepairChoice>>,
    plan: RwSignal<Option<RepairPlan>>,
    problem: RwSignal<Option<String>>,
    working: RwSignal<bool>,
    /// The job the plan was last fetched for as it ended; kept here, as the progress shown is made again each time.
    fetched: RwSignal<Option<i64>>,
    /// What each item you choose to add as an addition is added as, by key.
    adding: RwSignal<BTreeMap<String, RwSignal<NewAddition>>>,
    /// The page's owner: the fields in `adding` outlive each showing of the plan.
    owner: StoredValue<Owner>,
    system: StoredValue<String>,
    /// What the last bulk choice changed.
    said: RwSignal<Option<String>>,
    /// The loads of the plan to show, so only the last one's answer is shown.
    loads: StoredValue<Loads>,
    /// The choices asked for and picked on each plan shown before, by plan, so coming back to one keeps them.
    staged: StoredValue<BTreeMap<i64, Staged>>,
    /// When the last proposal was asked for.
    proposed_from: StoredValue<Option<ChangeMark>>,
    /// Whether the Repair tab is open, so a proposal shown can name its plan in the URL.
    open_tab: StoredValue<bool>,
    /// A plan proposed while the Repair tab was closed, and the plan the URL named then: for the URL to name the
    /// proposal once the tab opens, unless you opened another plan meanwhile.
    unnamed: StoredValue<Option<(i64, Option<i64>)>>,
    edit_query: Callback<Vec<(&'static str, Option<String>)>>,
    query: Memo<ParamsMap>,
    /// The route's parameters, whose `name` is the source set whose page is open.
    route: Memo<ParamsMap>,
    proposal: Action<PlanRepair, Result<Proposed, String>>,
}

impl RepairDesk {
    /// The repair of the source set `name`, whose DAT is `system`.
    pub fn new(name: String, system: String) -> Self {
        let path = format!("/source-sets/{}/repairs", api::segment(&name));
        let proposal = Action::new_local(move |body: &PlanRepair| {
            let (path, body) = (path.clone(), body.clone());
            async move {
                let planned = api::post::<_, RepairPlan>(&path, &body).await;
                planned.map(|p| (p, body.choices))
            }
        });
        let edit_query = use_query_edit();
        let query = use_query_map();
        let route = use_params_map();
        let desk = RepairDesk {
            name: StoredValue::new(name),
            picked: RwSignal::new(BTreeMap::new()),
            asked: RwSignal::new(BTreeMap::new()),
            plan: RwSignal::new(None),
            problem: RwSignal::new(None),
            working: RwSignal::new(false),
            fetched: RwSignal::new(None),
            adding: RwSignal::new(BTreeMap::new()),
            owner: StoredValue::new(Owner::current().expect("the source set page has an owner")),
            system: StoredValue::new(system),
            said: RwSignal::new(None),
            loads: StoredValue::new(Loads::default()),
            staged: StoredValue::new(BTreeMap::new()),
            proposed_from: StoredValue::new(None),
            open_tab: StoredValue::new(false),
            unnamed: StoredValue::new(None),
            edit_query,
            query,
            route,
            proposal,
        };
        Effect::new(move || {
            let Some(answer) = proposal.value().get() else {
                return;
            };
            let Some(mark) = desk.proposed_from.get_value() else {
                return;
            };
            if !desk.change_applies(mark) {
                desk.reload();
            } else if let Ok((p, chosen)) = answer {
                let id = p.id;
                desk.loads.update_value(|l| l.accepted(Some(id)));
                desk.show(p, chosen);
                desk.name_in_url(id);
            }
        });
        desk
    }

    /// Keeps what was asked for and picked on the plan shown, for coming back to it.
    fn stash(self) {
        if let Some(id) = self.plan.with_untracked(|p| p.as_ref().map(|p| p.id)) {
            let staged = (self.asked.get_untracked(), self.picked.get_untracked());
            self.staged.update_value(|s| {
                s.insert(id, staged);
            });
        }
    }

    /// Shows `plan`, asked for with `asked`: with what you picked on it before, if you've shown it already.
    fn show(self, plan: RepairPlan, asked: BTreeMap<String, RepairChoice>) {
        self.stash();
        let (asked, picked) = self
            .staged
            .with_value(|s| s.get(&plan.id).cloned())
            .unwrap_or((asked, BTreeMap::new()));
        self.problem.set(None);
        self.said.set(None);
        self.picked.set(picked);
        self.asked.set(asked);
        self.plan.set(Some(plan));
    }

    /// Subscribes to the plan shown, so that what reads the source set's open repair reads it again.
    pub fn track(self) {
        self.plan.track();
    }

    /// Whether your choices are held as they are: while the plan is approved or planned again.
    fn held(self) -> Signal<bool> {
        Signal::derive(move || self.working.get() || self.proposal.pending().get())
    }

    /// Whether a plan is being made.
    pub fn proposing(self) -> Signal<bool> {
        self.proposal.pending().into()
    }

    /// Shows the plan the URL `named`, or else the source set's `open` repair, each time what's wanted changes. The
    /// plan already shown stays as it is, with your picks.
    pub fn load(self, named: Option<i64>, open: Option<i64>) {
        let shown = self
            .plan
            .with_untracked(|p| p.as_ref().map(|p| (p.id, p.approval.is_some())));
        if let Some(ticket) = self
            .loads
            .try_update_value(|l| l.ask(named, open, shown))
            .flatten()
        {
            self.fetch(ticket, false);
        }
    }

    /// Fetches the plan wanted again, as the server holds it, after the answer to a change was dropped.
    fn reload(self) {
        if let Some(ticket) = self.loads.try_update_value(|l| l.reload()).flatten() {
            self.fetch(ticket, true);
        }
    }

    /// Fetches the plan `ticket` wants and shows it, unless another load was asked for meanwhile. The plan shown is
    /// shown again only when `fresh`.
    fn fetch(self, ticket: LoadTicket, fresh: bool) {
        let before = self.shown_id();
        let wanted = ticket.wanted;
        let path = format!(
            "/source-sets/{}/repairs",
            api::segment(&self.name.get_value())
        );
        leptos::task::spawn_local(async move {
            let plans = api::get::<Vec<RepairPlan>>(&path).await;
            // Another load was asked for meanwhile: that one wins. Or the page was left, taking the desk with it:
            // nothing is shown. Past this check the desk is still there to read.
            if !self
                .loads
                .try_with_value(|l| l.applies(ticket))
                .unwrap_or(false)
            {
                return;
            }
            match plans {
                Ok(plans) => match repair_to_show(&plans, wanted) {
                    Some(p) if !fresh && Some(p.id) == before => {}
                    Some(p) => self.show(p.clone(), chosen(p)),
                    None => {
                        self.stash();
                        self.plan.set(None);
                    }
                },
                Err(message) => self.problem.set(Some(message)),
            }
        });
    }

    /// Names the plan `id`, just proposed, in the URL: now, if the Repair tab is open, or else once it opens. Another
    /// tab's URL is left as it is.
    fn name_in_url(self, id: i64) {
        if !self.on_its_page() {
            self.unnamed.set_value(None);
        } else if self.open_tab.get_value() {
            self.unnamed.set_value(None);
            self.edit_query.run(vec![("plan", Some(id.to_string()))]);
        } else {
            let named = self.query.with_untracked(named_plan);
            self.unnamed.set_value(Some((id, named)));
        }
    }

    fn shown_id(self) -> Option<i64> {
        self.plan.with_untracked(|p| p.as_ref().map(|p| p.id))
    }

    /// Marks a change made now to the plan shown.
    fn mark(self) -> ChangeMark {
        self.loads.with_value(Loads::mark)
    }

    /// Whether the answer to the change `mark`ed is still to be shown: not once the page was left.
    fn change_applies(self, mark: ChangeMark) -> bool {
        self.loads
            .try_with_value(|l| l.change_applies(mark))
            .unwrap_or(false)
    }

    /// Whether the route is this desk's source set's page, as it is unless you have just left it for another's.
    fn on_its_page(self) -> bool {
        self.route
            .try_with(|r| r.get("name").as_deref() == self.name.try_get_value().as_deref())
            .unwrap_or(false)
    }

    /// Proposes a plan; planning again from a plan still open carries its choices and additions.
    pub fn propose(self) {
        let open = self.plan.get_untracked().filter(|p| p.approval.is_none());
        let chosen = open
            .as_ref()
            .map(|p| requested(p, &self.asked.get_untracked(), &self.picked.get_untracked()))
            .unwrap_or_default();
        let additions = self.adding.with_untracked(|adding| {
            let adding = adding
                .iter()
                .map(|(k, v)| (k.clone(), v.get_untracked()))
                .collect();
            open.as_ref()
                .map(|p| to_add(p, &chosen, &adding, &self.system.get_value()))
                .unwrap_or_default()
        });
        self.proposed_from.set_value(Some(self.mark()));
        self.proposal.dispatch_local(PlanRepair {
            choices: chosen,
            additions,
        });
    }

    fn approve(self) {
        let Some(p) = self.plan.get_untracked() else {
            return;
        };
        self.working.set(true);
        let mark = self.mark();
        let body = ApprovePlan {
            digest: p.digest.clone(),
        };
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, RepairPlan>(&format!("/repairs/{}/approval", p.id), &body).await;
            self.working.set(false);
            // An approved plan holds no picks, shown or not.
            if done.is_ok() {
                self.staged.update_value(|s| {
                    s.remove(&p.id);
                });
            }
            // Another plan was asked for meanwhile: show the one wanted, as the server holds it.
            if !self.change_applies(mark) {
                self.reload();
                return;
            }
            match done {
                Ok(p) => {
                    self.loads.update_value(|l| l.accepted(None));
                    self.picked.set(BTreeMap::new());
                    self.asked.set(BTreeMap::new());
                    self.show(p, BTreeMap::new());
                }
                Err(message) => self.problem.set(Some(message)),
            }
        });
    }

    /// The fields of what `item` is added as, made the first time they're asked for.
    fn fields(self, item: &RepairItem) -> RwSignal<NewAddition> {
        if let Some(fields) = self.adding.with_untracked(|a| a.get(&item.key).copied()) {
            return fields;
        }
        let value = item
            .addition
            .clone()
            .unwrap_or_else(|| suggested_addition(item, &self.system.get_value()));
        let fields = self.owner.get_value().with(|| RwSignal::new(value));
        self.adding.update_untracked(|a| {
            a.insert(item.key.clone(), fields);
        });
        fields
    }

    /// How many items you changed since the plan shown was made.
    fn changed(self) -> usize {
        let adding = self.adding.with(|a| {
            a.iter()
                .map(|(k, v)| (k.clone(), v.get()))
                .collect::<BTreeMap<_, _>>()
        });
        self.plan.with(|p| {
            p.as_ref()
                .map_or(0, |p| self.picked.with(|c| unplanned(p, c, &adding).len()))
        })
    }
}

/// One item of the plan shown.
#[derive(Clone)]
struct RepairRow {
    item: RepairItem,
    place: String,
    proposed: String,
    open: bool,
    outcome: Option<String>,
    done: Option<ItemOutcome>,
    desk: RepairDesk,
}

impl PartialEq for RepairRow {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.proposed == other.proposed
            && self.open == other.open
            && self.outcome == other.outcome
            && self.done == other.done
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairCol {
    Kind,
    Change,
    Choice,
    Outcome,
}

impl PlanRow for RepairRow {
    type Kind = PlanKind;
    fn kind(&self) -> PlanKind {
        PlanKind::from(self.item.kind)
    }

    fn outcome(&self) -> Option<ItemOutcome> {
        self.done
    }
}

impl ListRow for RepairRow {
    type Col = RepairCol;
    const COLUMNS: &'static [RepairCol] = &[
        RepairCol::Kind,
        RepairCol::Change,
        RepairCol::Choice,
        RepairCol::Outcome,
    ];

    fn column(col: RepairCol) -> Column {
        match col {
            RepairCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the repair does to it"),
            RepairCol::Change => Column::new(
                "change",
                "Current → proposed",
                Width::Flex {
                    min: 16.0,
                    share: 1,
                },
            )
            .sortable(),
            RepairCol::Choice => Column::new("choice", "Choice", Width::Fixed(11.0))
                .sortable()
                .hint("What you choose to do with it"),
            RepairCol::Outcome => plan::outcome_column(),
        }
    }

    fn key(&self) -> String {
        self.item.key.clone()
    }

    fn title(&self) -> String {
        self.place.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.place,
            self.proposed,
            self.item.summary,
            self.kind().look().label,
            self.item.chosen.label()
        )
    }

    fn cell(&self, col: RepairCol) -> AnyView {
        match col {
            RepairCol::Kind => {
                view! { <StatusIcon status=self.kind() tab_stop=false /> }.into_any()
            }
            RepairCol::Change => {
                view! { <Change current=self.place.clone() proposed=self.proposed.clone() /> }
                    .into_any()
            }
            RepairCol::Choice if self.open => choice_select(&self.item, self.desk),
            RepairCol::Choice => self.item.chosen.label().into_any(),
            RepairCol::Outcome => plan::outcome_cell(self.done),
        }
    }

    fn compare(&self, other: &Self, col: RepairCol) -> Ordering {
        match col {
            RepairCol::Kind => self.kind().rank().cmp(&other.kind().rank()),
            RepairCol::Change => self.place.cmp(&other.place),
            RepairCol::Choice => self.item.chosen.label().cmp(other.item.chosen.label()),
            RepairCol::Outcome => plan::compare_outcome(self.done, other.done),
        }
    }
}

/// The choice of one item, as the plan's shared `<select>`.
fn choice_select(item: &RepairItem, desk: RepairDesk) -> AnyView {
    let key = StoredValue::new(item.key.clone());
    let planned = StoredValue::new(item.clone());
    let now = Signal::derive(move || desk.picked.with(|p| planned.with_value(|i| current(i, p))));
    plan::choice_select(
        format!("What to do with {}", place(item)),
        item.choices.clone(),
        item.chosen,
        now,
        RepairChoice::label,
        desk.held(),
        Callback::new(move |c| {
            desk.picked.update(|m| {
                m.insert(key.get_value(), c);
            })
        }),
    )
}

/// Why and how: the item's summary, the plan's note on it, its steps, what it is added as, and its outcome.
fn repair_detail(row: RepairRow, steps: Arc<Vec<(String, String, &'static str)>>) -> AnyView {
    let item = row.item.clone();
    let desk = row.desk;
    let kind = row.kind();
    let addable = (row.open && item.choices.contains(&RepairChoice::AddAddition)).then(|| {
        let fields = desk.fields(&item);
        let shows = {
            let item = item.clone();
            move || desk.picked.with(|p| current(&item, p)) == RepairChoice::AddAddition
        };
        view! {
            <Show when=shows>
                <h3>"Add it as"</h3>
                <AdditionFields value=fields held=desk.held() />
            </Show>
        }
    });
    let own: Vec<_> = steps
        .iter()
        .filter(|(k, _, _)| *k == item.key)
        .map(|(_, line, class)| view! { <li class=*class>{line.clone()}</li> })
        .collect();
    view! {
        <dl class="facts">
            <dt>"Kind"</dt>
            <dd>{kind.look().label} " — " {kind.look().hint}</dd>
            <dt>"Why"</dt>
            <dd>{item.summary.clone()}</dd>
            <dt>"Now"</dt>
            <dd>
                <code class="path">{row.place.clone()}</code>
            </dd>
            <dt>"Proposed"</dt>
            <dd>
                <code class="path">{row.proposed.clone()}</code>
            </dd>
            <dt>"Choice"</dt>
            <dd>
                {format!("{} (the plan proposed {})", item.chosen.label(), item.default.label())}
                {row.open.then(|| view! { " " {choice_select(&item, desk)} })}
            </dd>
            {item.game.clone().map(|g| view! {
                <dt>"Game"</dt>
                <dd>{g}</dd>
            })}
            {item.note.clone().map(|n| view! {
                <dt>"Note"</dt>
                <dd>{n}</dd>
            })}
            {row.outcome.clone().map(|o| view! {
                <dt>"Outcome"</dt>
                <dd>{o}</dd>
            })}
            {item.addition.clone().filter(|_| !row.open).map(|a| view! {
                <dt>"Added as"</dt>
                <dd>
                    {format!("The addition {} of {}", a.name, a.system)}
                    {a.parent.map(|p| format!(", a version of {p}"))}
                </dd>
            })}
        </dl>
        {addable}
        {(!own.is_empty()).then(|| view! {
            <h3>"Its changes"</h3>
            <ul class="plain">{own}</ul>
        })}
    }
    .into_any()
}

/// The "apply to all" control of a kind.
fn apply_to_all(desk: RepairDesk, kind: PlanKind) -> AnyView {
    let choices = desk
        .plan
        .with_untracked(|p| p.as_ref().map(|p| kind_choices(p, kind)))
        .unwrap_or_default();
    plan::apply_to_all(
        kind,
        choices,
        RepairChoice::label,
        desk.held(),
        move |choice| {
            let said = desk.plan.with_untracked(|p| {
                let p = p.as_ref()?;
                let mut picked = desk.picked.get_untracked();
                let outcome = apply_to_kind(p, kind, Some(choice), &mut picked)?;
                desk.picked.set(picked);
                Some(outcome.message(choice.label()))
            });
            if let Some(said) = said {
                desk.said.set(Some(said));
            }
        },
    )
}

fn outcome_text(o: &stage_select_api::repair::RepairOutcome) -> String {
    {
        let what = match (&o.error, o.done) {
            (Some(e), _) => format!("Not done: {e}"),
            (None, true) => "Done".to_string(),
            (None, false) => "Not done".to_string(),
        };
        let detail = o.detail.join("; ");
        if detail.is_empty() {
            what
        } else {
            format!("{what} ({detail})")
        }
    }
}

/// The Repair tab: a plan proposed from the source set's latest audit, the choice for each item, set in bulk or one
/// at a time, every change it makes, and its approval and job.
///
/// It shows the plan the URL `named`, even a finished one, or else `current`, the source set's open repair. A plan not
/// yet approved that isn't the open repair, shown because the URL names it, was replaced, so it is shown read-only.
#[component]
pub fn RepairTab(
    desk: RepairDesk,
    #[prop(into)] named: Signal<Option<i64>>,
    #[prop(into)] current: Signal<Option<i64>>,
    /// Whether the source set is running an approved repair, whichever plan is shown.
    #[prop(into)]
    running: Signal<bool>,
) -> impl IntoView {
    desk.open_tab.set_value(true);
    on_cleanup(move || desk.open_tab.set_value(false));
    Effect::new(move || {
        let (named, open) = (named.get(), current.get());
        // Another source set's page is opening in place of this one: nothing of this one's goes in its URL or is
        // loaded for it.
        if !desk.on_its_page() {
            desk.unnamed.set_value(None);
            return;
        }
        // A plan proposed while the tab was closed is shown already: the URL names it, rather than its old plan being
        // fetched over it.
        if let Some((id, then)) = desk.unnamed.get_value()
            && named == then
        {
            desk.unnamed.set_value(None);
            desk.edit_query.run(vec![("plan", Some(id.to_string()))]);
            return;
        }
        desk.unnamed.set_value(None);
        desk.load(named, open);
    });
    let wanted = move || named.get().or(current.get());
    let replaced = Memo::new(move |_| {
        desk.plan.with(|p| {
            p.as_ref().is_some_and(|p| {
                p.approval.is_none() && wanted() == Some(p.id) && current.get() != Some(p.id)
            })
        })
    });
    let proposal = desk.proposal;
    let problem = Signal::derive(move || {
        proposal
            .value()
            .with(|v| v.as_ref().and_then(|r| r.as_ref().err().cloned()))
    });
    let none_yet = Signal::derive(move || {
        named.get().is_none() && current.get().is_none() && desk.plan.with(Option::is_none)
    });
    view! {
        <section aria-label="Repair">
            {move || none_yet.get().then(|| view! { <WhatARepairCouldChange name=desk.name.get_value() /> })}
            {move || {
                let shown = desk
                    .plan
                    .with(|p| p.as_ref().map(|p| (p.approval.is_some(), p.finished_at.is_some())));
                may_propose(shown, running.get()).then(|| view! {
                    <div class="actions">
                        <button
                            type="button"
                            class="primary"
                            disabled=move || desk.working.get() || proposal.pending().get()
                            on:click=move |_| desk.propose()
                        >
                            "Propose a repair"
                        </button>
                    </div>
                })
            }}
            <Pending pending=proposal.pending() status=MAKING_PLAN problem>
                {move || {
                    let replaced = replaced.get();
                    desk.plan.get().map(|p| view! { <PlanShown desk plan=p replaced /> })
                }}
            </Pending>
            {move || desk.problem.get().map(|message| view! { <Problem message /> })}
            {move || none_yet.get().then(|| view! {
                <Help summary="About repairs">
                    <p>
                        "A repair is proposed from the source set's latest audit and from items in your ingest \
                        folders: it renames misnamed entries, replaces mismatched or unreadable ones and fills \
                        missing games from your ingest folders, repacks what isn't stored as your storage policy \
                        says, and sets aside what no game needs. You review every change, and nothing changes until \
                        you approve. Nothing is deleted: what a repair replaces or sets aside goes to kept files."
                    </p>
                </Help>
            })}
        </section>
    }
}

/// What the source set's latest audit found that a repair could change, each count opening what it counts, led by a
/// line saying so, or why there's nothing to count.
#[component]
fn WhatARepairCouldChange(name: String) -> impl IntoView {
    let live = use_live();
    let wanted = StoredValue::new(name.clone());
    let latest = LocalResource::new(move || {
        live.settled.track();
        let name = wanted.get_value();
        async move {
            let sets: Vec<SourceSetSummary> = api::get("/source-set-summaries").await?;
            Ok::<_, String>(
                sets.into_iter()
                    .find(|s| s.set.name == name)
                    .and_then(|s| s.latest),
            )
        }
    });
    move || {
        let latest = latest.get()?.ok()?;
        let could = latest
            .as_ref()
            .map(|a| could_change(&a.findings, &a.games))
            .unwrap_or_default();
        let lead = repair_lead(latest.is_some(), &could);
        let tiles = could
            .into_iter()
            .map(|c| {
                let tab = match c.found {
                    Found::Entries(_) => PageTab::Entries,
                    Found::Games(_) => PageTab::Games,
                };
                let (key, value) = c.filter();
                let href = format!("{}&{key}={value}", tab_href(&name, tab));
                let n = view! { <a href=href>{c.n}</a> };
                match c.found {
                    Found::Entries(f) => view! {
                        <StatusCount status=FindingStatus::from(f) what=c.what()>{n}</StatusCount>
                    }
                    .into_any(),
                    Found::Games(s) => {
                        view! { <StatusCount status=s what=c.what()>{n}</StatusCount> }.into_any()
                    }
                }
            })
            .collect_view();
        Some(view! {
            <p>{lead}</p>
            <ul class="counts">{tiles}</ul>
        })
    }
}

#[component]
fn PlanShown(desk: RepairDesk, plan: RepairPlan, replaced: bool) -> impl IntoView {
    let p = plan;
    let open = p.approval.is_none() && !replaced;
    let steps: Arc<Vec<(String, String, &'static str)>> = Arc::new(
        p.content
            .steps
            .iter()
            .map(|s| {
                let (line, class) = step_line(&s.step);
                (s.item.clone(), line, class)
            })
            .collect(),
    );
    let by_item = steps_by_item(&p);
    let outcomes: HashMap<&str, _> = p.outcomes.iter().map(|o| (o.item.as_str(), o)).collect();
    let rows: Arc<Vec<RepairRow>> = Arc::new(
        p.content
            .items
            .iter()
            .map(|item| RepairRow {
                place: place(item),
                proposed: proposed(
                    item,
                    by_item
                        .get(item.key.as_str())
                        .map_or(&[][..], Vec::as_slice),
                ),
                open,
                outcome: outcomes.get(item.key.as_str()).map(|o| outcome_text(o)),
                done: item_outcome(&p, item),
                item: item.clone(),
                desk,
            })
            .collect(),
    );
    drop(by_item);
    drop(outcomes);
    let removals = p
        .content
        .steps
        .iter()
        .filter(|s| matches!(s.step, RepairStep::Keep { .. }))
        .count();
    let count = p.content.steps.len();
    let changed = move || desk.changed();
    let busy = desk.held();
    // A newer audit, or another repair approved, leaves a plan stale that this screen can't tell from a fresh one,
    // and an empty one only changes once planned again: an open repair always offers planning again.
    let approval = Signal::derive(move || Approval {
        changed: changed(),
        blocked: count == 0,
        stale: true,
    });
    let summary = plan_line(&p);
    // Only the choices some item offers are offered in bulk.
    let all_keys: Vec<String> = p.content.items.iter().map(|i| i.key.clone()).collect();
    let offered = if open {
        choices_for(&p, &all_keys)
    } else {
        Vec::new()
    };
    let set = move |rows: Vec<RepairRow>, choice: RepairChoice| {
        let keys: Vec<String> = rows.into_iter().map(|r| r.item.key).collect();
        let said = desk.plan.with_untracked(|p| {
            let p = p.as_ref()?;
            let mut picked = desk.picked.get_untracked();
            let outcome = apply_to_keys(p, &keys, choice, &mut picked);
            desk.picked.set(picked);
            Some(outcome.message(choice.label()))
        });
        desk.said.set(said);
    };
    let actions = plan::set_actions(
        [
            (Glyph::Check, RepairChoice::Apply),
            (Glyph::CircleOff, RepairChoice::Leave),
            (Glyph::Archive, RepairChoice::KeepAside),
        ],
        &offered,
        RepairChoice::label,
        busy,
        set,
    );
    let detail_steps = steps.clone();
    let audit_href = format!("/audits/{}", p.content.audit);
    let notes = p.content.notes.clone();
    let replaced_note = replaced.then(|| {
        view! {
            <Message kind=MessageKind::NOTICE live=Live::Off>
                "A newer plan replaced this one, so it is shown read-only. "
                <A href=tab_href(&desk.name.get_value(), PageTab::Repair)>"Show the source set's repair"</A>
            </Message>
        }
    });
    let audit = p.content.audit;
    let binding = p.content.fingerprint.len();
    let marker = p.content.creates_marker;
    let storage = storage_line(&p.content.storage);
    let help = move || {
        view! {
            <p>
                "A repair is proposed from the source set's latest audit and from items in ingest folders. Set a \
                choice for every item of a kind, for the rows you check, or one row at a time, then plan again to \
                review what it changes. Nothing changes until you approve, and nothing is deleted: what a repair \
                replaces or sets aside goes to kept files."
            </p>
            <p>
                "It's planned from " <A href=audit_href.clone()>{format!("audit #{audit}")}</A>
                {format!(
                    ". It binds the {binding} entries of the source set as they are now: anything changing there, \
                     or another audit, before you approve means planning again."
                )}
                {marker.then_some(
                    " It's this source set's first repair, so it writes the source set's marker, .stage-select/marker.",
                )}
            </p>
            <p>{storage.clone()}</p>
            <details>
                <summary>{format!("Every change, in order ({count}; {removals} set aside)")}</summary>
                <ul class="plain">
                    {steps
                        .iter()
                        .map(|(_, line, class)| view! { <li class=*class>{line.clone()}</li> })
                        .collect_view()}
                </ul>
            </details>
            {(!notes.is_empty()).then(|| view! {
                <details>
                    <summary>{format!("Left as it is ({})", notes.len())}</summary>
                    <ul class="plain">
                        {notes.iter().map(|n| view! { <li>{n.clone()}</li> }).collect_view()}
                    </ul>
                </details>
            })}
        }
    };
    let job = p.job;
    let unfinished = unfinished_note(&p);
    let recovery_error = p.recovery_error.clone();
    let audit_job = p.audit_job;
    let audit_owed = p.audit_owed;
    let plan_id = p.id;
    // Approve sits above the list; once approved, the repair's job and what it queued take its place.
    let approve = move || {
        if open {
            view! {
                <ApproveBar
                    label=format!("Approve and make {count} changes")
                    approval
                    busy
                    on_approve=Callback::new(move |()| desk.approve())
                    on_plan_again=Callback::new(move |()| desk.propose())
                />
            }
            .into_any()
        } else {
            view! {
                {job.map(|job| view! { <RepairProgress job plan=plan_id desk /> })}
                {match (unfinished.clone(), recovery_error.clone()) {
                    (Some(note), _) => Some(view! { <Message kind=MessageKind::NOTICE live=Live::Off>{note}</Message> }.into_any()),
                    (None, e) => e.map(|e| view! { <Problem message=e live=Live::Off /> }.into_any()),
                }}
                {audit_job.map(|job| view! { <JobOutcome job /> })}
                {audit_owed.then(|| view! {
                    <p class="note">"An incremental audit is queued once the job running for the source set ends."</p>
                })}
            }
            .into_any()
        }
    };
    view! {
        <h2>{format!("Repair plan #{}", p.id)}</h2>
        {replaced_note}
        {if p.content.items.is_empty() {
            view! {
                {approve()}
                <p class="note">"Nothing to repair."</p>
                <Help summary="About this plan">{help()}</Help>
            }
                .into_any()
        } else {
            view! {
                <PlanReview
                    id="repair-items"
                    label="Repair items"
                    rows=Signal::derive(move || rows.clone())
                    empty="Nothing to repair."
                    summary
                    placeholder="Filter by file, game, kind or choice"
                    detail=Callback::new(move |row: RepairRow| repair_detail(row, detail_steps.clone()))
                    actions
                    apply=Callback::new(move |kind: PlanKind| if open { apply_to_all(desk, kind) } else { ().into_any() })
                    blockers=Signal::stored(
                        (open && count == 0).then(PlanBlocker::nothing_to_change).into_iter().collect::<Vec<_>>(),
                    )
                    approve=approve
                    message=desk.said
                    help=help
                />
            }
                .into_any()
        }}
    }
}

/// The repair's job, live: its outcome, a way to cancel it while it runs, and the plan fetched again with its
/// outcomes once it ends.
#[component]
fn RepairProgress(job: i64, plan: i64, desk: RepairDesk) -> impl IntoView {
    let (shown, fetched) = (desk.plan, desk.fetched);
    let live = use_live();
    let problem = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let ended = live
            .book
            .with(|b| b.get(job).is_some_and(|l| l.job.state.is_final()));
        let unfinished =
            shown.with_untracked(|p| p.as_ref().is_some_and(|p| p.finished_at.is_none()));
        if fetch_after(job, ended, unfinished, fetched.get_untracked()) {
            fetched.set(Some(job));
            let mark = desk.mark();
            leptos::task::spawn_local(async move {
                let done = api::get::<RepairPlan>(&format!("/repairs/{plan}")).await;
                // Another plan was asked for meanwhile, or the page was left: show the one wanted, and fetch this one again when its job's
                // end is next seen.
                if !desk.change_applies(mark) {
                    fetched.set(None);
                    desk.reload();
                    return;
                }
                match done {
                    Ok(p) if p.id == plan => {
                        desk.loads.update_value(|l| l.accepted(None));
                        shown.set(Some(p));
                    }
                    Ok(_) => {}
                    Err(message) => problem.set(Some(message)),
                }
            });
        }
    });
    view! {
        <JobOutcome job cancel=true />
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// The plan the URL's `plan` names.
fn named_plan(query: &ParamsMap) -> Option<i64> {
    query.get("plan").and_then(|p| p.parse().ok())
}

/// A repair plan of `plan`, open for review, in the Repair tab's review: for the filtering benchmark.
#[cfg(feature = "bench")]
pub(crate) fn bench_plan(plan: RepairPlan) -> impl IntoView {
    let desk = RepairDesk::new(plan.content.source_set.clone(), "Bench".to_string());
    view! { <PlanShown desk plan replaced=false /> }
}
