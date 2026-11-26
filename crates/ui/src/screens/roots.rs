//! The Roots: the folders the Service works in. You register, edit and unregister them here; each change applies at
//! once, without a restart.

use leptos::prelude::*;
use leptos_router::components::A;
use stage_select_api::permissions::PermissionSettings;
use stage_select_api::roots::{FolderSuggestions, RootEdited, RootKind, RootSpec};
use stage_select_api::settings::{SettingsUpdate, SettingsView};
use stage_select_api::{Root, SourceSetRecord};
use std::collections::BTreeMap;

use crate::api;
use crate::model::when;
use crate::parts::choice::Choice;
use crate::parts::folder_picker::{Create, FolderPicker};
use crate::parts::help::Help;
use crate::parts::message::{Message, MessageKind, Problem, Said};
use crate::parts::overlay::Drawer;
use crate::parts::pending::{ActionPending, Pending};
use crate::permissions_model::{PolicyDraft, PolicyField};
use crate::roots_model::{
    RootDraft, access, changeable_line, changeable_sets, kind_label, kind_note, voided_lines,
};
use crate::screens::source_sets::set_href;

/// Where each root's ceilings are set: the job pace, under Operations.
pub(crate) const CEILINGS: &str = "/settings/operations#job-pace";

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
    let sets = LocalResource::new(move || {
        refresh.get();
        api::get::<Vec<SourceSetRecord>>("/source-sets")
    });
    let sets = Signal::derive(move || sets.get().and_then(Result::ok).unwrap_or_default());
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
            "The folders the service works in. You set each root's ceilings under "
            <A href=CEILINGS>"Operations"</A> "."
        </p>
        <div class="actions">
            <button type="button" on:click=move |_| show(Open::Register)>"Register a root…"</button>
        </div>
        {move || {
            said.get()
                .map(|(message, voided)| {
                    view! {
                        <Message kind=MessageKind::DONE>{message}</Message>
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
                                            <th scope="col"><span class="label">"Source sets it may change"</span></th>
                                            <th scope="col"><span class="label">"Changed"</span></th>
                                            <th scope="col"><span class="label">"Actions"</span></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {roots
                                            .into_iter()
                                            .map(|root| view! { <RootRow root sets show /> })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }
                            .into_any()
                    })
            }}
        </Pending>
        <PolicyForm roots=Signal::derive(move || roots.get().and_then(Result::ok).unwrap_or_default()) />
        <Help summary="What a root is">
            <p>
                "A root is a folder the service may work in. Register a root before you register anything in it. Its \
                 kind says what it holds and whether Stage Select may write to it: a source sets root for the \
                 folders that hold your games, read-only but for the source sets you let Stage Select change, and a \
                 play library root for the library folders it builds, writable. A change applies at once, without a \
                 restart."
            </p>
        </Help>
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
fn RootRow(
    root: Root,
    sets: Signal<Vec<SourceSetRecord>>,
    show: impl Fn(Open) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let name = root.name.clone();
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
                {move || sets.with(|sets| set_links(&changeable_sets(&name, sets)))}
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

/// A link to each of the Source Sets `names`, comma-separated.
fn set_links(names: &[String]) -> impl IntoView + use<> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let href = set_href(name);
            let name = name.clone();
            view! {
                {(i > 0).then_some(", ")}
                <A href=href>{name}</A>
            }
        })
        .collect_view()
}

