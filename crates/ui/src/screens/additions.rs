//! Additions: each system's additions set, the additions in it, and the fields every plan that adds one
//! asks for.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use stage_select_api::Dat;
use stage_select_api::additions::{
    Addition, AdditionState, AdditionsCheck, AdditionsFinding, AdditionsSet, EditAddition,
    NewAddition,
};

use crate::api;
use crate::app::Loading;
use crate::events::use_live;
use crate::kept_model::KeptPlace;
use crate::model::{short_sha, when};
use crate::parts::legend::{LegendStrip, present_looks};
use crate::parts::list::{Clip, Column, List, ListRow, Width, last_good_rows};
use crate::parts::message::{Live, Problem};
use crate::parts::status::{Status, StatusBadge, StatusIcon};
use crate::screens::kept::KeptFiles;
use crate::screens::lifecycle::{
    AdditionsSetAsideAction, LifecycleSection, Paths, UnregisterAction,
};

/// The name suggested for an addition made from the file at `path`: its file name, without folders or extension.
pub fn suggested_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => file.to_owned(),
    }
}

/// What you add a file as: its system, suggested when one is known, and its name, suggested from the file's.
pub fn new_addition(path: &str, system: Option<&str>) -> NewAddition {
    NewAddition {
        system: system.unwrap_or_default().to_owned(),
        name: suggested_name(path),
        parent: None,
    }
}

pub fn finding_label(finding: AdditionsFinding) -> &'static str {
    match finding {
        AdditionsFinding::Intact => "Intact",
        AdditionsFinding::Missing => "Missing",
        AdditionsFinding::Changed => "Changed",
        AdditionsFinding::Retired => "Retired, still there",
        AdditionsFinding::Unknown => "Unknown file",
        AdditionsFinding::Other => "Not a file",
    }
}

/// The system, name and parent game to add a file as an addition under, edited in place.
#[component]
pub fn AdditionFields(
    value: RwSignal<NewAddition>,
    /// Holds the fields as they are, such as while the plan they belong to is approved.
    #[prop(optional, into)]
    held: Signal<bool>,
) -> impl IntoView {
    let dats = LocalResource::new(|| async { api::get::<Vec<Dat>>("/dats").await });
    let list = format!("systems-{}", js_sys::Math::random().to_bits());
    let options = move || {
        dats.get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(|d| view! { <option value=d.dat.clone()></option> })
            .collect_view()
    };
    view! {
        <span class="addition-fields">
            <label>
                "System "
                <input
                    type="text"
                    list=list.clone()
                    required
                    placeholder="provider/DAT name"
                    prop:value=move || value.with(|v| v.system.clone())
                    disabled=held
                    on:input=move |ev| {
                        if !held.get_untracked() {
                            value.update(|v| v.system = event_target_value(&ev))
                        }
                    }
                />
                <datalist id=list>{options}</datalist>
            </label>
            " "
            <label>
                "Name "
                <input
                    type="text"
                    required
                    prop:value=move || value.with(|v| v.name.clone())
                    disabled=held
                    on:input=move |ev| {
                        if !held.get_untracked() {
                            value.update(|v| v.name = event_target_value(&ev))
                        }
                    }
                />
            </label>
            " "
            <label>
                "Parent game, if any "
                <input
                    type="text"
                    prop:value=move || value.with(|v| v.parent.clone().unwrap_or_default())
                    disabled=held
                    on:input=move |ev| {
                        if held.get_untracked() {
                            return;
                        }
                        let parent = event_target_value(&ev);
                        value.update(|v| v.parent = (!parent.trim().is_empty()).then_some(parent))
                    }
                />
            </label>
        </span>
    }
}

