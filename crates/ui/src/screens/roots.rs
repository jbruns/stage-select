//! The Roots: the folders the Service works in. You register, edit and unregister them here; each change applies at
//! once, without a restart.

use leptos::prelude::*;
use title_select_api::Root;
use title_select_api::roots::{RootEdited, RootKind};

use crate::api;
use crate::app::Problem;
use crate::model::when;
use crate::parts::overlay::Drawer;
use crate::parts::pending::Pending;
use crate::roots_model::{RootDraft, access, kind_label, kind_note, voided_lines};

/// What the drawer shows: a form to register a Root or edit one, or the confirmation to unregister one.
#[derive(Debug, Clone, PartialEq)]
enum Open {
    Register,
    Edit(Root),
    Unregister(Root),
}

impl Open {
    fn title(&self) -> String {
        match self {
            Open::Register => "Register a root".into(),
            Open::Edit(root) => format!("Edit the root {}", root.name),
            Open::Unregister(root) => format!("Unregister the root {}", root.name),
        }
    }
}

#[component]
pub fn Roots() -> impl IntoView {
    let refresh = RwSignal::new(0u64);
    let roots = LocalResource::new(move || {
        refresh.get();
        api::get::<Vec<Root>>("/roots")
    });
    let problem = Signal::derive(move || roots.get().and_then(Result::err));
    let open = RwSignal::new(None::<Open>);
    let drawer = RwSignal::new(false);
    Effect::new(move || {
        if !drawer.get() {
            open.set(None);
        }
    });
    let show = move |what: Open| {
        open.set(Some(what));
        drawer.set(true);
    };
    // What the last change did, and the approved plans it voided.
    let said = RwSignal::new(None::<(String, Vec<String>)>);
    let done = Callback::new(move |outcome: (String, Vec<String>)| {
        said.set(Some(outcome));
        drawer.set(false);
        refresh.update(|n| *n += 1);
    });
    view! {
        <h1>"Roots"</h1>
        <p class="lede">
            "The folders the service works in. Register a root before you register anything in it. A change applies at once. You set each root's ceilings under operations."
        </p>
        <div class="actions">
            <button type="button" on:click=move |_| show(Open::Register)>"Register a root…"</button>
        </div>
        {move || {
            said.get()
                .map(|(message, voided)| {
                    view! {
                        <p class="done" role="status">{message}</p>
                        {(!voided.is_empty())
                            .then(|| {
                                view! {
                                    <p>
                                        "It may no longer write where these approved plans would have, so they were voided:"
                                    </p>
                                    <ul>
                                        {voided
                                            .into_iter()
                                            .map(|line| view! { <li>{line}</li> })
                                            .collect_view()}
                                    </ul>
                                }
                            })}
                    }
                })
        }}
        <Pending pending=Signal::derive(move || roots.get().is_none()) status="Loading roots…" problem>
            {move || {
                roots
                    .get()
                    .and_then(Result::ok)
                    .map(|roots| {
                        if roots.is_empty() {
                            return view! {
                                <p>"You have no roots yet. Register one to start."</p>
                            }
                                .into_any();
                        }
                        view! {
                            <div class="table-scroll">
                                <table aria-label="Roots">
                                    <thead>
                                        <tr>
                                            <th scope="col"><span class="label">"Name"</span></th>
                                            <th scope="col"><span class="label">"Path"</span></th>
                                            <th scope="col"><span class="label">"Kind"</span></th>
                                            <th scope="col"><span class="label">"Access"</span></th>
                                            <th scope="col"><span class="label">"Writable paths"</span></th>
                                            <th scope="col"><span class="label">"Changed"</span></th>
                                            <th scope="col"><span class="label">"Actions"</span></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {roots
                                            .into_iter()
                                            .map(|root| view! { <RootRow root show /> })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }
                            .into_any()
                    })
            }}
        </Pending>
        <Drawer open=drawer title=Signal::derive(move || open.with(|o| o.as_ref().map(Open::title).unwrap_or_default()))>
            {move || match open.get() {
                Some(Open::Register) => view! { <RootForm was=None done /> }.into_any(),
                Some(Open::Edit(root)) => view! { <RootForm was=Some(root) done /> }.into_any(),
                Some(Open::Unregister(root)) => {
                    view! { <UnregisterRoot root done cancel=move || drawer.set(false) /> }.into_any()
                }
                None => ().into_any(),
            }}
        </Drawer>
    }
}

#[component]
fn RootRow(root: Root, show: impl Fn(Open) + Copy + Send + Sync + 'static) -> impl IntoView {
    let path = root.path.clone();
    let changed = format!("{}, by {}", when(&root.updated_at), root.updated_by);
    let edit = root.clone();
    let unregister = root.clone();
    view! {
        <tr>
            <td>{root.name.clone()}</td>
            <td>
                <code title=path>{root.path.clone()}</code>
            </td>
            <td>{kind_label(root.kind)}</td>
            <td>{access(&root)}</td>
            <td>
                {root
                    .writable_paths
                    .iter()
                    .map(|p| view! { <code>{p.clone()}</code> " " })
                    .collect_view()}
            </td>
            <td>{changed}</td>
            <td>
                <button type="button" class="small" on:click=move |_| show(Open::Edit(edit.clone()))>
                    "Edit…"
                </button>
                " "
                <button
                    type="button"
                    class="small"
                    on:click=move |_| show(Open::Unregister(unregister.clone()))
                >
                    "Unregister…"
                </button>
            </td>
        </tr>
    }
}

/// The form to register a Root, or to edit `was`.
#[component]
fn RootForm(was: Option<Root>, done: Callback<(String, Vec<String>)>) -> impl IntoView {
    let draft = RwSignal::new(was.as_ref().map(RootDraft::of).unwrap_or_default());
    let editing = was.is_some();
    let was = StoredValue::new(was);
    let busy = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let spec = match was.with_value(|w| draft.with_untracked(|d| d.spec(w.as_ref()))) {
            Ok(spec) => spec,
            Err(message) => return problem.set(Some(message)),
        };
        busy.set(true);
        problem.set(None);
        let editing = was.get_value().is_some();
        leptos::task::spawn_local(async move {
            let outcome = if editing {
                let path = format!("/roots/{}", api::segment(&spec.name));
                api::put::<_, RootEdited>(&path, &spec).await.map(|edited| {
                    (
                        format!("Saved the root {}.", edited.root.name),
                        voided_lines(&edited),
                    )
                })
            } else {
                api::post::<_, Root>("/roots", &spec)
                    .await
                    .map(|root| (format!("Registered the root {}.", root.name), Vec::new()))
            };
            busy.set(false);
            match outcome {
                Ok(outcome) => done.run(outcome),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let kind_choice = move |kind: RootKind| {
        view! {
            <p>
                <label class="inline">
                    <input
                        type="radio"
                        name="root-kind"
                        prop:checked=move || draft.with(|d| d.kind == kind)
                        on:change=move |_| draft.update(|d| d.kind = kind)
                    />
                    {kind_label(kind)}
                </label>
                <span class="note small">" " {kind_note(kind)}</span>
            </p>
        }
    };
    view! {
        <form class="form" on:submit=save>
            <label class="stacked" for="root-name">"Name"</label>
            <input
                id="root-name"
                type="text"
                disabled=editing
                prop:value=move || draft.with(|d| d.name.clone())
                on:input=move |ev| {
                    let value = event_target_value(&ev);
                    draft.update(|d| d.name = value);
                }
            />
            {editing.then(|| view! {
                <p class="note small">"A root's name can't change: what is registered in it refers to it by name."</p>
            })}
            <label class="stacked" for="root-path">"Path"</label>
            <input
                id="root-path"
                type="text"
                class="wide"
                placeholder="/srv/roms"
                prop:value=move || draft.with(|d| d.path.clone())
                on:input=move |ev| {
                    let value = event_target_value(&ev);
                    draft.update(|d| d.path = value);
                }
            />
            <p class="note small">"The folder must already exist on the service's host."</p>
            <fieldset>
                <legend>"Kind"</legend>
                {RootKind::ALL.into_iter().map(kind_choice).collect_view()}
            </fieldset>
            <p>
                <label class="inline">
                    <input
                        type="checkbox"
                        disabled=move || draft.with(RootDraft::writable_fixed)
                        prop:checked=move || draft.with(RootDraft::is_writable)
                        on:change=move |ev| {
                            let on = event_target_checked(&ev);
                            draft.update(|d| d.writable = on);
                        }
                    />
                    "Writable"
                </label>
            </p>
            <Show when=move || !draft.with(RootDraft::is_writable)>
                <label class="stacked" for="root-writable-paths">
                    "Paths repairs may write to, in the root, one to a line"
                </label>
                <textarea
                    id="root-writable-paths"
                    rows="3"
                    prop:value=move || draft.with(|d| d.writable_paths.clone())
                    on:input=move |ev| {
                        let value = event_target_value(&ev);
                        draft.update(|d| d.writable_paths = value);
                    }
                ></textarea>
            </Show>
            {editing.then(|| view! {
                <p class="note small">
                    "Making it less writable voids each approved plan, not yet started, that would write where it no longer may. You can't change its path or whether it's writable while a job runs on it."
                </p>
            })}
            <button type="submit" class="primary" disabled=busy>
                {if editing { "Save the root" } else { "Register the root" }}
            </button>
        </form>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// The confirmation to unregister `root`. The Service refuses while anything is registered in it, and says what.
#[component]
fn UnregisterRoot(
    root: Root,
    done: Callback<(String, Vec<String>)>,
    cancel: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let name = StoredValue::new(root.name.clone());
    let busy = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let unregister = move |_| {
        busy.set(true);
        problem.set(None);
        let name = name.get_value();
        leptos::task::spawn_local(async move {
            let path = format!("/roots/{}/unregistration", api::segment(&name));
            let result = api::post_empty(&path, &()).await;
            busy.set(false);
            match result {
                Ok(()) => done.run((format!("Unregistered the root {name}."), Vec::new())),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <p>
            {format!(
                "Unregister the root {}? Its files stay where they are, and the service no longer reads or writes there. You can't unregister it while anything is registered in it, it holds kept files you can still delete, or work that hasn't finished uses it.",
                root.name
            )}
        </p>
        <div class="form">
            <button type="button" class="primary" disabled=busy on:click=unregister>
                "Unregister"
            </button>
            <button type="button" on:click=move |_| cancel()>"Cancel"</button>
        </div>
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}
