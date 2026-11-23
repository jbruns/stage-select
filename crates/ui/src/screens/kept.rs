//! The one kept-files view, wherever kept files are shown: a Library Folder's, an Ingest Folder's, a Source Set's and
//! an Additions Set's, each with deleting the ones you check through a deletion you approve, and every folder's at once
//! on Activity, which links each file to its folder. The pure logic is in `kept_model`.

use std::cmp::Ordering;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_query_map;
use stage_select_api::SourceSetRecord;
use stage_select_api::additions::AdditionsSet;
use stage_select_api::build::{
    ApproveKeptDeletion, KeptDeletion, KeptFile, LibraryFolder, PlanKeptDeletion,
};
use stage_select_api::ingest::{IngestFolder, IngestKeptFile};
use stage_select_api::repair::{SourceKeptDeletion, SourceKeptFile};

use crate::api;
use crate::app::Loading;
use crate::events::use_live;
use crate::ingest_model::size;
use crate::kept_model::{
    Deletion, DeletionRow, KeptPlace, KeptRow, SetAsideBy, deleting, deleting_all, newest_first,
    summary,
};
use crate::model::{short_sha, when};
use crate::parts::help::Help;
use crate::parts::icon::Glyph;
use crate::parts::list::{
    Clip, Column, List, ListRow, RowAction, Selected, ToolAction, Width, last_good_rows,
};
use crate::parts::message::{Message, MessageKind, Problem};
use crate::parts::pending::ActionPending;
use crate::parts::plan::{self, ApproveBar, PlanReview, PlanRow};
use crate::parts::plan_model::Approval;
use crate::parts::status::{ItemOutcome, KeptDeletionKind, Status, StatusBadge, StatusIcon};
use crate::parts::url_state::{intern, scoped, use_query_edit};