/// The form to register a Root, or to edit `was`. `draft` holds the form as typed, when the page fills it in too.
#[component]
pub(crate) fn RootForm(
    was: Option<Root>,
    done: Callback<(String, Vec<String>)>,
    #[prop(optional)] draft: Option<RwSignal<RootDraft>>,
) -> impl IntoView {
    let draft =
        draft.unwrap_or_else(|| RwSignal::new(was.as_ref().map(RootDraft::of).unwrap_or_default()));
    let suggested = LocalResource::new(|| api::get::<FolderSuggestions>("/folder-suggestions"));
    let editing = was.is_some();
    // The source sets an edited sources root holds that Stage Select may change: each switches its own.
    let holder = was
        .as_ref()
        .filter(|r| r.kind == RootKind::Sources)
        .map(|r| r.name.clone());
    let sets = LocalResource::new(move || {
        let holder = holder.clone();
        async move {
            match holder {
                Some(root) => api::get::<Vec<SourceSetRecord>>("/source-sets")
                    .await
                    .ok()
                    .map(|sets| changeable_sets(&root, &sets)),
                None => None,
            }
        }
    });
    let changeable = Signal::derive(move || sets.get().flatten());
    let was = StoredValue::new(was);
    let problem = RwSignal::new(None::<String>);
    let request = Action::new_local(move |spec: &RootSpec| {
        let (spec, editing) = (spec.clone(), editing);
        async move {
            if editing {
                let path = format!("/roots/{}", api::segment(&spec.name));
                api::put::<_, RootEdited>(&path, &spec).await.map(|edited| {
                    (
                        format!("Saved the root {}.", edited.root.name),
                        voided_lines(&edited.voided),
                    )
                })
            } else {
                api::post::<_, Root>("/roots", &spec)
                    .await
                    .map(|root| (format!("Registered the root {}.", root.name), Vec::new()))
            }
        }
    });
    // The effect goes with the form: once the form is gone, an answer still on its way calls no one.
    Effect::new(move || {
        if let Some(Ok(outcome)) = request.value().get() {
            done.run(outcome);
        }
    });
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        match was.with_value(|w| draft.with_untracked(|d| d.spec(w.as_ref()))) {
            Ok(spec) => {
                problem.set(None);
                request.dispatch_local(spec);
            }
            Err(message) => {
                request.clear();
                problem.set(Some(message));
            }
        }
    };
    let kind_choice = move |kind: RootKind| {
        view! {
            <Choice
                radio="root-kind"
                checked=Signal::derive(move || draft.with(|d| d.kind == kind))
                on_change=Callback::new(move |_| draft.update(|d| d.kind = kind))
                note=move || kind_note(kind)
            >
                {kind_label(kind)}
            </Choice>
        }
    };
    view! {
        <ActionPending
            action=request
            status=if editing { "Saving the root…" } else { "Registering the root…" }
        >
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
            <div class="path-group">
                <label for="root-path">"Path"</label>
                <div class="path-field">
                    <input
                        id="root-path"
                        type="text"
                        class="wide"
                        placeholder="/srv/roms"
                        list="root-path-suggestions"
                        prop:value=move || draft.with(|d| d.path.clone())
                        on:input=move |ev| {
                            let value = event_target_value(&ev);
                            draft.update(|d| d.path = value);
                        }
                    />
                    <FolderPicker
                        field="the root's path"
                        value=Signal::derive(move || draft.with(|d| d.path.clone()))
                        on_choose=move |path: String| draft.update(|d| d.choose_path(path))
                        create=(!editing).then(|| Create {
                            label: "Create this folder",
                            run: Callback::new(move |path: String| draft.update(|d| {
                                d.choose_path(path);
                                d.create = true;
                            })),
                        })
                    />
                </div>
            </div>
            {if editing {
                view! { <p class="note small">"The folder must already exist on the service's host."</p> }.into_any()
            } else {
                view! {
                    <Choice
                        checked=Signal::derive(move || draft.with(|d| d.create))
                        on_change=Callback::new(move |on| draft.update(|d| d.create = on))
                        note=|| "If the folder doesn't exist yet, Stage Select creates it, and any missing folders above it, as the service's user."
                    >
                        "Create this folder"
                    </Choice>
                }
                .into_any()
            }}
            <fieldset>
                <legend>"Kind"</legend>
                {RootKind::ALL.into_iter().map(kind_choice).collect_view()}
            </fieldset>
            {move || changeable.get().map(|names| view! {
                <p class="note small">
                    {changeable_line(names.len())}
                    " "
                    {set_links(&names)}
                </p>
                <p class="note small">
                    "You let Stage Select change a source set, or stop it, on the source set's overview."
                </p>
            })}
            {editing.then(|| view! {
                <p class="note small">
                    "You can change its kind only while nothing is registered in it, and its path only while no job runs on it and no approved plan writes to it."
                </p>
            })}
            <button type="submit" class="primary" disabled=request.pending()>
                {if editing { "Save the root" } else { "Register the root" }}
            </button>
        </form>
        </ActionPending>
        <datalist id="root-path-suggestions">
            {move || {
                suggested
                    .get()
                    .and_then(Result::ok)
                    .map(|s| {
                        s.folders
                            .into_iter()
                            .map(|f| view! { <option value=f.path></option> })
                            .collect_view()
                    })
            }}
        </datalist>
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
    let request = Action::new_local(|name: &String| {
        let name = name.clone();
        async move {
            let path = format!("/roots/{}/unregistration", api::segment(&name));
            api::post_empty(&path, &())
                .await
                .map(|()| (format!("Unregistered the root {name}."), Vec::new()))
        }
    });
    // As in `RootForm`: the effect goes with the confirmation, so a late answer calls no one.
    Effect::new(move || {
        if let Some(Ok(outcome)) = request.value().get() {
            done.run(outcome);
        }
    });
    let name = StoredValue::new(root.name.clone());
    let unregister = move |_| {
        request.dispatch_local(name.get_value());
    };
    view! {
        <ActionPending action=request status="Unregistering the root…">
        <p>
            {format!(
                "Unregister the root {}? Its files stay where they are, and the service no longer reads or writes there. You can't unregister it while anything is registered in it, it holds kept files you can still delete, or work that hasn't finished uses it.",
                root.name
            )}
        </p>
        <div class="form">
            <button type="button" class="primary" disabled=request.pending() on:click=unregister>
                "Unregister"
            </button>
            <button type="button" on:click=move |_| cancel()>"Cancel"</button>
        </div>
        </ActionPending>
    }
}

