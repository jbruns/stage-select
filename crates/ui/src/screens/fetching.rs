//! Where fetching newer DAT revisions from Redump and MAME stands, per provider and system, with a way to fetch now.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_location;
use stage_select_api::fetch::{FetchProviderStatus, FetchStatus, FetchSupport, FetchSystem};

use crate::api;
use crate::events::use_live;
use crate::fetch_model::{
    NextFetch, artifact_name, failures, last_fetched, next_fetch, provider_state, refresh_after,
    support_next, support_state, system_state,
};
use crate::model::when;
use crate::parts::help::Help;
use crate::parts::pending::{ActionPending, Pending};
use crate::parts::status::{StatusBadge, StatusIcon};

/// Where you turn fetching from each provider on or off.
pub(crate) const FETCHING_SETTING: &str = "/settings/operations#fetching";
/// Where fetching stands, and where you fetch now.
pub(crate) const FETCHING_STATUS: &str = "/dats#fetching";
/// Where you choose what is fetched: each source set's "Follow its DAT".
pub(crate) const FOLLOWING: &str = "/source-sets";

/// Scrolls to the element `id` once `drawn`, when the link that opened the page named it (`#id`). The browser can't
/// do it itself: the link arrives before the element exists.
pub(crate) fn scroll_when_named(id: &'static str, drawn: impl Fn() -> bool + 'static) {
    let hash = use_location().hash;
    let scrolled = StoredValue::new(false);
    Effect::new(move || {
        if drawn() && !scrolled.get_value() && hash.get_untracked() == format!("#{id}") {
            scrolled.set_value(true);
            request_animation_frame(move || {
                if let Some(element) = document().get_element_by_id(id) {
                    element.scroll_into_view();
                }
            });
        }
    });
}

fn provider_name(p: &FetchProviderStatus) -> &'static str {
    p.provider.dat_provider()
}

/// The fetching panel of the DATs screen.
#[component]
pub fn Fetching() -> impl IntoView {
    let live = use_live();
    let fetched = RwSignal::new(0u32);
    let status = LocalResource::new(move || {
        live.settled.track();
        fetched.track();
        api::get::<FetchStatus>("/fetch")
    });
    let fetch = Action::new_local(move |provider: &String| {
        let path = format!("/fetch/{provider}");
        async move {
            let now: Result<FetchStatus, String> = api::post(&path, &()).await;
            fetched.update(|n| *n += 1);
            now
        }
    });
    // The event stream carries jobs only, so a fetch that the schedule starts, or one still running after its POST
    // returned, shows up by polling.
    Effect::new(move |_| {
        let after = status
            .get()
            .and_then(Result::ok)
            .as_ref()
            .and_then(refresh_after);
        if let Some(after) = after
            && let Ok(handle) = set_timeout_with_handle(move || fetched.update(|n| *n += 1), after)
        {
            on_cleanup(move || handle.clear());
        }
    });
    let problem = Signal::derive(move || status.get().and_then(Result::err));
    scroll_when_named("fetching", move || status.with(Option::is_some));
    view! {
        <section aria-labelledby="fetching">
            <h2 id="fetching">"Fetching"</h2>
            <p class="note">
                "You turn fetching on under " <A href=FETCHING_SETTING>"Settings, then Operations"</A>
                ", and it fetches the DATs your " <A href=FOLLOWING>"source sets"</A> " follow."
            </p>
            <Help summary="How fetching works">
                <p>
                    "For the systems your source sets follow, the service can fetch newer DAT revisions from Redump and from MAME's releases, and Redump's cue and SBI packs. Each provider is off until you turn it on under Settings, then Operations. Each system is fetched at most once a day, even when you fetch by hand. A fetch that fails keeps the last good DAT revision. A newer DAT revision makes the following source sets stale; fetching never starts an audit."
                </p>
                <p>
                    "With MAME on, the service also fetches catver.ini, languages.ini and bestgames.ini from progetto-SNAPS, for selecting arcade machines: when a source set first has a MAME ListXML DAT revision, and whenever a newer one arrives. A fetch that fails keeps the last good files. You can also import them yourself, or drop them into an ingest folder."
                </p>
                <p>
                    "No-Intro DATs are never fetched: download them yourself and drop them into an ingest folder, or import them here."
                </p>
            </Help>
            <ActionPending action=fetch status="Fetching…".to_string()>
                <Pending
                    pending=Signal::derive(move || status.get().is_none())
                    status="Loading fetching…"
                    problem
                >
                    {move || {
                        status
                            .get()
                            .and_then(Result::ok)
                            .map(|now| {
                                now.providers
                                    .into_iter()
                                    .map(|p| view! { <ProviderPanel provider=p fetch /> })
                                    .collect_view()
                            })
                    }}
                </Pending>
            </ActionPending>
        </section>
    }
}

