//! A source set's coverage: the editor shared by registering a source set and editing its coverage, the drawer that
//! edits it with a preview of what saving changes, and its games counted by region.

use leptos::prelude::*;
use leptos_router::components::A;
use title_select_api::coverage::PRESETS;
use title_select_api::{
    CoveragePreview, GameStatus, RegionCounts, SetCoverage, SourceSetRecord, Vocabulary,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::coverage_model::{self as model, Draft, Tick};
use crate::parts::icon::Glyph;
use crate::parts::overlay::Drawer;
use crate::parts::pending::PendingStatus;
use crate::parts::status::{Status, StatusIcon};

/// The icon for a source set's coverage and its name: every region, only some, or a coverage that couldn't be applied.
pub fn glyph(coverage: Option<&[String]>, unavailable: bool) -> (Glyph, &'static str) {
    match (coverage, unavailable) {
        (Some(_), true) => (Glyph::MapPinX, "Coverage not applied"),
        (Some(_), false) => (Glyph::MapPinned, "Only some regions"),
        (None, _) => (Glyph::Globe, "Every region"),
    }
}

/// What a coverage that couldn't be applied means for the counts shown.
pub const UNAVAILABLE: &str = "Your coverage couldn't be applied, as the games' regions can't be read now: these \
    counts are every game of the DAT.";

/// The coverage being chosen in `draft`: every region, or only some, chosen by preset or region by region. It loads
/// the engine's region names into `order`, the order a saved coverage keeps.
#[component]
pub fn CoverageEditor(draft: RwSignal<Draft>, order: RwSignal<Vec<String>>) -> impl IntoView {
    let vocabulary = LocalResource::new(|| api::get::<Vocabulary>("/vocabulary"));
    Effect::new(move || {
        if let Some(Ok(v)) = vocabulary.get() {
            order.set(v.regions);
        }
    });
    let only = move || draft.with(|d| !d.every);
    let presets = move || {
        PRESETS
            .iter()
            .map(|preset| {
                let tick = move || draft.with(|d| d.tick(preset));
                view! {
                    <li>
                        <label class="inline">
                            <input
                                type="checkbox"
                                prop:checked=move || tick() == Tick::All
                                prop:indeterminate=move || tick() == Tick::Some
                                on:change=move |ev| {
                                    let on = event_target_checked(&ev);
                                    draft.update(|d| d.set_preset(preset, on));
                                }
                            />
                            {preset.name}
                        </label>
                    </li>
                }
            })
            .collect_view()
    };
    let regions = move || {
        model::groups(&order.get())
            .into_iter()
            .map(|group| {
                let boxes = group
                    .regions
                    .into_iter()
                    .map(|region| {
                        let shown = model::region_name(&region).to_owned();
                        let region = StoredValue::new(region);
                        view! {
                            <li>
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        prop:checked=move || {
                                            draft.with(|d| region.with_value(|r| d.regions.contains(r)))
                                        }
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            draft.update(|d| region.with_value(|r| d.set_region(r, on)));
                                        }
                                    />
                                    {shown}
                                </label>
                            </li>
                        }
                    })
                    .collect_view();
                view! {
                    <fieldset class="setting">
                        <legend>{group.name}</legend>
                        <ul class="checks columns">{boxes}</ul>
                    </fieldset>
                }
            })
            .collect_view()
    };
    view! {
        <fieldset class="setting coverage-editor">
            <legend>"Coverage"</legend>
            <p class="note small">
                "The regions whose games you want to keep. A game outside your coverage never counts as missing, \
                and nothing fetches or lists it; one you have is kept and still counted."
            </p>
            <ul class="checks">
                <li>
                    <label class="inline">
                        <input
                            type="radio"
                            name="coverage-every"
                            prop:checked=move || !only()
                            on:change=move |_| draft.update(|d| d.every = true)
                        />
                        "Every region"
                    </label>
                </li>
                <li>
                    <label class="inline">
                        <input
                            type="radio"
                            name="coverage-every"
                            prop:checked=only
                            on:change=move |_| draft.update(|d| d.every = false)
                        />
                        "Only some regions"
                    </label>
                </li>
            </ul>
            <Show when=only>
                <ul class="checks" aria-label="Presets">
                    {presets()}
                </ul>
                <details class="choices">
                    <summary>"Choose regions"</summary>
                    {move || match vocabulary.get() {
                        None => view! { <Loading what="the regions" /> }.into_any(),
                        Some(Err(problem)) => {
                            let message = format!("Couldn't load the regions to choose from: {problem}");
                            view! { <Problem message /> }.into_any()
                        }
                        Some(Ok(_)) => regions().into_any(),
                    }}
                </details>
                <p class="note small">
                    "World games are always covered. A game with no region tag is covered only when you choose \
                    Unknown, which no preset includes."
                </p>
            </Show>
            <p aria-live="polite">
                <strong>{move || draft.with(Draft::label)}</strong>
            </p>
        </fieldset>
    }
}

