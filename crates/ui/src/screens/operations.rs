//! The Operations settings (#139): the Job pace, retention, Path Templates, checks, fetching, chdman's time limits,
//! each Ingest Folder's cadence and the webhook. They apply when saved, without a restart; the webhook's URL and
//! secret are write-only.

use leptos::prelude::*;
use title_select_api::settings::{NotifyEvent, Pace, SecretUpdate, SettingsView};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::settings_model::{CadenceDraft, Draft, pace_label, secret_state};

fn event_label(event: NotifyEvent) -> &'static str {
    match event {
        NotifyEvent::JobFinished => "A job finishes",
        NotifyEvent::JobFailed => "A job fails",
        NotifyEvent::SourceSetStale => "A source set goes stale",
        NotifyEvent::DatRevisionNew => "A new DAT revision arrives",
        NotifyEvent::FetchFailed => "A fetch fails",
        NotifyEvent::DriftFound => "A check finds changed files in a source set",
    }
}

#[component]
pub fn Operations() -> impl IntoView {
    let loaded = LocalResource::new(|| api::get::<SettingsView>("/settings"));
    view! {
        <h1>"Operations"</h1>
        <p class="lede">
            "How hard jobs work, what is kept, how folders are named, and what is checked, fetched and sent. Each change applies as soon as you save it, without a restart."
        </p>
        {move || match loaded.get() {
            None => view! { <Loading what="your settings" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(saved)) => view! { <Editor saved /> }.into_any(),
        }}
    }
}

