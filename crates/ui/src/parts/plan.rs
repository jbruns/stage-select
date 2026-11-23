//! The review of a plan, whatever it plans, in one shape: a one-line summary in numbers; its items' counts by kind,
//! each filtering the list and carrying an "apply to all" choice; Approve, with "Plan again" beside it once you change a
//! choice; once applied, the counts by outcome, failures first; the `List` of rows reading kind, current → proposed,
//! choice and outcome; and the plan's notes in `Help`. A repair plan's kinds are `PlanKind`; a build plan's are
//! `BuildKind`. The pure logic is in `parts::plan_model`.

use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_query_map;
use wasm_bindgen::JsCast;

use super::help::Help;
use super::icon::{Glyph, Svg};
use super::legend::{LegendStrip, present_looks};
use super::list::{Clip, Column, Keep, List, ListRow, RowAction, Selected, ToolAction, Width};
use super::message::{Live, Message, MessageKind};
use super::plan_model::{
    Approval, PlanBlocker, kind_counts, kind_from_slug, kind_slug, outcome_line, with_active,
};
use super::status::{ItemOutcome, Status, StatusIcon};
use super::url_state::{scoped, use_query_edit};

/// A row of a plan review: a `ListRow` with the kind of item it is.
pub trait PlanRow: ListRow {
    type Kind: Status;
    fn kind(&self) -> Self::Kind;

    /// Another kind it counts and filters under besides its own: what else it does, such as setting a file aside.
    fn also(&self) -> Option<Self::Kind> {
        None
    }

    /// Whether it counts under `kind`.
    fn is(&self, kind: Self::Kind) -> bool {
        self.kind() == kind || self.also() == Some(kind)
    }

    /// What became of it once the plan was applied; nothing before.
    fn outcome(&self) -> Option<ItemOutcome> {
        None
    }
}

/// The Outcome column every plan's list ends with: empty until the plan is applied.
pub fn outcome_column() -> Column {
    Column::new("outcome", "Outcome", Width::Fixed(6.0))
        .sortable()
        .hint("What became of it once the plan was applied")
}

/// A row's Outcome cell.
pub fn outcome_cell(outcome: Option<ItemOutcome>) -> AnyView {
    outcome.map_or_else(
        || ().into_any(),
        |o| view! { <StatusIcon status=o tab_stop=false /> }.into_any(),
    )
}

/// The order of two rows by outcome, failures first and rows without one last.
pub fn compare_outcome(a: Option<ItemOutcome>, b: Option<ItemOutcome>) -> std::cmp::Ordering {
    let rank = |o: Option<ItemOutcome>| o.map_or(usize::MAX, |o| o.rank());
    rank(a).cmp(&rank(b))
}

/// The kind the URL's `kind` filters a plan review by, and a way to change it.
pub fn use_kind_filter<K: Status>() -> (Signal<Option<K>>, Callback<Option<K>>) {
    use_status_filter("kind")
}

/// The status the URL's `key` filters a list by, and a way to change it: for a strip of counts that filter.
pub fn use_status_filter<K: Status>(key: &'static str) -> (Signal<Option<K>>, Callback<Option<K>>) {
    use_status_filter_in(None, key)
}

/// As `use_status_filter`, for the list scoped by `scope`: its `{scope}-{key}`.
pub fn use_status_filter_in<K: Status>(
    scope: Option<&str>,
    key: &'static str,
) -> (Signal<Option<K>>, Callback<Option<K>>) {
    let [key, row, sel, all] = [key, "row", "sel", "all"].map(|k| scoped(scope, k));
    let query = use_query_map();
    // A memo, so the rest of the query changing (checking every matching row sets `all`) leaves the kind, and the
    // list's filter by it, as they are: a new filter would clear the matching rows just checked.
    let kind = Memo::new(move |_| query.with(|q| q.get(key).and_then(|s| kind_from_slug(&s))));
    let edit = use_query_edit();
    let set = Callback::new(move |k: Option<K>| {
        // Another kind shows other rows, so the open row and the checked rows go.
        edit.run(vec![
            (key, k.map(kind_slug)),
            (row, None),
            (sel, None),
            (all, None),
        ])
    });
    (kind.into(), set)
}