/// What one place's kept files say about themselves, under the count.
fn about(place: &KeptPlace) -> &'static str {
    match place {
        KeptPlace::LibraryFolder { .. } => {
            "Files builds moved aside instead of deleting: removed, replaced, or conflicts you chose to set aside."
        }
        KeptPlace::IngestFolder(_) => {
            "Items you discarded, DATs you imported and the originals of repacks, set aside in the folder."
        }
        KeptPlace::SourceSet(_) => {
            "What repairs replaced or set aside, kept in the source set. Audits don't read them."
        }
        KeptPlace::AdditionsSet(_) => {
            "Files of additions you retired, kept in the additions set. Builds and checks don't read them. Restore an \
             addition to move its file back."
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptCol {
    Name,
    Folder,
    Size,
    Kept,
    SetAsideBy,
}

fn kept_column(col: KeptCol) -> Column {
    match col {
        KeptCol::Name => Column::new(
            "name",
            "File",
            Width::Flex {
                min: 10.0,
                share: 3,
            },
        )
        .sortable(),
        KeptCol::Folder => Column::new("folder", "Folder", Width::Flex { min: 8.0, share: 2 })
            .sortable()
            .hint("The folder it is kept in"),
        KeptCol::Size => Column::new("size", "Size", Width::Fixed(7.0))
            .sortable()
            .numeric(),
        KeptCol::Kept => Column::new("kept", "Kept", Width::Fixed(12.0)).sortable(),
        KeptCol::SetAsideBy => Column::new("by", "Set aside by", Width::Fixed(10.0))
            .sortable()
            .hint("The build, repair, plan or retired addition that set it aside"),
    }
}

fn kept_cell(row: &KeptRow, col: KeptCol) -> AnyView {
    match col {
        KeptCol::Name => view! { <Clip text=row.name.clone() /> }.into_any(),
        KeptCol::Folder => view! { <Clip text=row.place.name() /> }.into_any(),
        KeptCol::Size => size(row.size).into_any(),
        KeptCol::Kept => view! { <Clip text=when(&row.kept_at) /> }.into_any(),
        // Not a tab stop of its own: the row's action and its detail carry the same link for the keyboard.
        KeptCol::SetAsideBy => match &row.set_aside_by {
            Some(by) => {
                let (href, label) = (by.href(), by.label());
                view! {
                    <A href=href attr:tabindex="-1">
                        {label}
                    </A>
                }
                .into_any()
            }
            None => "".into_any(),
        },
    }
}

fn kept_compare(a: &KeptRow, b: &KeptRow, col: KeptCol) -> Ordering {
    match col {
        KeptCol::Name => a.name.cmp(&b.name),
        KeptCol::Folder => a.place.name().cmp(&b.place.name()),
        KeptCol::Size => a.size.cmp(&b.size),
        KeptCol::Kept => a.kept_at.cmp(&b.kept_at),
        KeptCol::SetAsideBy => by_order(&a.set_aside_by).cmp(&by_order(&b.set_aside_by)),
    }
}

fn by_order(by: &Option<SetAsideBy>) -> (String, i64) {
    match by {
        None => (String::new(), 0),
        Some(by @ (SetAsideBy::Build(n) | SetAsideBy::Retired(n))) => (kind_word(by), *n),
        Some(by @ (SetAsideBy::Repair { plan, .. } | SetAsideBy::IngestPlan { plan, .. })) => {
            (kind_word(by), *plan)
        }
    }
}

fn kind_word(by: &SetAsideBy) -> String {
    by.label().split(' ').next().unwrap_or_default().to_string()
}

fn search(row: &KeptRow) -> String {
    format!(
        "{} {} {} {}",
        row.name,
        row.place.name(),
        row.reason,
        row.set_aside_by
            .as_ref()
            .map(SetAsideBy::label)
            .unwrap_or_default()
    )
}

/// The link to what set a file aside, as a row action.
fn set_aside_action(row: &KeptRow) -> Option<RowAction> {
    row.set_aside_by.as_ref().map(|by| {
        let glyph = match by {
            SetAsideBy::Build(_) => Glyph::Package,
            SetAsideBy::Repair { .. } => Glyph::Wrench,
            SetAsideBy::Retired(_) => Glyph::CirclePlus,
            SetAsideBy::IngestPlan { .. } => Glyph::FolderInput,
        };
        RowAction::go(glyph, format!("Open {}", by.label()), by.href())
    })
}

/// A kept file of the folder whose page this is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept(KeptRow);

impl ListRow for Kept {
    type Col = KeptCol;
    const COLUMNS: &'static [KeptCol] = &[
        KeptCol::Name,
        KeptCol::Size,
        KeptCol::Kept,
        KeptCol::SetAsideBy,
    ];

    fn column(col: KeptCol) -> Column {
        kept_column(col)
    }

    fn key(&self) -> String {
        self.0.id.to_string()
    }

    fn title(&self) -> String {
        self.0.name.clone()
    }

    fn search(&self) -> String {
        search(&self.0)
    }

    fn cell(&self, col: KeptCol) -> AnyView {
        kept_cell(&self.0, col)
    }

    fn compare(&self, other: &Self, col: KeptCol) -> Ordering {
        kept_compare(&self.0, &other.0, col)
    }
}

/// A kept file of any folder, on Activity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeptAnywhere(KeptRow);

impl ListRow for KeptAnywhere {
    type Col = KeptCol;
    const COLUMNS: &'static [KeptCol] = &[
        KeptCol::Name,
        KeptCol::Folder,
        KeptCol::Size,
        KeptCol::Kept,
        KeptCol::SetAsideBy,
    ];

    fn column(col: KeptCol) -> Column {
        kept_column(col)
    }

    fn key(&self) -> String {
        self.0.key()
    }

    fn title(&self) -> String {
        self.0.name.clone()
    }

    fn search(&self) -> String {
        search(&self.0)
    }

    fn cell(&self, col: KeptCol) -> AnyView {
        kept_cell(&self.0, col)
    }

    fn compare(&self, other: &Self, col: KeptCol) -> Ordering {
        kept_compare(&self.0, &other.0, col)
    }

    fn href(&self) -> Option<String> {
        Some(self.0.place.href())
    }
}