/// The drawer that edits the coverage of the source set `name`, open while `open`. `coverage` is the stored one once
/// the source set has loaded (`None` until then): the editor waits for it, and starts from it as it was when it
/// first arrived, so a reload while you edit doesn't undo your choices. It previews what saving would change, and
/// bumps `refresh` once saved.
#[component]
pub fn CoverageDrawer(
    name: String,
    open: RwSignal<bool>,
    #[prop(into)] coverage: Signal<Option<Option<Vec<String>>>>,
    refresh: RwSignal<u64>,
    said: RwSignal<Option<String>>,
) -> impl IntoView {
    let name = StoredValue::new(name);
    // The coverage just saved, until the reload of the source set brings it: reopening the drawer sooner starts from
    // it, not from the coverage it replaced.
    let saved = RwSignal::new(None::<Option<Vec<String>>>);
    // Tracks both: a reload that brings back the same coverage notifies nothing, so saving one unchanged must clear
    // it as soon as it is set.
    Effect::new(move || {
        let loaded = coverage.get();
        if saved.with(|s| s.is_some() && !model::save_pending(s.as_ref(), loaded.as_ref())) {
            saved.set(None);
        }
    });
    view! {
        <Drawer open title="Edit coverage">
            {
                let start = Memo::new(move |first: Option<&Option<Option<Vec<String>>>>| {
                    first
                        .cloned()
                        .flatten()
                        .or_else(|| saved.get_untracked())
                        .or_else(|| coverage.get())
                });
                move || match start.get() {
                    None => view! { <PendingStatus status="Loading its coverage…" /> }.into_any(),
                    Some(coverage) => {
                        view! { <EditCoverage name=name.get_value() open coverage refresh said saved /> }.into_any()
                    }
                }
            }
        </Drawer>
    }
}