/// Every additions set: every addition in one list, then each set's check and kept files.
#[component]
pub fn Additions() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let sets = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        async { api::get::<Vec<AdditionsSet>>("/additions-sets").await }
    });
    let additions = last_good_rows(move || {
        sets.get()
            .map(|r| r.map(|sets| sets.into_iter().flat_map(|s| s.additions).collect()))
    });
    view! {
        <h1>"Additions"</h1>
        <p class="lede">
            "Files no DAT knows that you chose to keep, each known by its own hashes and a name you give it, in an \
            additions set per system. A selection profile that opts in for a system builds them alongside the \
            source set. Add one from an ingest folder, a repair of a source set or a cleanup of a library folder. \
            An additions job publishes each one after you approve it, and moves the file of one you retire into \
            the additions set's kept files."
        </p>
        {move || match sets.get() {
            None => view! { <Loading what="the additions sets" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <AdditionsList additions refresh />
        {move || {
            sets.get()
                .and_then(Result::ok)
                .map(|sets| {
                    sets.into_iter()
                        .map(|set| view! { <SetView set refresh /> })
                        .collect_view()
                })
        }}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdditionCol {
    State,
    Name,
    System,
    Parent,
    Approved,
}

#[derive(Clone, PartialEq)]
struct AdditionRow(Addition);

impl ListRow for AdditionRow {
    type Col = AdditionCol;
    const COLUMNS: &'static [AdditionCol] = &[
        AdditionCol::State,
        AdditionCol::Name,
        AdditionCol::System,
        AdditionCol::Parent,
        AdditionCol::Approved,
    ];

    fn column(col: AdditionCol) -> Column {
        match col {
            AdditionCol::State => Column::new("state", "State", Width::Fixed(4.5))
                .sortable()
                .hint("Whether builds use it"),
            AdditionCol::Name => Column::new(
                "name",
                "Name",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            AdditionCol::System => Column::new(
                "system",
                "System",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            AdditionCol::Parent => {
                Column::new("parent", "Version of", Width::Flex { min: 8.0, share: 2 })
                    .sortable()
                    .hint("The game it is a version of, if any")
            }
            AdditionCol::Approved => {
                Column::new("approved", "Approved", Width::Fixed(11.0)).sortable()
            }
        }
    }

    fn key(&self) -> String {
        self.0.id.to_string()
    }

    fn title(&self) -> String {
        self.0.name.clone()
    }

    fn search(&self) -> String {
        let a = &self.0;
        format!(
            "{} {} {} {} {}",
            a.name,
            a.system,
            a.parent.as_deref().unwrap_or_default(),
            a.origin,
            a.file_name
        )
    }

    fn cell(&self, col: AdditionCol) -> AnyView {
        let a = &self.0;
        match col {
            AdditionCol::State => view! { <StatusIcon status=a.state tab_stop=false /> }.into_any(),
            AdditionCol::Name => view! { <Clip text=a.name.clone() /> }.into_any(),
            AdditionCol::System => view! { <Clip text=a.system.clone() /> }.into_any(),
            AdditionCol::Parent => a
                .parent
                .clone()
                .map(|p| view! { <Clip text=p /> })
                .into_any(),
            AdditionCol::Approved => view! { <Clip text=when(&a.created_at) /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: AdditionCol) -> Ordering {
        let (a, b) = (&self.0, &other.0);
        match col {
            AdditionCol::State => a.state.rank().cmp(&b.state.rank()),
            AdditionCol::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            AdditionCol::System => a.system.cmp(&b.system),
            AdditionCol::Parent => a.parent.cmp(&b.parent),
            AdditionCol::Approved => a.created_at.cmp(&b.created_at),
        }
    }
}

/// The additions, each opening with its facts and the form to edit, retire or restore it.
/// Your unsaved edits to additions' names and parents, by addition: they live with the list, not its detail, so they
/// survive the detail moving between the pane and the drawer, or opening another addition and coming back.
#[derive(Debug, Clone, Default)]
struct Drafts(HashMap<i64, (String, String)>);

impl Drafts {
    /// The name and parent the form shows for addition `id`: your edit, or else the addition's own.
    fn of(&self, id: i64, name: &str, parent: Option<&str>) -> (String, String) {
        self.0
            .get(&id)
            .cloned()
            .unwrap_or_else(|| (name.to_string(), parent.unwrap_or_default().to_string()))
    }

    fn entry(&mut self, id: i64, name: &str, parent: Option<&str>) -> &mut (String, String) {
        let fresh = self.of(id, name, parent);
        self.0.entry(id).or_insert(fresh)
    }

    fn set_name(&mut self, id: i64, name: &str, parent: Option<&str>, value: String) {
        self.entry(id, name, parent).0 = value;
    }

    fn set_parent(&mut self, id: i64, name: &str, parent: Option<&str>, value: String) {
        self.entry(id, name, parent).1 = value;
    }

    /// Addition `id` is saved: the form shows what it now holds.
    fn saved(&mut self, id: i64) {
        self.0.remove(&id);
    }
}

#[component]
pub fn AdditionsList(
    #[prop(into)] additions: Signal<Arc<Vec<Addition>>>,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let rows = Signal::derive(move || {
        additions.with(|a| Arc::new(a.iter().cloned().map(AdditionRow).collect::<Vec<_>>()))
    });
    let looks =
        Signal::derive(move || rows.with(|rows| present_looks(rows.iter().map(|r| r.0.state))));
    let drafts = RwSignal::new(Drafts::default());
    view! {
        <List
            id="additions"
            label="Additions"
            rows
            empty="You have no additions yet."
            placeholder="Filter by name, system, game or where it came from"
            detail=Callback::new(move |row: AdditionRow| view! { <AdditionDetail addition=row.0 drafts refresh /> }.into_any())
            legend=move || view! { <LegendStrip looks list="additions" /> }
        />
    }
}

/// One additions set: where it is, its check, and its kept files.
#[component]
pub fn SetView(set: AdditionsSet, refresh: RwSignal<u64>) -> impl IntoView {
    let check = RwSignal::new(None::<AdditionsCheck>);
    let problem = RwSignal::new(None::<String>);
    let system = set.system.clone();
    let run_check = move |_| {
        let path = format!("/additions-set/checks?system={}", api::segment(&system));
        leptos::task::spawn_local(async move {
            match api::post::<_, AdditionsCheck>(&path, &()).await {
                Ok(c) => {
                    problem.set(None);
                    check.set(Some(c));
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let found = move || {
        check.get().map(|c| {
            let summary = match c.drifted() {
                true => "The check found drift: anything below that is not intact.",
                false => "The check found every active addition intact, and nothing else.",
            };
            view! {
                <p>{format!("Checked {}. {summary}", when(&c.checked_at))}</p>
                <ul class="plain">
                    {c
                        .entries
                        .into_iter()
                        .filter(|e| e.finding != AdditionsFinding::Intact)
                        .map(|e| {
                            view! {
                                <li>
                                    <code>{e.name}</code>
                                    {format!(": {}", finding_label(e.finding))}
                                    {e.detail.map(|d| format!(", {d}"))}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
        })
    };
    let count = set.additions.len();
    let query = format!("?system={}", api::segment(&set.system));
    let (aside_system, unregister_what) = (
        set.system.clone(),
        format!("the additions set of {}", set.system),
    );
    let leftovers = (!set.leftovers.is_empty()).then(|| {
        view! {
            <h3>"Found entries"</h3>
            <p class="note">
                "Folders an addition was being published through when the service stopped. Nothing reads or \
                deletes them; remove them yourself once you have looked at them."
            </p>
            <ul class="plain">
                {set
                    .leftovers
                    .iter()
                    .map(|l| {
                        view! {
                            <li>
                                <code>{l.name.clone()}</code>
                                {format!(", found {}", when(&l.found_at))}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
    });
    view! {
        <section aria-label=format!("Additions of {}", set.system)>
            <h2>{set.system.clone()}</h2>
            <p>
                "In root " <code>{set.root.clone()}</code> " at " <code>{set.path.clone()}</code>
                {format!(" · {count} additions")}
            </p>
            <div class="form">
                <button type="button" on:click=run_check>
                    "Check the additions set for drift"
                </button>
            </div>
            {found}
            {move || problem.get().map(|message| view! { <Problem message /> })}
            {leftovers}
            <h3>"Kept files"</h3>
            <KeptFiles place=KeptPlace::AdditionsSet(set.system.clone()) scope=format!("kept-{}", set.id) />
            <LifecycleSection>
                <AdditionsSetAsideAction system=aside_system refresh />
                <UnregisterAction
                    what=unregister_what
                    paths=Paths {
                        holds: format!("/additions-set/holds{query}"),
                        unregister: format!("/additions-set/unregistration{query}"),
                    }
                    left="Its files stay where they are, no longer managed."
                />
            </LifecycleSection>
        </section>
    }
}

/// An addition's facts, and the form to rename it, retire it or restore it.
#[component]
fn AdditionDetail(
    addition: Addition,
    drafts: RwSignal<Drafts>,
    refresh: RwSignal<u64>,
) -> impl IntoView {
    let id = addition.id;
    let (own_name, own_parent) = (
        StoredValue::new(addition.name.clone()),
        StoredValue::new(addition.parent.clone()),
    );
    let draft = move || {
        drafts
            .with(|d| own_name.with_value(|n| own_parent.with_value(|p| d.of(id, n, p.as_deref()))))
    };
    let set_name = move |value: String| {
        own_name.with_value(|n| {
            own_parent.with_value(|p| drafts.update(|d| d.set_name(id, n, p.as_deref(), value)))
        })
    };
    let set_parent = move |value: String| {
        own_name.with_value(|n| {
            own_parent.with_value(|p| drafts.update(|d| d.set_parent(id, n, p.as_deref(), value)))
        })
    };
    let problem = RwSignal::new(None::<String>);
    let editable = matches!(
        addition.state,
        AdditionState::Active | AdditionState::Pending
    );
    let retirable = addition.state == AdditionState::Active;
    let restorable = addition.state == AdditionState::Retired;
    let save = move |_| {
        let (name, parent) = untrack(draft);
        let body = EditAddition {
            name: Some(name),
            parent: Some((!parent.trim().is_empty()).then_some(parent)),
        };
        leptos::task::spawn_local(async move {
            match api::patch::<_, Addition>(&format!("/additions/{id}"), &body).await {
                Ok(_) => {
                    problem.set(None);
                    drafts.update(|d| d.saved(id));
                    refresh.update(|n| *n += 1);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let retire = move |_| {
        leptos::task::spawn_local(async move {
            match api::post::<_, Addition>(&format!("/additions/{id}/retirement"), &()).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let restore = move |_| {
        leptos::task::spawn_local(async move {
            match api::post::<_, Addition>(&format!("/additions/{id}/restoration"), &()).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    let hashes = addition.rom.as_ref().map(|rom| {
        view! {
            <dt>"ROM"</dt>
            <dd class="data">
                {format!("{} · {} bytes", rom.name, rom.size)}
                {rom.crc.clone().map(|h| format!(" · CRC32 {h}"))}
                {rom.md5.clone().map(|h| format!(" · MD5 {h}"))}
                {rom.sha1.clone().map(|h| format!(" · SHA-1 {h}"))}
                {rom.sha256.clone().map(|h| format!(" · SHA-256 {}", short_sha(&h)))}
            </dd>
        }
    });
    view! {
        <p>
            <StatusBadge status=addition.state />
        </p>
        {addition.error.clone().map(|message| view! { <Problem message live=Live::Off /> })}
        <dl class="facts">
            <dt>"System"</dt>
            <dd>{addition.system.clone()}</dd>
            <dt>"Stored as"</dt>
            <dd>{addition.file_name.clone()}</dd>
            <dt>"From"</dt>
            <dd>{format!("The {} {}", addition.source.label(), addition.origin)}</dd>
            {addition.parent.clone().map(|p| view! {
                <dt>"Version of"</dt>
                <dd>{p}</dd>
            })}
            <dt>"Approved"</dt>
            <dd>{format!("{} by {}", when(&addition.created_at), addition.approved_by)}</dd>
            {hashes}
        </dl>
        {editable.then(|| view! {
            <div class="form">
                <label>
                    "Name "
                    <input
                        type="text"
                        prop:value=move || draft().0
                        on:input=move |ev| set_name(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Parent game, if any "
                    <input
                        type="text"
                        prop:value=move || draft().1
                        on:input=move |ev| set_parent(event_target_value(&ev))
                    />
                </label>
                <button type="button" on:click=save>"Save the name and parent"</button>
                {retirable.then(|| view! {
                    <button type="button" on:click=retire>"Retire it from builds"</button>
                })}
            </div>
        })}
        {restorable.then(|| view! {
            <div class="form">
                <button type="button" on:click=restore>"Restore it to builds"</button>
            </div>
        })}
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_outlives_its_detail_until_it_is_saved() {
        let mut drafts = Drafts::default();
        assert_eq!(
            drafts.of(7, "Pocket Ranger", Some("Ranger")),
            ("Pocket Ranger".to_string(), "Ranger".to_string())
        );
        drafts.set_name(
            7,
            "Pocket Ranger",
            Some("Ranger"),
            "Pocket Ranger DX".into(),
        );
        drafts.set_parent(7, "Pocket Ranger", Some("Ranger"), String::new());
        assert_eq!(
            drafts.of(7, "Pocket Ranger", Some("Ranger")),
            ("Pocket Ranger DX".to_string(), String::new())
        );
        assert_eq!(
            drafts.of(8, "Other", None),
            ("Other".to_string(), String::new())
        );
        drafts.saved(7);
        assert_eq!(
            drafts.of(7, "Pocket Ranger DX", None),
            ("Pocket Ranger DX".to_string(), String::new())
        );
    }

    #[test]
    fn a_suggested_name_drops_folders_and_the_extension() {
        assert_eq!(suggested_name("hacks/My Hack (v1.1).a26"), "My Hack (v1.1)");
        assert_eq!(suggested_name("Demo.v2.zip"), "Demo.v2");
        assert_eq!(suggested_name(".hidden"), ".hidden");
        assert_eq!(suggested_name("noext"), "noext");
    }
}
