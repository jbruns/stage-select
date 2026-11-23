//! The review of a plan, whatever it plans: a strip of its items' counts by kind, each filtering the list and carrying
//! an "apply to all" choice, over a `List` of rows reading current → proposed with the choice at the end, and what one
//! bulk choice changed. A repair plan's kinds are `PlanKind`; a build plan's are `BuildKind`. The pure logic is in
//! `parts::plan_model`.

use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::hooks::use_query_map;
use wasm_bindgen::JsCast;

use super::icon::{Glyph, Svg};
use super::legend::{Legend, present};
use super::list::{Clip, Keep, List, ListRow, RowAction, Selected, ToolAction};
use super::message::{Message, MessageKind};
use super::plan_model::{kind_counts, kind_from_slug, kind_slug, with_active};
use super::status::{Status, StatusIcon};
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
            on:change=move |ev| {
                let value = event_target_value(&ev);
                if let Some(c) = choices.iter().copied().find(|c| label(*c) == value) {
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
/// `set` with the rows checked and the choice.
pub fn set_actions<R: ListRow, C: Copy + PartialEq + Send + Sync + 'static>(
    choices: impl IntoIterator<Item = (Glyph, C)>,
    offered: &[C],
    label: fn(C) -> &'static str,
    set: impl Fn(Vec<R>, C) + Copy + Send + Sync + 'static,
) -> Vec<ToolAction<R>> {
    let run = move |c: C| move |chosen: Selected<R>| set(chosen.rows, c);
    choices
        .into_iter()
        .filter(|(_, c)| offered.contains(c))
        .map(|(glyph, c)| ToolAction::checked(glyph, format!("Set to {}", label(c)), run(c)))
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

/// A plan's items: their counts by kind, filtering the list below, and what the last bulk choice changed in
/// `message`. The list's own toolbar `actions` act on the checked rows, or on every row matching its filter.
#[component]
pub fn PlanReview<R: PlanRow>(
    id: &'static str,
    #[prop(into)] label: String,
    #[prop(into)] rows: Signal<Arc<Vec<R>>>,
    #[prop(into)] empty: String,
    detail: Callback<R, AnyView>,
    #[prop(optional)] actions: Vec<ToolAction<R>>,
    #[prop(optional)] row_actions: Option<Callback<R, Vec<RowAction>>>,
    /// The "apply to all" control of a kind.
    #[prop(optional)]
    apply: Option<Callback<R::Kind, AnyView>>,
    /// What the last bulk choice changed.
    #[prop(optional, into)]
    message: Signal<Option<String>>,
    #[prop(optional, into)] placeholder: Option<String>,
    /// For a review beside another list on its page: the prefix of its URL keys, `{scope}-kind` among them.
    #[prop(optional, into)]
    scope: MaybeProp<String>,
) -> impl IntoView {
    let scope = scope.get_untracked();
    let (kind, set_kind) = use_status_filter_in::<R::Kind>(scope.as_deref(), "kind");
    let counts = Signal::derive(move || {
        rows.with(|r| {
            kind_counts(
                r.iter()
                    .flat_map(|r| std::iter::once(r.kind()).chain(r.also())),
            )
        })
    });
    let keep = Signal::derive(move || -> Keep<R> {
        kind.get().map(|k| {
            let keep: Arc<dyn Fn(&R) -> bool + Send + Sync> = Arc::new(move |r: &R| r.is(k));
            keep
        })
    });
    let statuses = Signal::derive(move || rows.with(|r| present(r.iter().map(R::kind))));
    let apply = apply.unwrap_or_else(|| Callback::new(|_| ().into_any()));
    let row_actions = row_actions.unwrap_or_else(|| Callback::new(|_| Vec::new()));
    let placeholder = placeholder.unwrap_or_else(|| "Filter the items".to_string());
    view! {
        <KindCounts counts active=kind on_filter=set_kind apply />
        {move || message.get().map(|said| view! { <Message kind=MessageKind::DONE>{said}</Message> })}
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
            legend=move || view! { <Legend statuses list=id /> }
            scope
        />
    }
}
