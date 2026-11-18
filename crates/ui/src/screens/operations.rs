//! The Operations settings (#139): the Job pace, retention, Path Templates, checks, fetching, chdman's time limits,
//! each Ingest Folder's cadence, the webhook and the RomM connection (#140). They apply when saved, without a
//! restart; the webhook's URL and secret and the RomM token are write-only.

use leptos::prelude::*;
use title_select_api::calibration::Calibration;
use title_select_api::romm::{RommCheck, RommFolderPlatform, RommTestRequest};
use title_select_api::settings::{Ceiling, NotifyEvent, Pace, SecretUpdate, SettingsView};
use title_select_api::{Job, Root};

use crate::api;
use crate::app::{Loading, Problem};
use crate::calibration_model;
use crate::events::use_live;
use crate::parts::legend::{Legend, present};
use crate::parts::overlay::Tooltip;
use crate::parts::pending::{ActionPending, Pending};
use crate::parts::status::{StatusBadge, StatusIcon};
use crate::settings_model::{
    CadenceDraft, CeilingField, CeilingKey, Draft, Refreshed, at_pace, ceiling, ceiling_curve,
    ceiling_keys, locked_reason, pace_label, refreshed, romm_check_lines, romm_platform_label,
    secret_state, source_label,
};

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
            "How hard jobs work, what is kept, how folders are named, what is checked, fetched and sent, and how to reach RomM. Each change applies as soon as you save it, without a restart."
        </p>
        {move || match loaded.get() {
            None => view! { <Loading what="your settings" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(saved)) => view! { <Editor saved /> }.into_any(),
        }}
        <CalibrationSection />
    }
}

/// Calibration: start one, see where it stands, and the results that apply now.
#[component]
fn CalibrationSection() -> impl IntoView {
    let live = use_live();
    let results = LocalResource::new(move || {
        live.settled.get();
        api::get::<Calibration>("/calibration")
    });
    let active = Memo::new(move |_| {
        live.book.with(|book| {
            let jobs = book.newest_first();
            calibration_model::active(jobs.iter().map(|l| (&l.job, l.progress.as_ref())))
        })
    });
    let start =
        Action::new_local(|_: &()| async { api::post::<_, Job>("/calibrations", &()).await });
    let cancel = Action::new_local(|job: &i64| {
        let path = format!("/jobs/{job}/cancellation");
        async move { api::post::<_, Job>(&path, &serde_json::json!({})).await }
    });
    view! {
        <section class="staged" aria-labelledby="calibration-title">
            <h2 id="calibration-title">"Calibration"</h2>
            <p class="note small">
                "Calibration measures how fast the host hashes as hash workers are added, and how fast each root reads as payload readers are added, on files you audited. It only reads; it never writes to a root. It waits for running jobs to finish, and no other job starts until it ends. It's never run again by itself: when the host or a root changes, its results are stale and still apply until you calibrate again."
            </p>
            <details class="note small">
                <summary>"Calibrate after a quiet period"</summary>
                <p>
                    "A filesystem that caches what it reads, such as ZFS with its ARC, can make a root look faster than it is. Calibrate when little has been read lately, and each file is read at most once in a calibration."
                </p>
            </details>
            <ActionPending action=start status="Starting calibration…">
                <ActionPending action=cancel status="Cancelling calibration…">
                    <div class="actions">
                        {move || match active.get() {
                            None => view! {
                                <button
                                    type="button"
                                    class="primary"
                                    disabled=move || start.pending().get()
                                    on:click=move |_| {
                                        start.dispatch_local(());
                                    }
                                >
                                    "Calibrate"
                                </button>
                            }
                                .into_any(),
                            Some(calibrating) => {
                                let job = calibrating.job;
                                view! {
                                    <p>
                                        {calibrating.stands} " (job " {job} ")"
                                    </p>
                                    <button
                                        type="button"
                                        disabled=calibrating.cancelling || cancel.pending().get()
                                        on:click=move |_| {
                                            cancel.dispatch_local(job);
                                        }
                                    >
                                        "Cancel calibration"
                                    </button>
                                }
                                    .into_any()
                            }
                        }}
                    </div>
                </ActionPending>
            </ActionPending>
            {move || match results.get() {
                None => view! { <Loading what="the calibration" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(calibration)) => view! { <CalibrationResults calibration /> }.into_any(),
            }}
        </section>
    }
}

