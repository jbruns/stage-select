//! The first run: the home screen leads a store with no Roots here, through registering a Root, importing a DAT and
//! registering a Source Set, to a first Audit and a Library Folder. Each step reuses the form of the screen it
//! belongs to. Nothing here is remembered: once a Root exists, the home screen opens the Source Sets instead.

use leptos::prelude::*;
use leptos_router::NavigateOptions;
use leptos_router::components::{A, Redirect};
use stage_select_api::build::LibraryFolder;
use stage_select_api::roots::FolderSuggestions;
use stage_select_api::{Dat, Root, SourceSetSummary};

use crate::api;
use crate::events::use_live;
use crate::parts::overlay::Drawer;
use crate::parts::pending::{ActionPending, Pending};
use crate::parts::status::{StatusBadge, StepStatus};
use crate::roots_model::RootDraft;
use crate::screens::dats::Upload;
use crate::screens::roots::RootForm;
use crate::screens::source_sets::{Register, new_library_href, queue_audit, set_href};
use crate::source_set_model::running;
use crate::welcome_model::{Facts, LoadState, State, Step, root_name};

/// The home screen: the first-run steps while there are no Roots, else the Source Sets.
#[component]
pub fn Home() -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<Root>>("/roots"));
    let replace = || NavigateOptions {
        replace: true,
        ..Default::default()
    };
    move || {
        roots.get().map(|roots| match roots {
            Ok(roots) if roots.is_empty() => {
                view! { <Redirect path="/welcome" options=replace() /> }.into_any()
            }
            _ => view! { <Redirect path="/source-sets" options=replace() /> }.into_any(),
        })
    }
}

/// What the store holds that the steps show, loaded together.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    roots: Vec<Root>,
    dats: Vec<Dat>,
    sets: Vec<SourceSetSummary>,
    libraries: usize,
}

impl Held {
    fn facts(&self) -> Facts {
        Facts::of(&self.roots, self.dats.len(), &self.sets, self.libraries)
    }
}

async fn held() -> Result<Held, String> {
    Ok(Held {
        roots: api::get("/roots").await?,
        dats: api::get("/dats").await?,
        sets: api::get("/source-set-summaries").await?,
        libraries: api::get::<Vec<LibraryFolder>>("/libraries").await?.len(),
    })
}

#[component]
pub fn Welcome() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    // Each load is tagged with the request that asked for it, so the page knows whether the latest has come back.
    let asked = Memo::new(move |_| live.settled.get() + refresh.get());
    let loaded = LocalResource::new(move || {
        let at = asked.get();
        async move { (at, held().await) }
    });
    // The last load that worked, so a reload doesn't close the forms open in the steps.
    let last = RwSignal::new(None::<Held>);
    Effect::new(move || {
        if let Some((_, Ok(held))) = loaded.get() {
            last.set(Some(held));
        }
    });
    let load = Memo::new(move |_| {
        loaded.with(|latest| {
            let in_flight = latest.as_ref().is_none_or(|(at, _)| *at != asked.get());
            let worked = matches!(latest, Some((_, Ok(_))));
            let error = latest.as_ref().and_then(|(_, r)| r.as_ref().err());
            LoadState::of(
                in_flight,
                worked || last.with(Option::is_some),
                error.map(String::as_str),
            )
        })
    });
    let changed = Callback::new(move |_: ()| refresh.update(|n| *n += 1));
    let skipped = RwSignal::new(Vec::<Step>::new());
    view! {
        <h1>"Set up Stage Select"</h1>
        <p class="lede">
            "Stage Select reads only the folders you register. These steps take you from here to your first audit. \
             You can skip any step, and take it later from the screen it names."
        </p>
        <Pending
            pending=Signal::derive(move || load.with(|l| l.pending))
            status="Loading what you've set up…"
            problem=Signal::derive(move || load.with(|l| l.problem.clone()))
        >
            <Show when=move || load.with(|l| l.problem.is_some())>
                <p>
                    <button type="button" on:click=move |_| changed.run(())>"Try again"</button>
                </p>
            </Show>
            <Show when=move || load.with(|l| l.steps)>
                <ol class="steps">
                    {Step::ALL
                        .into_iter()
                        .map(|step| view! { <StepPanel step last skipped changed /> })
                        .collect_view()}
                </ol>
            </Show>
        </Pending>
        <p>
            <A href="/source-sets">"Go to your source sets"</A>
            <span class="note">" You can come back to these steps at /welcome."</span>
        </p>
    }
}