#[component]
fn EditCoverage(
    name: String,
    open: RwSignal<bool>,
    coverage: Option<Vec<String>>,
    refresh: RwSignal<u64>,
    said: RwSignal<Option<String>>,
    saved: RwSignal<Option<Option<Vec<String>>>>,
) -> impl IntoView {
    let path = format!("/source-sets/{}/coverage", api::segment(&name));
    let draft = RwSignal::new(Draft::of(coverage.as_deref()));
    let order = RwSignal::new(Vec::<String>::new());
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let proposed = Memo::new(move |_| order.with(|o| draft.with(|d| d.coverage(o))));
    let preview = {
        let path = format!("{path}/preview");
        LocalResource::new(move || {
            let proposed = proposed.get();
            let path = path.clone();
            async move {
                let made = match &proposed {
                    Err(why) => Err(why.clone()),
                    Ok(coverage) => {
                        let request = SetCoverage {
                            coverage: coverage.clone(),
                        };
                        api::post::<_, CoveragePreview>(&path, &request)
                            .await
                            .map_err(|p| {
                                format!("Couldn't preview the change: {p} You can still save it.")
                            })
                    }
                };
                (proposed, made)
            }
        })
    };
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let coverage = match proposed.get_untracked() {
            Ok(coverage) => coverage,
            Err(message) => {
                problem.set(Some(message));
                return;
            }
        };
        let path = path.clone();
        let name = name.clone();
        busy.set(true);
        problem.set(None);
        leptos::task::spawn_local(async move {
            let stored = api::put::<_, SourceSetRecord>(&path, &SetCoverage { coverage }).await;
            busy.set(false);
            match stored {
                Ok(set) => {
                    said.set(Some(format!(
                        "Saved: {name} now covers {}.",
                        model::label_in_sentence(set.coverage.as_deref())
                    )));
                    saved.set(Some(set.coverage));
                    refresh.update(|n| *n += 1);
                    open.set(false);
                }
                Err(message) => problem.set(Some(format!("Couldn't save the coverage: {message}"))),
            }
        });
    };
    view! {
        <form class="form" on:submit=save>
            <CoverageEditor draft order />
            <p class="note" aria-live="polite">
                {move || proposed.with(|now| model::preview_text(now, preview.get().as_ref()))}
            </p>
            <button type="submit" class="primary" disabled=busy>
                "Save coverage"
            </button>
        </form>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// Where you edit the coverage of the source set `name`: its page, with the drawer open.
pub fn edit_href(name: &str) -> String {
    format!("/source-sets/{}?edit=coverage", api::segment(name))
}

/// The coverage a library folder inherits from its source set `name`, read-only, with a link to edit it there.
#[component]
pub fn InheritedCoverage(name: String) -> impl IntoView {
    let href = edit_href(&name);
    let set = LocalResource::new(move || {
        let path = format!("/source-sets/{}", api::segment(&name));
        async move { api::get::<SourceSetRecord>(&path).await }
    });
    view! {
        {move || match set.get() {
            None => "Loading…".into_any(),
            Some(Ok(set)) => {
                let coverage = set.coverage.as_deref();
                format!(
                    "{}, from its source set. {} ",
                    model::label(coverage),
                    model::regions_line(coverage)
                )
                .into_any()
            }
            Some(Err(problem)) => format!("Couldn't load its source set's coverage: {problem} ").into_any(),
        }}
        <A href=href>"Edit its coverage"</A>
    }
}

/// The latest audit's games of the source set `name` by region, as a collapsible table: the regions its coverage
/// covers, or every region. Loaded when first opened.
#[component]
pub fn ByRegion(name: String) -> impl IntoView {
    let opened = RwSignal::new(false);
    let all = RwSignal::new(false);
    let counts = LocalResource::new(move || {
        let path = format!("/source-sets/{}/regions", api::segment(&name));
        let opened = opened.get();
        async move {
            if opened {
                api::get::<RegionCounts>(&path).await.map(Some)
            } else {
                Ok(None)
            }
        }
    });
    let cells = |games: &std::collections::BTreeMap<GameStatus, usize>| {
        GameStatus::ALL
            .into_iter()
            .map(|s| view! { <td class="data">{games.get(&s).copied().unwrap_or(0)}</td> })
            .collect_view()
    };
    let table = move |counts: RegionCounts| {
        let rows = model::region_rows(&counts.regions, all.get())
            .into_iter()
            .map(|r| {
                let outside = (!r.covered).then_some(" (outside your coverage)");
                view! {
                    <tr>
                        <th scope="row">{model::region_name(&r.region).to_owned()} {outside}</th>
                        {cells(&r.games)}
                    </tr>
                }
            })
            .collect_view();
        view! {
            <div class="table-scroll">
                <table aria-label="Games by region">
                    <thead>
                        <tr>
                            <th scope="col">
                                <span class="label">"Region"</span>
                            </th>
                            {GameStatus::ALL
                                .into_iter()
                                .map(|s| {
                                    view! {
                                        <th scope="col" class="data">
                                            <StatusIcon status=s tab_stop=false />
                                            " "
                                            <span class="label">{s.look().label}</span>
                                        </th>
                                    }
                                })
                                .collect_view()}
                        </tr>
                    </thead>
                    <tbody>
                        {rows}
                        <tr>
                            <th scope="row">"Every game, once"</th>
                            {cells(&counts.total)}
                        </tr>
                    </tbody>
                </table>
            </div>
        }
    };
    view! {
        <details class="choices" on:toggle=move |_| opened.set(true)>
            <summary>"By region"</summary>
            <label class="inline">
                <input type="checkbox" prop:checked=all on:change=move |ev| all.set(event_target_checked(&ev)) />
                "All regions"
            </label>
            <p class="note small">
                "A game in several regions counts in each, so the regions add up to more than every game once."
            </p>
            {move || match counts.get() {
                Some(Ok(Some(c))) => table(c).into_any(),
                Some(Err(problem)) => {
                    let message = format!("Couldn't count the games by region: {problem}");
                    view! { <Problem message /> }.into_any()
                }
                _ => view! { <Loading what="the games by region" /> }.into_any(),
            }}
        </details>
    }
}