#[component]
fn ProviderPanel(
    provider: FetchProviderStatus,
    fetch: Action<String, Result<FetchStatus, String>>,
) -> impl IntoView {
    let name = provider_name(&provider);
    let key = provider.provider.as_str().to_string();
    let state = provider_state(&provider);
    let can_fetch = provider.enabled && !provider.running;
    let release = provider.release.clone().and_then(|r| {
        r.last_success
            .and_then(|a| a.release)
            .map(|tag| format!("Latest release found: {tag}."))
    });
    let off = (!provider.enabled).then(|| {
        view! {
            <p>
                "Off. To turn it on, go to " <A href=FETCHING_SETTING>"Settings, then Operations"</A>
                {format!(", and turn on fetching from {name}.")}
            </p>
        }
    });
    let note = if !provider.enabled {
        None
    } else if provider.systems.is_empty() && provider.support.is_some() {
        Some(format!(
            "No source set follows a {name} DAT, so only the support files are fetched."
        ))
    } else if provider.systems.is_empty() {
        Some(format!(
            "No source set follows a {name} DAT, so nothing is fetched."
        ))
    } else {
        None
    };
    let support = provider
        .support
        .clone()
        .map(|s| view! { <SupportPanel provider=provider.clone() support=s /> });
    let systems = provider.systems.clone();
    let rows = systems
        .iter()
        .map(|s| system_row(&provider, s))
        .collect_view();
    view! {
        <section aria-label=name class="fetch-provider">
            <h3>
                {name} " " <StatusBadge status=state />
            </h3>
            <button
                type="button"
                disabled=move || !can_fetch || fetch.pending().get()
                on:click=move |_| {
                    fetch.dispatch_local(key.clone());
                }
            >
                {format!("Fetch from {name} now")}
            </button>
            {release.map(|r| view! { <p>{r}</p> })}
            {off}
            {note.map(|n| view! { <p>{n}</p> })}
            {(!provider.systems.is_empty())
                .then(move || {
                    view! {
                        <div class="table-scroll">
                            <table aria-label=format!("{name} systems")>
                                <thead>
                                    <tr>
                                        <th scope="col">
                                            <span class="label">"State"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"DAT"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Fetches"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Source sets"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Last fetched"</span>
                                        </th>
                                        <th scope="col">
                                            <span class="label">"Next"</span>
                                        </th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {rows}
                                </tbody>
                            </table>
                        </div>
                        <SystemProblems systems />
                    }
                })}
            {support}
        </section>
    }
}

fn system_row(provider: &FetchProviderStatus, system: &FetchSystem) -> impl IntoView + use<> {
    let state = system_state(provider, system);
    let fetches = if system.system.is_none() {
        "Can't fetch".to_string()
    } else {
        system
            .artifacts
            .iter()
            .map(|a| artifact_name(a.artifact))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let last = last_fetched(&system.artifacts).map_or_else(|| "Never".to_string(), |t| when(&t));
    let next = next_fetch(&system.artifacts).map_or_else(|| "Now".to_string(), |t| when(&t));
    view! {
        <tr>
            <td>
                <StatusIcon status=state />
            </td>
            <td>{system.dat.clone()}</td>
            <td>{fetches}</td>
            <td>{system.source_sets.join(", ")}</td>
            <td>{last}</td>
            <td>{next}</td>
        </tr>
    }
}

/// Why each system's last fetch failed, under its provider's table.
#[component]
fn SystemProblems(systems: Vec<FetchSystem>) -> impl IntoView {
    let problems: Vec<String> = systems
        .iter()
        .flat_map(|s| {
            failures(&s.artifacts)
                .into_iter()
                .map(move |(artifact, error)| {
                    format!("{} {}: {error}", s.dat, artifact_name(artifact))
                })
        })
        .collect();
    (!problems.is_empty()).then(|| {
        view! {
            <ul class="problems" aria-label="Failed fetches">
                {problems.into_iter().map(|p| view! { <li>{p}</li> }).collect_view()}
            </ul>
        }
    })
}

/// Where fetching MAME's support files stands, under MAME's systems.
#[component]
fn SupportPanel(provider: FetchProviderStatus, support: FetchSupport) -> impl IntoView {
    let state = support_state(&provider, &support);
    let last = last_fetched(&support.artifacts).map_or_else(|| "Never".to_string(), |t| when(&t));
    let owed = if support.fetched_for_listxml {
        "Fetched for your newest MAME ListXML DAT revisions."
    } else {
        "Not yet fetched for your newest MAME ListXML DAT revisions."
    };
    let next = match support_next(&provider, &support) {
        Some(NextFetch::Now) => " Next: now.".to_string(),
        Some(NextFetch::At(t)) => format!(" Next: {}.", when(&t)),
        None if support.fetched_for_listxml => {
            " They're fetched again when a newer one arrives.".to_string()
        }
        None => String::new(),
    };
    let problems: Vec<String> = failures(&support.artifacts)
        .into_iter()
        .map(|(artifact, error)| format!("{}: {error}", artifact_name(artifact)))
        .collect();
    view! {
        <section aria-label="MAME support files">
            <h4>"Support files " <StatusBadge status=state /></h4>
            <p>
                {format!(
                    "catver.ini, languages.ini and bestgames.ini from progetto-SNAPS, for {}. {owed}",
                    support.source_sets.join(", "),
                )}
            </p>
            <p>{format!("Last fetched: {last}.{next}")}</p>
            {(!problems.is_empty())
                .then(|| {
                    view! {
                        <ul class="problems" aria-label="Failed support file fetches">
                            {problems.into_iter().map(|p| view! { <li>{p}</li> }).collect_view()}
                        </ul>
                    }
                })}
        </section>
    }
}