/// One step: what it does, where it stands, and its form while you can take it.
#[component]
fn StepPanel(
    step: Step,
    last: RwSignal<Option<Held>>,
    skipped: RwSignal<Vec<Step>>,
    changed: Callback<()>,
) -> impl IntoView {
    let state = Memo::new(move |_| {
        last.with(|h| {
            h.as_ref()
                .map_or(State::Waiting(""), |h| h.facts().state(step))
        })
    });
    let is_skipped = Memo::new(move |_| skipped.with(|s| s.contains(&step)));
    let status = move || StepStatus::of(state.get(), is_skipped.get());
    let skip = move |_| skipped.update(|s| s.push(step));
    let unskip = move |_| skipped.update(|s| s.retain(|&t| t != step));
    let heading = format!(
        "step-{}",
        Step::ALL.iter().position(|&s| s == step).unwrap_or(0) + 1
    );
    let labelled_by = heading.clone();
    view! {
        <li>
            <section class="panel step" aria-labelledby=labelled_by>
                <h2 id=heading>
                    {step.title()}
                    " "
                    {move || view! { <StatusBadge status=status() /> }}
                </h2>
                <p>{explanation(step)}</p>
                {move || match (state.get(), is_skipped.get()) {
                    (State::Done, _) => view! { <Done step last changed /> }.into_any(),
                    (_, true) => {
                        view! {
                            <p class="note">
                                {format!("You skipped this step. You can take it later from {}.", step.later())}
                                " "
                                <button type="button" class="small" on:click=unskip>
                                    "Show this step"
                                </button>
                            </p>
                        }
                            .into_any()
                    }
                    (State::Waiting(why), _) => {
                        view! {
                            <p class="note">{why}</p>
                            <p>
                                <button type="button" class="small" on:click=skip>"Skip this step"</button>
                            </p>
                        }
                            .into_any()
                    }
                    (State::Ready, _) => {
                        view! {
                            <Ready step last changed />
                            <p>
                                <button type="button" class="small" on:click=skip>"Skip this step"</button>
                            </p>
                        }
                            .into_any()
                    }
                }}
            </section>
        </li>
    }
}

/// What a step is for and what taking it does.
fn explanation(step: Step) -> &'static str {
    match step {
        Step::Root => {
            "A root is a folder Stage Select may work in. Register a read-only root for the folders that hold your \
             games, and a writable root for the library folders it builds. It reads nothing until you register a \
             root, and writes only where you let it: in a writable root, and in the paths of a read-only root you let \
             repairs write to."
        }
        Step::Dat => {
            "A DAT lists every file a set of games should have; an audit checks your files against it. Import a DAT \
             file you have, or fetch the latest from a provider on the DATs screen."
        }
        Step::SourceSet => {
            "A source set is a folder in a read-only root holding the games of one DAT. Registering it reads no file \
             yet."
        }
        Step::Audit => {
            "An audit reads each file of a source set and checks it against its DAT. It never changes your files."
        }
        Step::Library => {
            "A library folder is where Stage Select builds the games you select, for your front end to play. It \
             needs a writable root. Your source set stays as it is."
        }
    }
}