#[component]
fn CalibrationResults(calibration: Calibration) -> impl IntoView {
    let rows = calibration_model::rows(&calibration);
    let statuses = present(rows.iter().map(|r| r.status));
    let reasons: Vec<String> = rows.iter().filter_map(|r| r.reason.clone()).collect();
    let body = rows
        .into_iter()
        .map(|row| {
            let (workers, rate) = match row.knee {
                Some(knee) => (
                    knee.workers.to_string(),
                    calibration_model::mib_per_s(knee.bytes_per_s),
                ),
                None => ("–".to_owned(), "–".to_owned()),
            };
            view! {
                <tr>
                    <td><StatusBadge status=row.status /></td>
                    <th scope="row">{row.what}</th>
                    <td>{row.of}</td>
                    <td class="numeric">{workers}</td>
                    <td class="numeric">{rate}</td>
                    <td>{calibration_model::curve(&row.curve)}</td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <div class="table-scroll">
            <table aria-label="Calibration results">
                <thead>
                    <tr>
                        <th scope="col"><span class="label">"Status"</span></th>
                        <th scope="col"><span class="label">"Measured"</span></th>
                        <th scope="col"><span class="label">"Of"</span></th>
                        <th scope="col" class="numeric"><span class="label">"Workers at the knee"</span></th>
                        <th scope="col" class="numeric"><span class="label">"MiB/s at the knee"</span></th>
                        <th scope="col"><span class="label">"MiB/s by workers"</span></th>
                    </tr>
                </thead>
                <tbody>{body}</tbody>
            </table>
        </div>
        <Legend statuses=statuses list="calibration" />
        {(!reasons.is_empty())
            .then(|| {
                view! {
                    <ul class="note small">
                        {reasons.into_iter().map(|r| view! { <li>{r}</li> }).collect_view()}
                    </ul>
                }
            })}
        <p class="note small">
            "The knee is the fewest workers within 10% of the fastest, never past a step that was slower than the one before. The host's are hash workers; a root's are payload readers."
        </p>
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
                let dirty = dirty.get_untracked();
                match saved.with_untracked(|s| refreshed(s, &now, dirty)) {
                    Refreshed::Same => {}
                    Refreshed::Show => {
                        if !dirty {
                            draft.set(Draft::of(&now));
                        }
                        saved.set(now);
                    }
                    Refreshed::Newer => newer.set(Some(now)),
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
            <PaceSection saved draft newer />
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
            <RommSection saved draft />
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
fn PaceSection(
    saved: RwSignal<SettingsView>,
    draft: RwSignal<Draft>,
    newer: RwSignal<Option<SettingsView>>,
) -> impl IntoView {
    view! {
        <fieldset class="setting">
            <legend>"Job pace"</legend>
            <p class="note small">
                "The pace is a share of each ceiling, shared by every job running at once. A new pace reaches running jobs at each worker's next file; a lower one never stops a running job."
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
            {move || {
                (draft.with(|d| d.pace) == Pace::Unthrottled)
                    .then(|| {
                        view! {
                            <p class="note small tone-review">
                                "Unthrottled, jobs use every ceiling in full, with no limit on reads or writes, and can slow everything else on this server. The header shows Unthrottled while it is on."
                            </p>
                        }
                    })
            }}
            <CeilingsTable saved draft newer />
        </fieldset>
    }
}

/// Every Ceiling of the Host and of each Root: what the pace makes of it, where it comes from, what a calibration
/// measured, and your own value, which you can change unless the configuration file sets it.
#[component]
fn CeilingsTable(
    saved: RwSignal<SettingsView>,
    draft: RwSignal<Draft>,
    newer: RwSignal<Option<SettingsView>>,
) -> impl IntoView {
    let live = use_live();
    let calibration = LocalResource::new(move || {
        live.settled.get();
        api::get::<Calibration>("/calibration")
    });
    // The calibration's row for the Host (`None`) or a Root, for its curve and whether it is stale.
    let measured = move |root: Option<String>| {
        calibration.with(|c| {
            let rows = calibration_model::rows(c.as_ref()?.as_ref().ok()?);
            match root {
                None => rows.into_iter().next(),
                Some(root) => rows.into_iter().skip(1).find(|r| r.what == root),
            }
        })
    };
    let rows = move || {
        saved.with(|s| {
            ceiling_keys(&s.jobs)
                .into_iter()
                .filter_map(|key| {
                    let c = ceiling(&s.jobs, &key)?.clone();
                    let observed = match &key {
                        (Some(root), CeilingField::WriteMibPerS) => s
                            .jobs
                            .roots
                            .iter()
                            .find(|r| r.root == *root)
                            .and_then(|r| r.observed_write_mib_per_s),
                        _ => None,
                    };
                    Some((key, c, observed))
                })
                .collect::<Vec<_>>()
        })
    };
    view! {
        <details class="ceilings">
            <summary>"Adjust ceilings"</summary>
            <p class="note small">
                "Each ceiling is the most the pace shares out. Its value comes from the first of these that has one: your configuration file, yours, the calibration, then the CPU count. Clear yours to fall back. Read and write rates have no limit until a calibration or you set one, and roots on one storage pool add up."
            </p>
            <div class="table-scroll">
                <table aria-label="Ceilings">
                    <thead>
                        <tr>
                            <th scope="col"><span class="label">"Of"</span></th>
                            <th scope="col"><span class="label">"Limit"</span></th>
                            <th scope="col" class="numeric"><span class="label">"At this pace"</span></th>
                            <th scope="col" class="numeric"><span class="label">"Ceiling"</span></th>
                            <th scope="col"><span class="label">"Source"</span></th>
                            <th scope="col"><span class="label">"Yours"</span></th>
                            <th scope="col"><span class="label">"Calibration"</span></th>
                            <th scope="col"><span class="label">"MiB/s by workers"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows()
                                .into_iter()
                                .map(|(key, c, observed)| {
                                    view! { <CeilingRow key c observed draft measured /> }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
            <div class="actions">
                <button
                    type="button"
                    on:click=move |_| {
                        // The latest measurements, even in settings kept aside while you have changes.
                        let latest = newer
                            .with_untracked(|n| n.as_ref().map(|n| n.jobs.clone()))
                            .unwrap_or_else(|| saved.with_untracked(|s| s.jobs.clone()));
                        draft.update(|d| d.use_measured(&latest));
                    }
                >
                    "Use measured values"
                </button>
                <button
                    type="button"
                    on:click=move |_| saved.with_untracked(|s| draft.update(|d| d.reset_ceilings(&s.jobs)))
                >
                    "Reset to defaults"
                </button>
            </div>
            <p class="note small">
                "Both change only your values, and apply when you save. Running jobs pick up a saved ceiling at each worker's next file."
            </p>
        </details>
    }
}

#[component]
fn CeilingRow(
    key: CeilingKey,
    c: Ceiling,
    observed: Option<u64>,
    draft: RwSignal<Draft>,
    measured: impl Fn(Option<String>) -> Option<calibration_model::Row> + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let (root, field) = key.clone();
    let of = root.clone().unwrap_or_else(|| "Host".into());
    let id = format!(
        "ceiling-{}-{field:?}",
        root.as_deref()
            .unwrap_or("host")
            .replace(|ch: char| !ch.is_ascii_alphanumeric(), "-")
    );
    let ceiling_value = c.ceiling;
    let now = move || at_pace(draft.with(|d| d.pace), field, ceiling_value);
    let shown = c.ceiling.map(|v| v.to_string()).unwrap_or_else(|| {
        if field.is_rate() {
            "No limit".into()
        } else {
            "–".into()
        }
    });
    let source = source_label(c.source);
    let yours = match c.locked_by.clone() {
        Some(locked) => view! {
            <Tooltip label=locked_reason(&locked) focusable=true>
                <span>"Locked"</span>
            </Tooltip>
        }
        .into_any(),
        None => {
            let draft_key = key.clone();
            let get_key = key.clone();
            let placeholder = observed
                .map(|o| format!("{o}, seen in past jobs"))
                .unwrap_or_default();
            let label = format!("Your {} for {of}", field.label().to_lowercase());
            let suggest = observed.map(|o| {
                let typed = key.clone();
                let key = key.clone();
                view! {
                    <button
                        type="button"
                        class="small"
                        disabled=move || draft.with(|d| !d.ceilings.get(&typed).is_none_or(|t| t.trim().is_empty()))
                        on:click=move |_| draft.update(|d| {
                            d.ceilings.insert(key.clone(), o.to_string());
                        })
                    >
                        {format!("Start at {o}")}
                    </button>
                }
            });
            view! {
                <input
                    id=id
                    type="text"
                    inputmode="numeric"
                    size="6"
                    aria-label=label
                    placeholder=placeholder
                    prop:value=move || draft.with(|d| d.ceilings.get(&get_key).cloned().unwrap_or_default())
                    on:input=move |ev| {
                        let value = event_target_value(&ev);
                        draft.update(|d| {
                            d.ceilings.insert(draft_key.clone(), value);
                        });
                    }
                />
                {suggest}
            }
            .into_any()
        }
    };
    // Read as the calibration loads and each time it is fetched again, as a calibration ends.
    let calibrated = Memo::new(move |_| {
        field
            .calibrated()
            .then(|| measured(root.clone()))
            .flatten()
            .map(|r| (r.status, ceiling_curve(field, &r.curve)))
    });
    let status = move || match calibrated.get() {
        Some((status, _)) => view! { <StatusIcon status /> }.into_any(),
        None => "–".into_any(),
    };
    let curve = move || {
        calibrated
            .get()
            .and_then(|(_, curve)| curve)
            .unwrap_or_else(|| "–".into())
    };
    view! {
        <tr>
            <td>{of}</td>
            <th scope="row">{field.label()}</th>
            <td class="numeric">{now}</td>
            <td class="numeric">{shown}</td>
            <td>{source}</td>
            <td>{yours}</td>
            <td>{status}</td>
            <td>{curve}</td>
        </tr>
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

#[component]
fn RommSection(saved: RwSignal<SettingsView>, draft: RwSignal<Draft>) -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<Root>>("/roots"));
    let live = use_live();
    let platforms = LocalResource::new(move || {
        saved.track();
        live.settled.track();
        api::get::<Vec<RommFolderPlatform>>("/romm/platforms")
    });
    let test = Action::new_local(|request: &RommTestRequest| {
        let request = request.clone();
        async move { api::post::<_, RommCheck>("/romm/test", &request).await }
    });
    let run_test = move |_| {
        let request = draft.with_untracked(|d| RommTestRequest {
            base_url: Some(d.romm_base_url.trim().to_owned()).filter(|u| !u.is_empty()),
            token: d.romm_token.clone(),
        });
        test.dispatch_local(request);
    };
    view! {
        <fieldset class="setting">
            <legend>"RomM"</legend>
            <p class="note small">
                "How Title Select reaches your RomM. It is off until you turn it on, and a RomM that can't be reached never stops anything else. The token is never shown again once saved: you can only replace or clear it. A backup of the store holds the token."
            </p>
            <p>
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=move || draft.with(|d| d.romm_enabled)
                        on:change=move |ev| {
                            let on = event_target_checked(&ev);
                            draft.update(|d| d.romm_enabled = on);
                        }
                    />
                    "Use RomM"
                </label>
            </p>
            <TextField
                draft
                id="romm-url"
                label="RomM's address, with its scheme and port, as this server reaches it"
                get=|d| d.romm_base_url.clone()
                set=|d, v| d.romm_base_url = v
            />
            <SecretField
                draft
                id="romm-token"
                label="RomM token"
                kind="password"
                is_set=Signal::derive(move || saved.with(|s| s.romm.token_set))
                get=|d| d.romm_token.clone()
                set=|d, v| d.romm_token = v
            />
            <details>
                <summary>"Which token to create"</summary>
                <p>
                    "In RomM, create a client token for a user that isn't an admin, with the scopes roms.read, platforms.read, collections.read, collections.write and tasks.run. Add me.read to see here which user it signs in as."
                </p>
            </details>
            <label class="stacked" for="romm-root">"The root RomM's library is in"</label>
            <select
                id="romm-root"
                prop:value=move || draft.with(|d| d.romm_root.clone())
                on:change=move |ev| {
                    let root = event_target_value(&ev);
                    draft.update(|d| d.romm_root = root);
                }
            >
                <option value="">"None chosen"</option>
                {move || {
                    roots
                        .get()
                        .and_then(Result::ok)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|r| !r.ingest)
                        .map(|r| {
                            let selected = draft.with_untracked(|d| d.romm_root == r.name);
                            let label = r.name.clone();
                            view! { <option value=r.name selected=selected>{label}</option> }
                        })
                        .collect_view()
                }}
            </select>
            <div class="actions">
                <button type="button" disabled=move || test.pending().get() on:click=run_test>
                    "Test the connection"
                </button>
            </div>
            <ActionPending action=test status="Testing the connection…".to_string()>
                {move || {
                    test.value()
                        .get()
                        .and_then(Result::ok)
                        .map(|check| {
                            view! {
                                <ul class="checks" role="status" aria-label="What the test found">
                                    {romm_check_lines(&check)
                                        .into_iter()
                                        .map(|(tone, line)| view! { <li class=tone.class()>{line}</li> })
                                        .collect_view()}
                                </ul>
                            }
                        })
                }}
            </ActionPending>
            <p>"The RomM platform each library folder builds:"</p>
            <Pending
                pending=Signal::derive(move || platforms.get().is_none())
                status="Loading the platforms…"
                problem=Signal::derive(move || platforms.get().and_then(Result::err))
            >
                {move || {
                    let rows = platforms.get().and_then(Result::ok).unwrap_or_default();
                    if rows.is_empty() {
                        return view! { <p>"You have no library folders."</p> }.into_any();
                    }
                    view! {
                        <div class="table-scroll">
                            <table aria-label="RomM platforms">
                                <thead>
                                    <tr>
                                        <th scope="col"><span class="label">"Library folder"</span></th>
                                        <th scope="col"><span class="label">"RomM platform"</span></th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {rows
                                        .into_iter()
                                        .map(|row| {
                                            let (cell, why) = romm_platform_label(&row.platform);
                                            view! {
                                                <tr>
                                                    <th scope="row">{format!("{}: {}", row.root, row.path)}</th>
                                                    <td>
                                                        {match why {
                                                            Some(why) => {
                                                                view! {
                                                                    <Tooltip label=why.to_string() focusable=true>
                                                                        {cell}
                                                                    </Tooltip>
                                                                }
                                                                    .into_any()
                                                            }
                                                            None => view! { <code>{cell}</code> }.into_any(),
                                                        }}
                                                    </td>
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
            </Pending>
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