#[component]
fn Editor(saved: SettingsView) -> impl IntoView {
    let draft = RwSignal::new(Draft::of(&saved));
    let saved = RwSignal::new(saved);
    let dirty = Memo::new(move |_| saved.with(|s| draft.with(|d| *d != Draft::of(s))));
    let outcome = RwSignal::new(None::<Result<(), String>>);
    let saving = RwSignal::new(false);
    // Settings someone saved elsewhere while you had changes here, shown once you discard yours.
    let newer = RwSignal::new(None::<SettingsView>);
    // A change heard of while a save was under way, fetched once the save is done: the save's response may be older.
    let missed = RwSignal::new(false);
    // Goes up with each save, so a fetch that a save overtook is fetched again rather than applied.
    let saves = RwSignal::new(0u64);
    let refetch = RwSignal::new(0u64);
    let live = use_live();
    Effect::watch(
        move || (live.settings.get(), refetch.get()),
        move |_, _, _| {
            if saving.get_untracked() {
                missed.set(true);
                return;
            }
            let started = saves.get_untracked();
            leptos::task::spawn_local(async move {
                let Ok(now) = api::get::<SettingsView>("/settings").await else {
                    return;
                };
                if saving.get_untracked() {
                    missed.set(true);
                    return;
                }
                if saves.get_untracked() != started {
                    refetch.update(|n| *n += 1);
                    return;
                }
                if saved.with_untracked(|s| *s == now) {
                    return;
                }
                if dirty.get_untracked() {
                    newer.set(Some(now));
                } else {
                    draft.set(Draft::of(&now));
                    saved.set(now);
                }
            });
        },
        false,
    );
    let save = move |_| {
        let update = match saved.with_untracked(|s| draft.with_untracked(|d| d.update(s))) {
            Ok(update) => update,
            Err(message) => return outcome.set(Some(Err(message))),
        };
        saves.update(|n| *n += 1);
        saving.set(true);
        leptos::task::spawn_local(async move {
            let result = api::patch::<_, SettingsView>("/settings", &update).await;
            saving.set(false);
            match result {
                Ok(now) => {
                    draft.set(Draft::of(&now));
                    saved.set(now);
                    newer.set(None);
                    outcome.set(Some(Ok(())));
                }
                Err(message) => outcome.set(Some(Err(message))),
            }
            if missed.get_untracked() {
                missed.set(false);
                refetch.update(|n| *n += 1);
            }
        });
    };
    view! {
        <div class="settings">
            <PaceSection saved draft />
            <fieldset class="setting">
                <legend>"Retention"</legend>
                <p class="note small">
                    "How many full audits and selections each source set keeps. Older ones are removed after each new one; their summaries stay."
                </p>
                <NumberField draft id="audits-keep" label="Audits kept" get=|d| d.audits_keep.clone() set=|d, v| d.audits_keep = v />
                <NumberField draft id="selections-keep" label="Selections kept" get=|d| d.selections_keep.clone() set=|d, v| d.selections_keep = v />
            </fieldset>
            <fieldset class="setting">
                <legend>"Path templates"</legend>
                <p class="note small">
                    "The paths proposed when you register a source set or a library folder. You can change the path at registration. Tokens: "
                    <code>"{provider}"</code>", "<code>"{dat}"</code>" and "<code>"{platform}"</code>"."
                </p>
                <TextField draft id="source-set-template" label="Source sets, in their root" get=|d| d.source_set_template.clone() set=|d, v| d.source_set_template = v />
                <TextField draft id="library-folder-template" label="Library folders, in their play library root" get=|d| d.library_folder_template.clone() set=|d, v| d.library_folder_template = v />
            </fieldset>
            <fieldset class="setting">
                <legend>"Checking source sets"</legend>
                <NumberField
                    draft
                    id="check-every"
                    label="Check each audited source set for changed files every, in minutes; 0 checks only when you ask"
                    get=|d| d.check_every_minutes.clone()
                    set=|d, v| d.check_every_minutes = v
                />
            </fieldset>
            <fieldset class="setting">
                <legend>"Fetching DAT revisions"</legend>
                <p class="note small">
                    "With fetching from a provider on, the service fetches newer DAT revisions for the systems you follow, at most once a day each. Turning one on takes effect within the hour."
                </p>
                <p>
                    <label class="inline">
                        <input
                            type="checkbox"
                            prop:checked=move || draft.with(|d| d.fetch.redump)
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                draft.update(|d| d.fetch.redump = on);
                            }
                        />
                        "Fetch from Redump"
                    </label>
                </p>
                <p>
                    <label class="inline">
                        <input
                            type="checkbox"
                            prop:checked=move || draft.with(|d| d.fetch.mame)
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                draft.update(|d| d.fetch.mame = on);
                            }
                        />
                        "Fetch from MAME"
                    </label>
                </p>
            </fieldset>
            <fieldset class="setting">
                <legend>"CHD time limits"</legend>
                <p class="note small">
                    "How long chdman may run on one disc before it is stopped and the disc fails: the time limit, plus the time added for each GiB of the disc's tracks."
                </p>
                <NumberField draft id="chd-seconds" label="Each chdman run's time limit, in seconds" get=|d| d.chd_seconds.clone() set=|d, v| d.chd_seconds = v />
                <NumberField draft id="chd-per-gib" label="Time added for each GiB, in seconds" get=|d| d.chd_seconds_per_gib.clone() set=|d, v| d.chd_seconds_per_gib = v />
            </fieldset>
            <IngestSection draft />
            <WebhookSection saved draft />
        </div>
        <section class="staged" aria-labelledby="save-title">
            <h2 id="save-title">"Save"</h2>
            <div class="actions">
                <button
                    type="button"
                    class="primary"
                    disabled=move || !dirty.get() || saving.get()
                    on:click=save
                >
                    {move || if saving.get() { "Saving…" } else { "Save your settings" }}
                </button>
                <button
                    type="button"
                    disabled=move || !dirty.get() || saving.get()
                    on:click=move |_| {
                        if let Some(now) = newer.try_update(Option::take).flatten() {
                            saved.set(now);
                        }
                        draft.set(saved.with_untracked(Draft::of));
                        outcome.set(None);
                    }
                >
                    "Discard changes"
                </button>
            </div>
            {move || {
                newer
                    .with(Option::is_some)
                    .then(|| {
                        view! {
                            <p class="note" role="status">
                                "Your settings were changed elsewhere since you opened them. Save to replace them with yours, or discard your changes to see them."
                            </p>
                        }
                    })
            }}
            {move || match outcome.get() {
                Some(Ok(())) if !dirty.get() => {
                    view! { <p class="done" role="status">"Saved your settings. They apply now."</p> }.into_any()
                }
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                _ => ().into_any(),
            }}
        </section>
    }
}