/// A cell reading `current` → `proposed`, each cut off where it doesn't fit.
#[component]
pub fn Change(#[prop(into)] current: String, #[prop(into)] proposed: String) -> impl IntoView {
    view! {
        <span class="change">
            <Clip text=current />
            <span class="change-to">
                <Svg glyph=Glyph::ArrowRight />
                <span class="visually-hidden">" becomes "</span>
            </span>
            <Clip text=proposed />
        </span>
    }
}

/// The "apply to all" control of `kind`: a `<select>` of `choices`, named by `label`, that starts at "no change" and
/// goes back to it once you pick one, since it sets a choice rather than holding one. Nothing when there's no choice.
pub fn apply_to_all<K: Status, C: Copy + Send + Sync + 'static>(
    kind: K,
    choices: Vec<C>,
    label: fn(C) -> &'static str,
    held: Signal<bool>,
    pick: impl Fn(C) + 'static,
) -> AnyView {
    if choices.is_empty() {
        return ().into_any();
    }
    let what = kind.look().label.to_lowercase();
    let options = choices
        .iter()
        .map(
            |c| view! { <option value=label(*c)>{format!("Apply to all: {}", label(*c))}</option> },
        )
        .collect_view();
    view! {
        <select
            class="apply-all"
            aria-label=format!("Set every {what} item to")
            title=format!("Set every {what} item to one choice")
            disabled=held
            on:change=move |ev| {
                let value = event_target_value(&ev);
                if held.get_untracked() {
                    // Frozen while the plan is approved or planned again.
                } else if let Some(c) = choices.iter().copied().find(|c| label(*c) == value) {
                    pick(c);
                }
                if let Some(el) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok()) {
                    el.set_value("");
                }
            }
        >
            <option value="">"Apply to all: No change"</option>
            {options}
        </select>
    }
    .into_any()
}

/// The toolbar's "Set to …" actions over the checked rows: one for each of `choices` that `offered` holds, running
/// `set` with the rows checked and the choice. They are disabled while `held`.
pub fn set_actions<R: ListRow, C: Copy + PartialEq + Send + Sync + 'static>(
    choices: impl IntoIterator<Item = (Glyph, C)>,
    offered: &[C],
    label: fn(C) -> &'static str,
    held: Signal<bool>,
    set: impl Fn(Vec<R>, C) + Copy + Send + Sync + 'static,
) -> Vec<ToolAction<R>> {
    let run = move |c: C| {
        move |chosen: Selected<R>| {
            if !held.get_untracked() {
                set(chosen.rows, c)
            }
        }
    };
    choices
        .into_iter()
        .filter(|(_, c)| offered.contains(c))
        .map(|(glyph, c)| {
            ToolAction::checked(glyph, format!("Set to {}", label(c)), run(c)).held_while(held)
        })
        .collect()
}

/// The counts of a plan's items by kind, or of a list's rows by status. Each count filters the list to its kind, or
/// back to every kind; the kind filtered on stays, at zero, once none are left. `apply` gives the control beside it that sets one choice for every item of the kind. `label`
/// names the strip, "Items by kind" unless given.
#[component]
pub fn KindCounts<K: Status>(
    #[prop(into)] counts: Signal<Vec<(K, usize)>>,
    #[prop(into)] active: Signal<Option<K>>,
    on_filter: Callback<Option<K>>,
    #[prop(optional)] apply: Option<Callback<K, AnyView>>,
    #[prop(optional)] label: Option<&'static str>,
) -> impl IntoView {
    view! {
        <ul class="kind-counts" aria-label=label.unwrap_or("Items by kind")>
            {move || {
                with_active(counts.get(), active.get())
                    .into_iter()
                    .map(|(kind, n)| {
                        let look = kind.look();
                        let on = move || active.get() == Some(kind);
                        let hint = format!("{} Show only these.", look.hint);
                        view! {
                            <li class="kind-count" class:active=on>
                                <button
                                    type="button"
                                    class="kind-filter"
                                    aria-pressed=move || on().to_string()
                                    title=hint
                                    on:click=move |_| {
                                        on_filter.run(if on() { None } else { Some(kind) })
                                    }
                                >
                                    <StatusIcon status=kind tab_stop=false />
                                    <span class="what">{look.label}</span>
                                    <span class="n">{n}</span>
                                </button>
                                {apply.map(|apply| apply.run(kind))}
                            </li>
                        }
                    })
                    .collect_view()
            }}
        </ul>
    }
}