fn place_link(place: &KeptPlace) -> RowAction {
    let name = place.name();
    let (glyph, label) = match place {
        KeptPlace::LibraryFolder { .. } => (
            Glyph::Library,
            format!("Open the kept files of library folder {name}"),
        ),
        KeptPlace::IngestFolder(_) => (
            Glyph::FolderInput,
            format!("Open the kept files of ingest folder {name}"),
        ),
        KeptPlace::SourceSet(_) => (
            Glyph::Database,
            format!("Open the kept files of source set {name}"),
        ),
        KeptPlace::AdditionsSet(_) => (Glyph::CirclePlus, format!("Open the additions of {name}")),
    };
    RowAction::go(glyph, label, place.href())
}

/// A kept file's reason, where it is, and what it read as: the detail pane, never a cell.
fn kept_detail(row: KeptRow, anywhere: bool) -> AnyView {
    let folder = anywhere.then(|| {
        let (href, name) = (row.place.href(), row.place.name());
        view! {
            <dt>"Folder"</dt>
            <dd>
                <A href=href>{name}</A>
            </dd>
        }
    });
    let by = row.set_aside_by.as_ref().map(|by| {
        let (href, label) = (by.href(), by.label());
        view! {
            <dt>"Set aside by"</dt>
            <dd>
                <A href=href>{label}</A>
            </dd>
        }
    });
    view! {
        <dl class="facts">
            {folder}
            <dt>"Why"</dt>
            <dd>{row.reason.clone()}</dd>
            {by}
            <dt>"Kept as"</dt>
            <dd>
                <code class="path">{row.kept_path()}</code>
            </dd>
            {row.game.clone().map(|g| view! {
                <dt>"Game"</dt>
                <dd>{g}</dd>
            })}
            <dt>"Size"</dt>
            <dd class="data">
                {size(row.size)}
                {row.directory.then_some(", a directory kept whole")}
            </dd>
            <dt>"Kept"</dt>
            <dd>{when(&row.kept_at)}</dd>
            {row.sha256.clone().map(|sha| view! {
                <dt>"SHA-256"</dt>
                <dd>
                    <code class="hash">{sha}</code>
                </dd>
            })}
        </dl>
    }
    .into_any()
}

/// The kept files of `place`.
async fn kept_files(place: KeptPlace) -> Result<Vec<KeptRow>, String> {
    let path = place.files_path();
    let rows = match &place {
        KeptPlace::LibraryFolder { id, path: folder } => api::get::<Vec<KeptFile>>(&path)
            .await?
            .into_iter()
            .map(|f| KeptRow::library(*id, folder, f))
            .collect(),
        KeptPlace::IngestFolder(root) => api::get::<Vec<IngestKeptFile>>(&path)
            .await?
            .into_iter()
            .map(|f| KeptRow::ingest(root, f))
            .collect(),
        KeptPlace::SourceSet(name) => api::get::<Vec<SourceKeptFile>>(&path)
            .await?
            .into_iter()
            .map(|f| KeptRow::source(name, f))
            .collect(),
        KeptPlace::AdditionsSet(system) => api::get::<Vec<SourceKeptFile>>(&path)
            .await?
            .into_iter()
            .map(|f| KeptRow::additions(system, f))
            .collect(),
    };
    Ok(rows)
}

/// The kept files of every Library Folder, Ingest Folder, Source Set and Additions Set.
async fn every_kept_file() -> Result<Vec<KeptRow>, String> {
    let mut rows = Vec::new();
    let folders: Vec<LibraryFolder> = api::get("/libraries").await?;
    for folder in folders {
        rows.extend(
            kept_files(KeptPlace::LibraryFolder {
                id: folder.id,
                path: folder.path,
            })
            .await?,
        );
    }
    let ingest: Vec<IngestFolder> = api::get("/ingest-folders").await?;
    for folder in ingest.into_iter().filter(|f| f.id.is_some()) {
        rows.extend(kept_files(KeptPlace::IngestFolder(folder.root)).await?);
    }
    let sets: Vec<SourceSetRecord> = api::get("/source-sets").await?;
    for set in sets {
        rows.extend(kept_files(KeptPlace::SourceSet(set.name)).await?);
    }
    let additions: Vec<AdditionsSet> = api::get("/additions-sets").await?;
    for set in additions {
        rows.extend(
            set.kept
                .into_iter()
                .map(|f| KeptRow::additions(&set.system, f)),
        );
    }
    Ok(newest_first(rows))
}