#[component]
fn PaceSection(saved: RwSignal<SettingsView>, draft: RwSignal<Draft>) -> impl IntoView {
    let counts = move || {
        let ceilings = saved.with(|s| s.jobs.ceilings);
        (ceilings, draft.with(|d| d.pace).counts(ceilings))
    };
    view! {
        <fieldset class="setting">
            <legend>"Job pace"</legend>
            <p class="note small">
                "The pace is a share of the ceilings in your configuration file. A new pace reaches running jobs at each worker's next file; a lower one never stops a running job."
            </p>
            {Pace::ALL
                .into_iter()
                .map(|pace| {
                    view! {
                        <p>
                            <label class="inline">
                                <input
                                    type="radio"
                                    name="pace"
                                    prop:checked=move || draft.with(|d| d.pace == pace)
                                    on:change=move |_| draft.update(|d| d.pace = pace)
                                />
                                {pace_label(pace)}
                            </label>
                        </p>
                    }
                })
                .collect_view()}
            <div class="table-scroll">
                <table aria-label="Job limits">
                    <thead>
                        <tr>
                            <th scope="col"><span class="label">"Limit"</span></th>
                            <th scope="col" class="numeric"><span class="label">"Ceiling"</span></th>
                            <th scope="col" class="numeric"><span class="label">"At this pace"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            let (ceilings, active) = counts();
                            [
                                ("Jobs at once", ceilings.concurrent, active.concurrent),
                                ("Readers in each job", ceilings.readers, active.readers),
                                ("Hash workers in each job", ceilings.hashers, active.hashers),
                            ]
                                .into_iter()
                                .map(|(what, ceiling, now)| {
                                    view! {
                                        <tr>
                                            <th scope="row">{what}</th>
                                            <td class="numeric">{ceiling}</td>
                                            <td class="numeric">{now}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
            {move || {
                (draft.with(|d| d.pace) == Pace::Unthrottled)
                    .then(|| {
                        view! {
                            <p class="note small tone-review">
                                "Unthrottled, jobs use every ceiling in full and can slow everything else on this server. The header shows Unthrottled while it is on."
                            </p>
                        }
                    })
            }}
        </fieldset>
    }
}

#[component]
fn IngestSection(draft: RwSignal<Draft>) -> impl IntoView {
    let roots = move || draft.with(|d| d.ingest_folders.keys().cloned().collect::<Vec<_>>());
    let field = move |root: String, settle: bool| {
        let root = StoredValue::new(root);
        let value = move || {
            draft.with(|d| {
                d.ingest_folders.get(&root.get_value()).map_or_else(
                    String::new,
                    |c: &CadenceDraft| {
                        if settle {
                            c.settle_secs.clone()
                        } else {
                            c.scan_every_minutes.clone()
                        }
                    },
                )
            })
        };
        let set = move |v: String| {
            draft.update(|d| {
                if let Some(c) = d.ingest_folders.get_mut(&root.get_value()) {
                    if settle {
                        c.settle_secs = v;
                    } else {
                        c.scan_every_minutes = v;
                    }
                }
            })
        };
        view! {
            <input
                type="text"
                inputmode="numeric"
                aria-label=move || {
                    format!(
                        "{}: {}",
                        root.get_value(),
                        if settle { "settle after, in seconds" } else { "scan every, in minutes" },
                    )
                }
                prop:value=value
                on:input=move |ev| set(event_target_value(&ev))
            />
        }
    };
    view! {
        <fieldset class="setting">
            <legend>"Ingest folders"</legend>
            <p class="note small">
                "An arriving item is assessed once its size and times stay unchanged for the settle time. Leave scan every empty to scan only when you ask."
            </p>
            {move || {
                let roots = roots();
                if roots.is_empty() {
                    return view! { <p>"You have no ingest folders."</p> }.into_any();
                }
                view! {
                    <div class="table-scroll">
                        <table aria-label="Ingest folders">
                            <thead>
                                <tr>
                                    <th scope="col"><span class="label">"Ingest folder"</span></th>
                                    <th scope="col"><span class="label">"Settle after, in seconds"</span></th>
                                    <th scope="col"><span class="label">"Scan every, in minutes"</span></th>
                                </tr>
                            </thead>
                            <tbody>
                                {roots
                                    .into_iter()
                                    .map(|root| {
                                        view! {
                                            <tr>
                                                <th scope="row">{root.clone()}</th>
                                                <td>{field(root.clone(), true)}</td>
                                                <td>{field(root, false)}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    </div>
                }
                    .into_any()
            }}
        </fieldset>
    }
}

#[component]
fn WebhookSection(saved: RwSignal<SettingsView>, draft: RwSignal<Draft>) -> impl IntoView {
    view! {
        <fieldset class="setting">
            <legend>"Webhook"</legend>
            <p class="note small">
                "Title Select posts each event you choose to the webhook URL, signed with the secret. Both are needed before anything is sent. Neither is ever shown again once saved: you can only replace or clear them."
            </p>
            <SecretField
                draft
                id="webhook-url"
                label="Webhook URL"
                kind="url"
                is_set=Signal::derive(move || saved.with(|s| s.notify.url_set))
                get=|d| d.url.clone()
                set=|d, v| d.url = v
            />
            <SecretField
                draft
                id="webhook-secret"
                label="Signing secret"
                kind="password"
                is_set=Signal::derive(move || saved.with(|s| s.notify.secret_set))
                get=|d| d.secret.clone()
                set=|d, v| d.secret = v
            />
            <p>"Send when:"</p>
            <ul class="checks">
                {NotifyEvent::ALL
                    .into_iter()
                    .map(|event| {
                        view! {
                            <li>
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        prop:checked=move || draft.with(|d| d.events.contains(&event))
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            draft.update(|d| d.set_event(event, on));
                                        }
                                    />
                                    {event_label(event)}
                                </label>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
            <TextField
                draft
                id="workbench-url"
                label="Your workbench's address, for links in each event; leave it empty to send none"
                get=|d| d.workbench_url.clone()
                set=|d, v| d.workbench_url = v
            />
        </fieldset>
    }
}

/// A write-only secret: whether it is set, a field to replace it, and a button to clear it.
#[component]
fn SecretField(
    draft: RwSignal<Draft>,
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    is_set: Signal<bool>,
    get: fn(&Draft) -> SecretUpdate,
    set: fn(&mut Draft, SecretUpdate),
) -> impl IntoView {
    let update = move || draft.with(get);
    view! {
        <label class="stacked" for=id>
            {label}": "{move || secret_state(is_set.get(), &update())}
        </label>
        <div class="source">
            <input
                id=id
                type=kind
                autocomplete="off"
                placeholder=move || if is_set.get() { "Type a new one to replace it" } else { "" }
                prop:value=move || match update() {
                    SecretUpdate::Set(text) => text,
                    _ => String::new(),
                }
                on:input=move |ev| {
                    let text = event_target_value(&ev);
                    draft.update(|d| {
                        set(d, if text.is_empty() { SecretUpdate::Keep } else { SecretUpdate::Set(text) })
                    });
                }
            />
            <button
                type="button"
                disabled=move || !is_set.get() || update() == SecretUpdate::Clear
                on:click=move |_| draft.update(|d| set(d, SecretUpdate::Clear))
            >
                {format!("Clear the {}", label.to_lowercase())}
            </button>
            {move || {
                (update() != SecretUpdate::Keep)
                    .then(|| {
                        view! {
                            <button type="button" on:click=move |_| draft.update(|d| set(d, SecretUpdate::Keep))>
                                {format!("Keep the {}", label.to_lowercase())}
                            </button>
                        }
                    })
            }}
        </div>
    }
}

#[component]
fn NumberField(
    draft: RwSignal<Draft>,
    id: &'static str,
    label: &'static str,
    get: fn(&Draft) -> String,
    set: fn(&mut Draft, String),
) -> impl IntoView {
    view! {
        <label class="stacked" for=id>{label}</label>
        <input
            id=id
            type="text"
            inputmode="numeric"
            prop:value=move || draft.with(get)
            on:input=move |ev| {
                let value = event_target_value(&ev);
                draft.update(|d| set(d, value));
            }
        />
    }
}

#[component]
fn TextField(
    draft: RwSignal<Draft>,
    id: &'static str,
    label: &'static str,
    get: fn(&Draft) -> String,
    set: fn(&mut Draft, String),
) -> impl IntoView {
    view! {
        <label class="stacked" for=id>{label}</label>
        <input
            id=id
            type="text"
            class="wide"
            prop:value=move || draft.with(get)
            on:input=move |ev| {
                let value = event_target_value(&ev);
                draft.update(|d| set(d, value));
            }
        />
    }
}