/// The one way to change an item's choice, the same in every plan: a `<select>` of the choices it `offers`, named by
/// `label`, showing `now` and marked while it differs from `planned`, the choice the plan shown gives it. Used in its
/// row's Choice cell and in the open row's detail.
pub fn choice_select<C: Copy + PartialEq + Send + Sync + 'static>(
    label: String,
    offers: Vec<C>,
    planned: C,
    now: Signal<C>,
    name: fn(C) -> &'static str,
    held: Signal<bool>,
    pick: Callback<C>,
) -> AnyView {
    let options = offers
        .iter()
        .map(|c| view! { <option value=name(*c)>{name(*c)}</option> })
        .collect_view();
    view! {
        <select
            class="choice"
            class:changed=move || now.get() != planned
            aria-label=label
            prop:value=move || name(now.get())
            disabled=held
            on:change=move |ev| {
                let value = event_target_value(&ev);
                if held.get_untracked() {
                    // Frozen while the plan is approved or planned again: show the choice as it stands.
                    if let Some(el) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok()) {
                        el.set_value(name(now.get_untracked()));
                    }
                } else if let Some(c) = offers.iter().find(|c| name(**c) == value) {
                    pick.run(*c);
                }
            }
        >
            {options}
        </select>
    }
    .into_any()
}

/// Approve, labelled with what it does, and "Plan again" beside it once you change a choice: approving approves the
/// plan as shown, so it waits until you plan again. `busy` holds both while a request is under way.
#[component]
pub fn ApproveBar(
    #[prop(into)] label: Signal<String>,
    #[prop(into)] approval: Signal<Approval>,
    on_approve: Callback<()>,
    #[prop(optional)] on_plan_again: Option<Callback<()>>,
    #[prop(optional, into)] busy: Signal<bool>,
) -> impl IntoView {
    view! {
        <div class="actions plan-approve">
            <button
                type="button"
                class:primary=move || approval.get().may_approve()
                disabled=move || busy.get() || !approval.get().may_approve()
                on:click=move |_| on_approve.run(())
            >
                {label}
            </button>
            {move || {
                on_plan_again.filter(|_| approval.get().plan_again()).map(|again| view! {
                    <button
                        type="button"
                        class="primary"
                        disabled=busy
                        on:click=move |_| again.run(())
                    >
                        "Plan again"
                    </button>
                })
            }}
        </div>
        {move || approval.get().notice().map(|said| view! { <Message kind=MessageKind::NOTICE>{said}</Message> })}
    }
}