/// Plans deleting `body`'s kept files of `place`.
async fn plan_deletion(place: KeptPlace, body: PlanKeptDeletion) -> Result<Deletion, String> {
    let path = place.plan_path();
    Ok(match place {
        KeptPlace::LibraryFolder { .. } => api::post::<_, KeptDeletion>(&path, &body).await?.into(),
        _ => api::post::<_, SourceKeptDeletion>(&path, &body)
            .await?
            .into(),
    })
}

async fn load_deletion(place: KeptPlace, id: i64) -> Result<Deletion, String> {
    let path = place.deletion_path(id);
    Ok(match place {
        KeptPlace::LibraryFolder { .. } => api::get::<KeptDeletion>(&path).await?.into(),
        _ => api::get::<SourceKeptDeletion>(&path).await?.into(),
    })
}

/// Approves deletion `id` as the kept files of `owner`, the folder or set you reviewed it under, by its id.
async fn approve_deletion(
    place: KeptPlace,
    id: i64,
    owner: i64,
    digest: String,
) -> Result<Deletion, String> {
    let path = place.approval_path(id);
    let body = ApproveKeptDeletion { digest, owner };
    Ok(match place {
        KeptPlace::LibraryFolder { .. } => api::post::<_, KeptDeletion>(&path, &body).await?.into(),
        _ => api::post::<_, SourceKeptDeletion>(&path, &body)
            .await?
            .into(),
    })
}

/// The kept-files view. With a `place`, that folder's kept files, and deleting the ones you check, or every one,
/// through a deletion you review and approve, shown in the list's place while the URL's `kept-deletion` (or
/// `{scope}-deletion`) names it. Without one, every folder's kept files, each linking to its folder, where you delete
/// them.
#[component]
pub fn KeptFiles(
    #[prop(optional)] place: Option<KeptPlace>,
    /// For a view beside another list on its page: the prefix of its URL keys, such as `kept-3` for `kept-3-q`.
    #[prop(optional, into)]
    scope: Option<String>,
    /// While the folder is busy with a Job, which a deletion would wait for.
    #[prop(optional, into)]
    busy: Option<Signal<bool>>,
    /// The page's own counter, bumped once a deletion is carried out.
    #[prop(optional)]
    refresh: Option<RwSignal<u64>>,
) -> impl IntoView {
    match place {
        Some(place) => view! { <FolderKept place scope busy refresh /> }.into_any(),
        None => view! { <EveryKept /> }.into_any(),
    }
}