/// A step you can take now: its form, or a link to the screen that takes it.
#[component]
fn Ready(step: Step, last: RwSignal<Option<Held>>, changed: Callback<()>) -> impl IntoView {
    let names = Signal::derive(move || {
        last.with(|h| {
            h.as_ref()
                .map(|h| {
                    h.sets
                        .iter()
                        .map(|s| s.set.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    match step {
        Step::Root => view! { <RegisterRoot changed /> }.into_any(),
        Step::Dat => view! {
            <Upload on_done=changed />
            <p>
                <A href="/dats">"Fetch DATs from a provider on the DATs screen"</A>
            </p>
        }
        .into_any(),
        Step::SourceSet => view! { <RegisterSet changed /> }.into_any(),
        Step::Audit => view! { <AuditSets names /> }.into_any(),
        Step::Library => view! {
            <ul>
                {move || {
                    names
                        .get()
                        .into_iter()
                        .map(|name| {
                            view! {
                                <li>
                                    <A href=new_library_href(&name)>
                                        {format!("Make a library folder for {name}")}
                                    </A>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
        }
        .into_any(),
    }
}

/// A button to audit each Source Set, off while its request is under way or a job runs on the Source Set.
#[component]
fn AuditSets(names: Signal<Vec<String>>) -> impl IntoView {
    let live = use_live();
    let busy = Memo::new(move |_| {
        live.book
            .with(|b| running(b.newest_first().iter().map(|l| &l.job)))
    });
    let audit = Action::new_local(move |name: &String| {
        let name = name.clone();
        async move {
            let job = queue_audit(&name).await?;
            let message = format!("Queued audit job #{} for {name}.", job.id);
            // The stream's event for the job may come after this response: until it does, the book wouldn't know the
            // source set is busy, and the button would come back on.
            live.book.update(|b| {
                b.accept(job);
            });
            Ok(message)
        }
    });
    view! {
        <ActionPending action=audit status="Queuing the audit…">
            <div class="actions">
                {move || {
                    names
                        .get()
                        .into_iter()
                        .map(|name| {
                            let label = format!("Audit {name}");
                            let running = name.clone();
                            view! {
                                <button
                                    type="button"
                                    class="primary"
                                    disabled=move || {
                                        audit.pending().get() || busy.with(|b| b.contains(&running))
                                    }
                                    on:click=move |_| {
                                        audit.dispatch(name.clone());
                                    }
                                >
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()
                }}
            </div>
            {move || {
                audit
                    .value()
                    .get()
                    .and_then(Result::ok)
                    .map(|message| {
                        view! {
                            <p class="done" role="status">
                                {message}
                                " This step is done once the audit finishes. You can follow it under Activity."
                            </p>
                        }
                    })
            }}
        </ActionPending>
    }
}

/// A step you've taken: what it left, and for a Root, the form to register another.
#[component]
fn Done(step: Step, last: RwSignal<Option<Held>>, changed: Callback<()>) -> impl IntoView {
    let held =
        move |f: fn(&Held) -> String| move || last.with(|h| h.as_ref().map(f).unwrap_or_default());
    match step {
        Step::Root => {
            let again = RwSignal::new(false);
            view! {
                <p class="done">
                    {held(|h| {
                        let roots = h
                            .roots
                            .iter()
                            .map(|r| {
                                let access = if r.writable { "writable" } else { "read-only" };
                                format!("{} ({}, {access})", r.name, r.path)
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        format!("Your roots: {roots}. You manage them under Settings, then Roots.")
                    })}
                </p>
                <Show
                    when=move || again.get()
                    fallback=move || {
                        view! {
                            <button type="button" on:click=move |_| again.set(true)>
                                "Register another root"
                            </button>
                        }
                    }
                >
                    <RegisterRoot changed />
                </Show>
            }
            .into_any()
        }
        Step::Dat => view! {
            <p class="done">
                {held(|h| {
                    let dats = h.dats.iter().map(|d| d.dat.clone()).collect::<Vec<_>>().join(", ");
                    format!("Your DATs: {dats}.")
                })}
            </p>
        }
        .into_any(),
        Step::SourceSet => view! {
            <p class="done">
                {held(|h| {
                    let sets = h.sets.iter().map(|s| s.set.name.clone()).collect::<Vec<_>>().join(", ");
                    format!("Your source sets: {sets}.")
                })}
            </p>
        }
        .into_any(),
        Step::Audit => view! {
            <p class="done">"Your first audit is done. Open it to see what it found:"</p>
            <ul>
                {move || {
                    let audited: Vec<(String, i64)> = last.with(|h| {
                        h.iter()
                            .flat_map(|h| &h.sets)
                            .filter_map(|s| s.latest.as_ref().map(|a| (s.set.name.clone(), a.id)))
                            .collect()
                    });
                    audited
                        .into_iter()
                        .map(|(name, id)| {
                            view! {
                                <li>
                                    <A href=format!("/audits/{id}")>{format!("Audit #{id} of {name}")}</A>
                                    " · "
                                    <A href=set_href(&name)>{format!("Open {name}")}</A>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
        }
        .into_any(),
        Step::Library => view! {
            <p class="done">
                {held(|h| format!("You have {} library folders. ", h.libraries))}
                <A href="/libraries">"Open your library folders"</A>
            </p>
        }
        .into_any(),
    }
}

/// The form to register a Root, with the folders the service can see under the common mount points to choose from.
#[component]
fn RegisterRoot(changed: Callback<()>) -> impl IntoView {
    let suggested = LocalResource::new(|| api::get::<FolderSuggestions>("/folder-suggestions"));
    let draft = RwSignal::new(RootDraft::default());
    let said = RwSignal::new(None::<String>);
    let done = Callback::new(move |(message, _): (String, Vec<String>)| {
        said.set(Some(message));
        draft.set(RootDraft::default());
        changed.run(());
    });
    let choose = move |path: String| {
        draft.update(|d| {
            d.name = root_name(&path);
            d.path = path;
        })
    };
    view! {
        {move || {
            suggested
                .get()
                .map(|found| match found {
                    Ok(found) if found.folders.is_empty() => {
                        view! {
                            <p class="note">
                                "The service sees no folder under /data, /mnt, /media or /srv. Type the path of a folder it can see."
                            </p>
                        }
                            .into_any()
                    }
                    Ok(found) => {
                        view! {
                            <p>"Folders the service can see. Choose one to fill in the form, then check it:"</p>
                            <ul class="suggestions">
                                {found
                                    .folders
                                    .into_iter()
                                    .map(|f| {
                                        let path = f.path.clone();
                                        let can = if f.writable {
                                            "the service can write to it"
                                        } else {
                                            "the service can only read it"
                                        };
                                        view! {
                                            <li>
                                                <button
                                                    type="button"
                                                    class="small"
                                                    on:click=move |_| choose(path.clone())
                                                >
                                                    "Use\u{a0}"
                                                    <code>{f.path.clone()}</code>
                                                </button>
                                                <span class="note small">{format!(" {can}")}</span>
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                            {found
                                .more
                                .then(|| {
                                    view! {
                                        <p class="note small">
                                            "Some folders aren't listed: a mount point holds more than the service lists. Type the path if yours isn't here."
                                        </p>
                                    }
                                })}
                        }
                            .into_any()
                    }
                    Err(message) => {
                        view! {
                            <p class="note">
                                {format!("The service couldn't list the folders it sees: {message}. Type the path of a folder.")}
                            </p>
                        }
                            .into_any()
                    }
                })
        }}
        <div class="step-form">
            <RootForm was=None done draft />
        </div>
        {move || said.get().map(|message| view! { <p class="done" role="status">{message}</p> })}
    }
}

/// The button that opens the form to register a Source Set.
#[component]
fn RegisterSet(changed: Callback<()>) -> impl IntoView {
    let open = RwSignal::new(false);
    view! {
        <button type="button" class="primary" on:click=move |_| open.set(true)>
            "Register a source set…"
        </button>
        <Drawer open title="Register a source set">
            <Register on_added=Callback::new(move |_: String| {
                open.set(false);
                changed.run(());
            }) />
        </Drawer>
    }
}