/// Which policy a row of the permission policy's table sets: every root's, or one root's own.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PolicyRow {
    Every,
    Root(String),
}

impl PolicyRow {
    /// What the row sets, as its fields' labels name it.
    fn name(&self) -> String {
        match self {
            PolicyRow::Every => "every root".into(),
            PolicyRow::Root(name) => name.clone(),
        }
    }
}

/// The permission policy: one for every root, and each root's own in its place, as typed.
#[derive(Debug, Clone, Default, PartialEq)]
struct PolicyDrafts {
    every: PolicyDraft,
    roots: BTreeMap<String, PolicyDraft>,
}

impl PolicyDrafts {
    /// The saved settings, as the form shows them.
    fn of(settings: &PermissionSettings) -> PolicyDrafts {
        PolicyDrafts {
            every: PolicyDraft::of(settings.global),
            roots: settings
                .roots
                .iter()
                .map(|(name, p)| (name.clone(), PolicyDraft::of(Some(*p))))
                .collect(),
        }
    }

    /// What `row` sets, as typed; empty when nothing is.
    fn get(&self, row: &PolicyRow) -> PolicyDraft {
        match row {
            PolicyRow::Every => self.every.clone(),
            PolicyRow::Root(name) => self.roots.get(name).cloned().unwrap_or_default(),
        }
    }

    /// What `row` sets, to type into.
    fn get_mut(&mut self, row: &PolicyRow) -> &mut PolicyDraft {
        match row {
            PolicyRow::Every => &mut self.every,
            PolicyRow::Root(name) => self.roots.entry(name.clone()).or_default(),
        }
    }

    /// The settings typed for every root and for each of `shown`, or what is wrong with them. A root's own policy
    /// that the form doesn't show, as one unregistered since it loaded, is never saved.
    fn read(&self, shown: &[String]) -> Result<PermissionSettings, String> {
        let mut roots = BTreeMap::new();
        for (name, draft) in self.roots.iter().filter(|(name, _)| shown.contains(name)) {
            if let Some(policy) = draft.read(name)? {
                roots.insert(name.clone(), policy);
            }
        }
        Ok(PermissionSettings {
            global: self.every.read("every root")?,
            roots,
        })
    }
}