#[component]
fn EveryKept() -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        every_kept_file()
    });
    let rows = last_good_rows(move || {
        files
            .get()
            .map(|r| r.map(|rows| rows.into_iter().map(KeptAnywhere).collect()))
    });
    let lead = move || rows.with(|r| summary(&r.iter().map(|k| k.0.clone()).collect::<Vec<_>>()));
    view! {
        <p class="lede">{lead}</p>
        <Help summary="You delete kept files from their folder's page.">
            <p>
                "Each folder keeps the files it set aside until you approve deleting them. Open a file's folder to \
                plan deleting it."
            </p>
        </Help>
        {move || match files.get() {
            None => view! { <Loading what="the kept files" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="kept-files"
            label="Kept files"
            rows
            empty="No files are set aside in any folder."
            placeholder="Filter by file, folder, reason or what set it aside"
            row_actions=Callback::new(move |row: KeptAnywhere| {
                std::iter::once(place_link(&row.0.place)).chain(set_aside_action(&row.0)).collect()
            })
            detail=Callback::new(|row: KeptAnywhere| kept_detail(row.0, true))
        />
    }
}

#[component]
fn FolderKept(
    place: KeptPlace,
    scope: Option<String>,
    busy: Option<Signal<bool>>,
    refresh: Option<RwSignal<u64>>,
) -> impl IntoView {
    let live = use_live();
    let busy = busy.unwrap_or_else(|| Signal::stored(false));
    let reloaded = RwSignal::new(0u64);
    let place = StoredValue::new(place);
    // Scoped even alone on its page, so its review's keys never meet another list's.
    let scope = scope.unwrap_or_else(|| "kept".to_string());
    let list_id = intern(scope.clone());
    let review_scope = format!("{scope}-deletion");
    let review_id = intern(review_scope.clone());
    let shown_key = scoped(Some(&scope), "deletion");
    let list_keys = ["row", "sel", "all"].map(|k| scoped(Some(&scope), k));
    let review_keys =
        ["q", "sort", "row", "sel", "all", "kind"].map(|k| scoped(Some(&review_scope), k));

    let query = use_query_map();
    let shown =
        Memo::new(move |_| query.with(|q| q.get(shown_key).and_then(|v| v.parse::<i64>().ok())));
    let edit = use_query_edit();
    let show = move |deletion: Option<i64>| {
        let mut changes: Vec<(&'static str, Option<String>)> =
            vec![(shown_key, deletion.map(|d| d.to_string()))];
        changes.extend(list_keys.iter().map(|k| (*k, None)));
        changes.extend(review_keys.iter().map(|k| (*k, None)));
        edit.run(changes);
    };

    let files = LocalResource::new(move || {
        live.settled.track();
        reloaded.track();
        refresh.inspect(|r| r.track());
        kept_files(place.get_value())
    });
    let rows = last_good_rows(move || {
        files
            .get()
            .map(|r| r.map(|rows| rows.into_iter().map(Kept).collect()))
    });
    let lead = move || rows.with(|r| summary(&r.iter().map(|k| k.0.clone()).collect::<Vec<_>>()));

    let planning = Action::new_local(move |body: &PlanKeptDeletion| {
        let body = body.clone();
        async move {
            let planned = plan_deletion(place.get_value(), body).await;
            if let Ok(d) = &planned {
                show(Some(d.id));
            }
            planned
        }
    });
    let waiting = RwSignal::new(false);
    let plan = move |body: PlanKeptDeletion| {
        if busy.get_untracked() {
            waiting.set(true);
        } else {
            waiting.set(false);
            planning.dispatch_local(body);
        }
    };
    let actions = vec![
        ToolAction::checked(
            Glyph::Trash2,
            "Plan deleting",
            move |chosen: Selected<Kept>| {
                let rows: Vec<KeptRow> = chosen.rows.into_iter().map(|k| k.0).collect();
                plan(deleting(&rows));
            },
        ),
        ToolAction::page(Glyph::Trash2, "Plan deleting every kept file", move || {
            plan(deleting_all())
        }),
    ];
    let about = place.with_value(about);
    let actions = StoredValue::new(actions);
    let scope = StoredValue::new(scope);
    let list = Callback::new(move |()| {
        view! {
            <p class="lede">{lead}</p>
            <Help summary="Nothing here is deleted until you approve deleting it.">
                <p>{about}</p>
                <p>
                    "Check the files to delete and plan deleting them, or plan deleting every kept file. You \
                    review the plan, then approve it."
                </p>
            </Help>
            {move || match files.get() {
                None => view! { <Loading what="the kept files" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(_)) => ().into_any(),
            }}
            {move || waiting.get().then(|| view! {
                <Message kind=MessageKind::NOTICE>
                    "The folder is busy with a job. Plan deleting once it finishes."
                </Message>
            })}
            <ActionPending action=planning status="Planning the deletion…">
                <List
                    id=list_id
                    label="Kept files"
                    rows
                    empty="No files are set aside."
                    placeholder="Filter by file, reason or what set it aside"
                    actions=actions.get_value()
                    row_actions=Callback::new(move |row: Kept| set_aside_action(&row.0).into_iter().collect())
                    detail=Callback::new(|row: Kept| kept_detail(row.0, false))
                    scope=scope.get_value()
                />
            </ActionPending>
        }
        .into_any()
    });
    let after = Callback::new(move |()| {
        reloaded.update(|n| *n += 1);
        if let Some(r) = refresh {
            r.update(|n| *n += 1);
        }
    });
    let back = Callback::new(move |()| show(None));
    let review_scope = StoredValue::new(review_scope);
    view! {
        {move || match shown.get() {
            Some(id) => view! {
                <DeletionReview
                    place=place.get_value()
                    id
                    list_id=review_id
                    scope=review_scope.get_value()
                    busy
                    after
                    back
                />
            }
            .into_any(),
            None => list.run(()),
        }}
    }
}

/// A kept file a deletion covers, in its review.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeletionItem(DeletionRow);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeletionCol {
    Kind,
    Name,
    Size,
    Outcome,
}

impl PlanRow for DeletionItem {
    type Kind = KeptDeletionKind;
    fn kind(&self) -> KeptDeletionKind {
        self.0.kind
    }
    fn outcome(&self) -> Option<ItemOutcome> {
        self.0.outcome()
    }
}

impl ListRow for DeletionItem {
    type Col = DeletionCol;
    const COLUMNS: &'static [DeletionCol] = &[
        DeletionCol::Kind,
        DeletionCol::Name,
        DeletionCol::Size,
        DeletionCol::Outcome,
    ];

    fn column(col: DeletionCol) -> Column {
        match col {
            DeletionCol::Kind => Column::new("kind", "Kind", Width::Fixed(3.5))
                .sortable()
                .hint("What the deletion does with the file, or did"),
            DeletionCol::Name => Column::new(
                "name",
                "File",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            DeletionCol::Size => Column::new("size", "Size", Width::Fixed(7.0))
                .sortable()
                .numeric(),
            DeletionCol::Outcome => plan::outcome_column(),
        }
    }

    fn key(&self) -> String {
        self.0.id.to_string()
    }

    fn title(&self) -> String {
        self.0.name.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {}",
            self.0.name,
            self.0.kind.look().label,
            self.0.detail.as_deref().unwrap_or_default()
        )
    }

    fn cell(&self, col: DeletionCol) -> AnyView {
        match col {
            DeletionCol::Kind => {
                view! { <StatusIcon status=self.0.kind tab_stop=false /> }.into_any()
            }
            DeletionCol::Name => view! { <Clip text=self.0.name.clone() /> }.into_any(),
            DeletionCol::Size => size(self.0.size).into_any(),
            DeletionCol::Outcome => plan::outcome_cell(self.outcome()),
        }
    }

    fn compare(&self, other: &Self, col: DeletionCol) -> Ordering {
        match col {
            DeletionCol::Kind => self.0.kind.rank().cmp(&other.0.kind.rank()),
            DeletionCol::Name => self.0.name.cmp(&other.0.name),
            DeletionCol::Size => self.0.size.cmp(&other.0.size),
            DeletionCol::Outcome => plan::compare_outcome(self.outcome(), other.outcome()),
        }
    }
}

fn deletion_detail(row: DeletionItem) -> AnyView {
    let row = row.0;
    view! {
        <dl class="facts">
            <dt>"Does"</dt>
            <dd>
                <StatusBadge status=row.kind />
            </dd>
            {row.detail.clone().map(|d| view! {
                <dt>"Why"</dt>
                <dd>{d}</dd>
            })}
            <dt>"Kept as"</dt>
            <dd>
                <code class="path">{row.kept_path()}</code>
            </dd>
            <dt>"Size"</dt>
            <dd class="data">{size(row.size)}</dd>
            {row.reviewed.clone().map(|sha| view! {
                <dt>"Read as"</dt>
                <dd>
                    "SHA-256 " <code class="hash">{sha}</code>
                </dd>
            })}
        </dl>
    }
    .into_any()
}

/// A deletion of kept files, in the plan shape: what it deletes, approving it above its files, and, once carried out,
/// what it did, a file left kept first.
#[component]
fn DeletionReview(
    place: KeptPlace,
    id: i64,
    list_id: &'static str,
    scope: String,
    busy: Signal<bool>,
    after: Callback<()>,
    back: Callback<()>,
) -> impl IntoView {
    let live = use_live();
    let reloaded = RwSignal::new(0u64);
    let place = StoredValue::new(place);
    let loaded = LocalResource::new(move || {
        live.settled.track();
        reloaded.track();
        load_deletion(place.get_value(), id)
    });
    let latest = Memo::new(move |prev: Option<&Option<Deletion>>| match loaded.get() {
        Some(Ok(d)) => Some(d),
        _ => prev.cloned().flatten(),
    });
    let approving = Action::new_local(move |d: &Deletion| {
        let (id, owner, digest) = (d.id, d.owner_id(), d.digest.clone());
        async move {
            let done = approve_deletion(place.get_value(), id, owner, digest).await;
            if done.is_ok() {
                reloaded.update(|n| *n += 1);
                after.run(());
            }
            done
        }
    });
    let rows = last_good_rows(move || {
        loaded
            .get()
            .map(|d| d.map(|d| d.rows.into_iter().map(DeletionItem).collect()))
    });
    let summary = Signal::derive(move || {
        latest.with(|d| d.as_ref().map(Deletion::summary).unwrap_or_default())
    });
    let approve = move || {
        latest.get().map(|d| {
            let here = place.with_value(|p| d.belongs_to(p));
            let elsewhere = (!here).then(|| {
                let owner = place.with_value(|p| d.owner_href(p));
                view! {
                    <Message kind=MessageKind::PROBLEM>
                        "This plan deletes kept files of another folder, so you can't approve it here. "
                        {match owner {
                            Some(href) => view! { <A href>"Review it with its own kept files"</A> "." }.into_any(),
                            None => "That folder is unregistered: review its kept files under Unregistered.".into_any(),
                        }}
                    </Message>
                }
            });
            let approve = (!d.approved && here).then(|| {
                let shown = StoredValue::new(d.clone());
                view! {
                    <ApproveBar
                        label=d.approve_label()
                        approval=Approval::default()
                        on_approve=Callback::new(move |()| {
                            approving.dispatch_local(shown.get_value());
                        })
                        busy=Signal::derive(move || busy.get() || approving.pending().get())
                    />
                }
            });
            let recovery = d.recovery_error.clone().map(|message| view! { <Problem message /> });
            view! {
                {elsewhere}
                {recovery}
                {approve}
            }
        })
    };
    let help = move || {
        latest.get().map(|d| {
            view! {
                <p>
                    {format!(
                        "Digest {}: approving binds these files as they read now. ",
                        short_sha(&d.digest),
                    )}
                    "A file is deleted only while it still reads as it did when you planned this. One that \
                    changed since is left kept, and its row says why."
                </p>
                <p>
                    "Digest " <code class="hash">{d.digest.clone()}</code>
                </p>
            }
        })
    };
    view! {
        <h3>{format!("Deleting kept files, plan #{id}")}</h3>
        <div class="form">
            <button type="button" on:click=move |_| back.run(())>
                "Back to the kept files"
            </button>
        </div>
        {move || match loaded.get() {
            None if latest.with(Option::is_none) => {
                view! { <Loading what="the deletion" /> }.into_any()
            }
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            _ => ().into_any(),
        }}
        <PlanReview
            id=list_id
            label=format!("Deleting kept files, plan #{id}")
            rows
            empty="The deletion covers no files."
            placeholder="Filter by file or outcome"
            detail=Callback::new(deletion_detail)
            summary
            approve=move || view! {
                <ActionPending action=approving status="Deleting the kept files…">
                    {approve}
                </ActionPending>
            }
            help=help
            scope
        />
    }
}