/// What blocks approving a plan, each with the item it is about and what to do: nothing when none does.
#[component]
pub fn Blockers(#[prop(into)] blockers: Signal<Vec<PlanBlocker>>) -> impl IntoView {
    move || {
        let blockers = blockers.get();
        (!blockers.is_empty()).then(|| {
            view! {
                <Message kind=MessageKind::NOTICE live=Live::Off>
                    <p>"You can't approve this plan until these are fixed:"</p>
                    <ul class="plain blockers">
                        {blockers
                            .into_iter()
                            .map(|b| {
                                view! {
                                    <li>
                                        <strong>{b.what}</strong>
                                        {b.item.map(|i| view! { " " <code class="path">{i}</code> })}
                                        {format!(": {}. {}", b.why.trim_end_matches('.'), b.fix)}
                                        {b.link.map(|(href, text)| view! { " " <A href>{text}</A> })}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                </Message>
            }
        })
    }
}

/// A plan's review, in the plan shape's order: `summary`, the one-line summary in numbers; its items' counts by kind,
/// filtering the list, each with its "apply to all" control from `apply`; `blockers`, what blocks approving, each with
/// its item and what to do; `approve`, Approve and "Plan again", and
/// what the last bulk choice changed in `message`; once any row has an outcome, the counts by outcome, failures
/// first, each filtering the list; the list, whose toolbar `actions` act on the checked rows or every row matching its
/// filter; and `help`, the plan's notes, in `Help`. Approve is never below the list.
#[component]
pub fn PlanReview<R: PlanRow>(
    id: &'static str,
    #[prop(into)] label: String,
    #[prop(into)] rows: Signal<Arc<Vec<R>>>,
    #[prop(into)] empty: String,
    detail: Callback<R, AnyView>,
    #[prop(into)] summary: Signal<String>,
    #[prop(optional)] actions: Vec<ToolAction<R>>,
    #[prop(optional)] row_actions: Option<Callback<R, Vec<RowAction>>>,
    /// The "apply to all" control of a kind.
    #[prop(optional)]
    apply: Option<Callback<R::Kind, AnyView>>,
    /// What blocks approving, each with its item and what to do: shown above Approve.
    #[prop(optional, into)]
    blockers: Signal<Vec<PlanBlocker>>,
    /// Approve and "Plan again", usually an `ApproveBar`: shown above the list.
    #[prop(optional, into)]
    approve: Option<ViewFn>,
    /// What the last bulk choice changed.
    #[prop(optional, into)]
    message: Signal<Option<String>>,
    #[prop(optional, into)] placeholder: Option<String>,
    /// For a review beside another list on its page: the prefix of its URL keys, `{scope}-kind` among them.
    #[prop(optional, into)]
    scope: MaybeProp<String>,
    /// The plan's notes: what it binds, what it writes besides, the storage policy it follows.
    #[prop(optional, into)]
    help: Option<ViewFn>,
) -> impl IntoView {
    let scope = scope.get_untracked();
    let (kind, set_kind) = use_status_filter_in::<R::Kind>(scope.as_deref(), "kind");
    let (outcome, set_outcome) = use_status_filter_in::<ItemOutcome>(scope.as_deref(), "outcome");
    let counts = Signal::derive(move || {
        rows.with(|r| {
            kind_counts(
                r.iter()
                    .flat_map(|r| std::iter::once(r.kind()).chain(r.also())),
            )
        })
    });
    let outcomes =
        Signal::derive(move || rows.with(|r| kind_counts(r.iter().filter_map(R::outcome))));
    let keep = Signal::derive(move || -> Keep<R> {
        let (k, o) = (kind.get(), outcome.get());
        (k.is_some() || o.is_some()).then(|| {
            let keep: Arc<dyn Fn(&R) -> bool + Send + Sync> = Arc::new(move |r: &R| {
                k.is_none_or(|k| r.is(k)) && o.is_none_or(|o| r.outcome() == Some(o))
            });
            keep
        })
    });
    let looks = Signal::derive(move || {
        rows.with(|r| {
            let mut looks = present_looks(r.iter().map(R::kind));
            looks.extend(present_looks(r.iter().filter_map(R::outcome)));
            looks
        })
    });
    let apply = apply.unwrap_or_else(|| Callback::new(|_| ().into_any()));
    let row_actions = row_actions.unwrap_or_else(|| Callback::new(|_| Vec::new()));
    let placeholder = placeholder.unwrap_or_else(|| "Filter the items".to_string());
    view! {
        <p class="plan-line">{summary}</p>
        <KindCounts counts active=kind on_filter=set_kind apply />
        <Blockers blockers />
        {approve.map(|a| a.run())}
        {move || message.get().map(|said| view! { <Message kind=MessageKind::DONE>{said}</Message> })}
        {move || {
            let counts = outcomes.get();
            (!counts.is_empty() || outcome.get().is_some()).then(|| view! {
                <p class="plan-line">{outcome_line(&counts)}</p>
                <KindCounts
                    counts
                    active=outcome
                    on_filter=set_outcome
                    label="Items by outcome"
                />
            })
        }}
        <List
            id
            label
            rows
            empty
            detail
            actions
            row_actions
            keep
            placeholder
            legend=move || view! { <LegendStrip looks list=id /> }
            scope
        />
        {help.map(|h| view! { <Help summary="About this plan">{h.run()}</Help> })}
    }
}