/// The permission policy's form: the mode and group you want for files and folders, for every root and each root's
/// own in its place.
#[component]
fn PolicyForm(roots: Signal<Vec<Root>>) -> impl IntoView {
    let settings = LocalResource::new(|| api::get::<SettingsView>("/settings"));
    let draft = RwSignal::new(PolicyDrafts::default());
    Effect::new(move || {
        if let Some(Ok(settings)) = settings.get() {
            draft.set(PolicyDrafts::of(&settings.permissions));
        }
    });
    let said = RwSignal::new(None::<Result<String, String>>);
    let save = move |_| {
        let shown: Vec<String> = roots.with_untracked(|r| r.iter().map(|r| r.name.clone()).collect());
        let permissions = match draft.with_untracked(|d| d.read(&shown)) {
            Ok(p) => p,
            Err(problem) => {
                said.set(Some(Err(problem)));
                return;
            }
        };
        let update = SettingsUpdate {
            permissions: Some(permissions),
            ..Default::default()
        };
        leptos::task::spawn_local(async move {
            said.set(Some(
                match api::patch::<_, SettingsView>("/settings", &update).await {
                    Ok(now) => {
                        draft.set(PolicyDrafts::of(&now.permissions));
                        Ok("Saved the permission policy. Check a folder to compare it.".into())
                    }
                    Err(problem) => Err(format!("Couldn't save the permission policy: {problem}")),
                },
            ));
        });
    };
    let field = move |row: PolicyRow, which: PolicyField| {
        let row = StoredValue::new(row);
        let (label, placeholder) = (which.label(), which.placeholder());
        view! {
            <input
                type="text"
                inputmode="numeric"
                size="6"
                aria-label=move || format!("{}: {label}", row.with_value(PolicyRow::name))
                placeholder=placeholder
                prop:value=move || draft.with(|d| which.of(&d.get(&row.get_value())).clone())
                on:input=move |ev| {
                    let value = event_target_value(&ev);
                    draft.update(|d| *which.of_mut(d.get_mut(&row.get_value())) = value);
                }
            />
        }
    };
    let row = move |row: PolicyRow, label: String| {
        view! {
            <tr>
                <th scope="row">{label}</th>
                <td>{field(row.clone(), PolicyField::FileMode)}</td>
                <td>{field(row.clone(), PolicyField::FolderMode)}</td>
                <td>{field(row, PolicyField::Group)}</td>
            </tr>
        }
    };
    view! {
        <h2 id="permission-policy">"Permission policy"</h2>
        <div class="table-scroll">
            <table aria-label="Permission policy">
                <thead>
                    <tr>
                        <th scope="col"><span class="label">"Root"</span></th>
                        <th scope="col"><span class="label">"File mode"</span></th>
                        <th scope="col"><span class="label">"Folder mode"</span></th>
                        <th scope="col"><span class="label">"Group"</span></th>
                    </tr>
                </thead>
                <tbody>
                    {row(PolicyRow::Every, "Every root".into())}
                    {move || {
                        roots
                            .get()
                            .into_iter()
                            .map(|root| row(PolicyRow::Root(root.name.clone()), root.name))
                            .collect_view()
                    }}
                </tbody>
            </table>
        </div>
        <div class="actions">
            <button type="button" on:click=save>"Save the permission policy"</button>
        </div>
        <Said said />
        <Help summary="Leave a root's policy empty to use every root's, and every root's empty to keep the host's defaults.">
            <p>
                "Stage Select gives every file and folder it writes your policy's mode, and its group when you give one by number, as id -g shows it. A folder mode such as 2770 includes setgid, so new files take the folder's group."
            </p>
            <p>
                "Each check of a source set or library folder compares what it sees with your policy, and its Permissions tab lists what differs. That never makes a source set stale or a library folder drift. Stage Select restores what it owns through a plan you approve, changes a group only to one it is in, and for the rest gives you the command to run on the host. A source set it may not change is only reported."
            </p>
        </Help>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::permissions::{Mode, PermissionPolicy};

    #[test]
    fn the_policy_form_saves_a_policy_only_for_a_root_it_shows() {
        let policy = PermissionPolicy {
            file_mode: Mode(0o660),
            folder_mode: Mode(0o2770),
            group: None,
        };
        let saved = PermissionSettings {
            global: None,
            roots: [("gone".to_owned(), policy), ("sets".to_owned(), policy)].into(),
        };

        let read = PolicyDrafts::of(&saved).read(&["sets".to_owned()]).unwrap();

        assert_eq!(read.roots.keys().collect::<Vec<_>>(), ["sets"]);
    }
}
