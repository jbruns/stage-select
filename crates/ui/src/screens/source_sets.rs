use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use stage_select_api::repair::{PlanRepair, RepairPlan};
use stage_select_api::{
    AddSourceSet, AuditEvidence, Dat, GameStatus, Job, PathTemplateDefaults, Root, SourceSetRecord,
    SourceSetSummary, StartAudit,
};

use crate::api;
use crate::app::{Loading, navigator};
use crate::coverage_model::{self, Draft};
use crate::events::use_live;
use crate::model::{needs_review_from_counts, when};
use crate::parts::choice::Choice;
use crate::parts::folder_picker::{Create, FolderPicker, Within};
use crate::parts::help::Help;
use crate::parts::icon::Glyph;
use crate::parts::icon::Svg;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    CellTip, Clip, Column, Keep, List, ListRow, RowAction, ToolAction, Width, last_good_rows,
};
use crate::parts::message::Problem;
use crate::parts::outcome::{Link, Report, Reports, ShowReports};
use crate::parts::overlay::Drawer;
use crate::parts::pending::PendingStatus;
use crate::parts::plan::{KindCounts, use_status_filter};
use crate::parts::plan_model::kind_counts;
use crate::parts::status::{SourceSetStatus, Status, StatusIcon};
use crate::parts::tabs::tab_link;
use crate::parts::url_state::encode;
use crate::registration;
use crate::screens::coverage::{self, CoverageEditor, UNAVAILABLE};
use crate::screens::repair::MAKING_PLAN;
use crate::source_set_model::{
    self as model, CREATE_SET, CREATE_SET_NOTE, MAY_CHANGE, MAY_CHANGE_NOTE, PageTab,
};

/// A source set's page.
pub fn set_href(name: &str) -> String {
    format!("/source-sets/{}", encode(name))
}

/// A tab of a source set's page.
pub fn tab_href(name: &str, tab: PageTab) -> String {
    tab_link(&set_href(name), Some(tab.slug()), &[])
}

/// Where you review the repair plan `id` of the source set `name`.
pub fn repair_plan_href(name: &str, id: i64) -> String {
    tab_link(
        &set_href(name),
        Some(PageTab::Repair.slug()),
        &[("plan", &id.to_string())],
    )
}

/// Proposes a repair of `name` from its latest audit, with the default choices: the one way a page outside the
/// source set's Repair tab proposes one, then opens it at [`repair_plan_href`].
pub async fn propose_repair(name: &str) -> Result<RepairPlan, String> {
    api::post(
        &format!("{}/repairs", set_href(name)),
        &PlanRepair::default(),
    )
    .await
}

/// A repair being proposed from a page: whether one is on its way, and why the last was refused.
#[derive(Clone, Copy)]
pub struct Proposal {
    pending: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
}

impl Proposal {
    pub fn new() -> Proposal {
        Proposal {
            pending: RwSignal::new(false),
            problem: RwSignal::new(None),
        }
    }

    pub fn pending(self) -> Signal<bool> {
        self.pending.into()
    }

    /// Proposes a repair of `name` and, once the Service returns the plan, opens it at [`repair_plan_href`]: the plan
    /// is named before the Repair tab loads, so it opens on it. A second start while one is on its way does nothing.
    pub fn start(self, name: String, navigate: Callback<String>) {
        if self.pending.get_untracked() {
            return;
        }
        self.pending.set(true);
        self.problem.set(None);
        leptos::task::spawn_local(async move {
            match propose_repair(&name).await {
                Ok(plan) => navigate.run(repair_plan_href(&name, plan.id)),
                Err(message) => self
                    .problem
                    .set(Some(format!("Couldn't propose a repair: {message}"))),
            }
            self.pending.set(false);
        });
    }
}

impl Default for Proposal {
    fn default() -> Self {
        Self::new()
    }
}

/// The Repair button, primary unless `secondary`: it proposes a repair of `name` and opens the plan. `title` says what
/// it proposes from.
#[component]
pub fn RepairNow(
    proposal: Proposal,
    name: String,
    title: &'static str,
    #[prop(optional)] secondary: bool,
) -> impl IntoView {
    let navigate = navigator();
    let name = StoredValue::new(name);
    view! {
        <button
            type="button"
            class=(!secondary).then_some("primary")
            disabled=proposal.pending()
            title=title
            on:click=move |_| proposal.start(name.get_value(), navigate)
        >
            "Repair"
        </button>
    }
}

/// How `proposal` is going: the plan being made, or why it was refused.
#[component]
pub fn ProposalStatus(proposal: Proposal) -> impl IntoView {
    view! {
        <Show when=move || proposal.pending.get()>
            <PendingStatus status=MAKING_PLAN />
        </Show>
        {move || proposal.problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// Where you preview a selection of the source set.
pub fn preview_href(name: &str) -> String {
    format!("/selections/new?source_set={}", encode(name))
}

/// Where you make a library folder for the source set.
pub fn new_library_href(name: &str) -> String {
    format!("/libraries/new?source_set={}", encode(name))
}

/// Queues an incremental audit of `name` against its default DAT revision, and reports the job or its refusal.
pub fn start_audit(name: String, reports: Reports) {
    leptos::task::spawn_local(async move {
        reports.started(queue_audit(&name).await, &format!("Couldn't audit {name}"));
    });
}

/// Queues an incremental Audit of `name` against its default DAT Revision.
pub async fn queue_audit(name: &str) -> Result<Job, String> {
    let request = StartAudit {
        source_set: name.to_owned(),
        revision: None,
        evidence: AuditEvidence::Incremental,
    };
    api::post("/audits", &request).await
}

/// Queues a check of `name` for files changed since its latest audit, and reports the job or its refusal.
pub fn start_check(name: String, reports: Reports) {
    leptos::task::spawn_local(async move {
        let path = format!("{}/stat-walks", set_href(&name));
        let started = api::post::<_, Job>(&path, &()).await;
        reports.started(started, &format!("Couldn't check {name} for changes"));
    });
}

/// Each cause of a stale source set as its icon, whose tooltip says what it found.
pub fn stale_icons(stale: &[stage_select_api::SourceSetStale]) -> AnyView {
    model::stale_causes(stale)
        .into_iter()
        .map(|(cause, line)| {
            view! {
                <span class="stale-cause" title=line>
                    <StatusIcon status=cause tab_stop=false />
                </span>
            }
        })
        .collect_view()
        .into_any()
}

/// The status line while repairs are proposed for `names`.
pub fn proposing_status(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("Making the repair plan for {one}… a large source set can take a while."),
        _ => format!(
            "Making repair plans for {}… a large source set can take a while.",
            names.join(", ")
        ),
    }
}

#[derive(Clone, PartialEq)]
struct SetRow {
    summary: SourceSetSummary,
    status: SourceSetStatus,
}

impl SetRow {
    fn name(&self) -> &str {
        &self.summary.set.name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetCol {
    Status,
    Name,
    Games,
    Coverage,
    Dat,
    Audited,
}

/// The source set's game counts by status, each an icon and its number.
fn game_counts(games: &BTreeMap<GameStatus, usize>) -> AnyView {
    view! {
        <span class="game-counts">
            {GameStatus::ALL
                .into_iter()
                .map(|s| {
                    view! {
                        <span class="game-count">
                            <StatusIcon status=s tab_stop=false />
                            <span class="n">{games.get(&s).copied().unwrap_or(0)}</span>
                        </span>
                    }
                })
                .collect_view()}
        </span>
    }
    .into_any()
}

fn coverage_label(s: &SourceSetSummary) -> String {
    coverage_model::label(s.set.coverage.as_deref())
}

/// What a source set's coverage is, for its tooltip: every region it names, and whether it could be applied.
fn coverage_tip(s: &SourceSetSummary) -> String {
    let coverage = s.set.coverage.as_deref();
    let (_, kind) = coverage::glyph(coverage, s.coverage_unavailable);
    let regions = coverage_model::regions_line(coverage);
    match (coverage, s.coverage_unavailable) {
        (None, _) => regions,
        (Some(_), true) => format!("{kind}: {regions} {UNAVAILABLE}"),
        (Some(_), false) => format!("{kind}: {regions}"),
    }
}

/// A source set's coverage in its list cell: its icon and short label, every region in the tooltip.
fn coverage_cell(s: &SourceSetSummary) -> AnyView {
    let coverage = s.set.coverage.as_deref();
    let (glyph, _) = coverage::glyph(coverage, s.coverage_unavailable);
    let label = coverage_model::label(coverage);
    view! {
        <CellTip label=coverage_tip(s)>
            <Svg glyph />
            " "
            {label}
        </CellTip>
    }
    .into_any()
}

impl ListRow for SetRow {
    type Col = SetCol;
    const COLUMNS: &'static [SetCol] = &[
        SetCol::Status,
        SetCol::Name,
        SetCol::Games,
        SetCol::Coverage,
        SetCol::Dat,
        SetCol::Audited,
    ];

    fn column(col: SetCol) -> Column {
        match col {
            SetCol::Status => Column::new("status", "Status", Width::Fixed(6.0))
                .sortable()
                .hint("Running, repair open, needs review, stale, verified or never audited"),
            SetCol::Name => Column::new(
                "name",
                "Source set",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            SetCol::Games => Column::new("games", "Games", Width::Fixed(15.0)).hint(
                "Complete, incomplete and missing games in its latest audit, in its coverage only",
            ),
            SetCol::Coverage => Column::new("coverage", "Coverage", Width::Fixed(9.0))
                .sortable()
                .hint("The regions whose games it keeps"),
            SetCol::Dat => Column::new(
                "dat",
                "DAT",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            SetCol::Audited => {
                Column::new("audited", "Latest audit", Width::Fixed(13.0)).sortable()
            }
        }
    }

    fn key(&self) -> String {
        self.summary.set.name.clone()
    }

    fn title(&self) -> String {
        self.summary.set.name.clone()
    }

    fn search(&self) -> String {
        let set = &self.summary.set;
        let causes: Vec<&str> = model::stale_causes(&set.stale)
            .into_iter()
            .map(|(cause, _)| cause.look().label)
            .collect();
        format!(
            "{} {} {} {} {} {} {}",
            set.name,
            coverage_model::label(set.coverage.as_deref()),
            set.dat,
            set.root,
            set.path,
            self.status.look().label,
            causes.join(" ")
        )
    }

    fn cell(&self, col: SetCol) -> AnyView {
        let s = &self.summary;
        match col {
            SetCol::Status => {
                let causes =
                    (self.status == SourceSetStatus::Stale).then(|| stale_icons(&s.set.stale));
                view! {
                    <StatusIcon status=self.status tab_stop=false />
                    {causes}
                }
                .into_any()
            }
            SetCol::Name => view! { <Clip text=s.set.name.clone() /> }.into_any(),
            SetCol::Games => match &s.latest {
                Some(audit) => game_counts(&audit.games),
                None => "—".into_any(),
            },
            SetCol::Coverage => coverage_cell(s),
            SetCol::Dat => view! { <Clip text=s.set.dat.clone() /> }.into_any(),
            SetCol::Audited => match &s.latest {
                Some(audit) => view! { <Clip text=when(&audit.finished_at) /> }.into_any(),
                None => "Never".into_any(),
            },
        }
    }

    fn compare(&self, other: &Self, col: SetCol) -> Ordering {
        let (a, b) = (&self.summary, &other.summary);
        match col {
            SetCol::Status => self.status.rank().cmp(&other.status.rank()),
            SetCol::Name | SetCol::Games => a.set.name.cmp(&b.set.name),
            SetCol::Coverage => coverage_label(a)
                .cmp(&coverage_label(b))
                .then_with(|| a.set.name.cmp(&b.set.name)),
            SetCol::Dat => a.set.dat.cmp(&b.set.dat),
            SetCol::Audited => a
                .latest
                .as_ref()
                .map(|l| &l.finished_at)
                .cmp(&b.latest.as_ref().map(|l| &l.finished_at)),
        }
    }

    fn href(&self) -> Option<String> {
        Some(set_href(self.name()))
    }
}

/// What a list action reports to, and what it changes.
#[derive(Clone, Copy)]
struct Ctx {
    said: Reports,
    proposing: RwSignal<Vec<String>>,
    refresh: RwSignal<u64>,
}

impl Ctx {
    /// Proposes a repair of each of `names` in turn, saying how each went.
    fn propose(self, names: Vec<String>) {
        let names: Vec<String> = names
            .into_iter()
            .filter(|n| !self.proposing.with_untracked(|p| p.contains(n)))
            .collect();
        if names.is_empty() {
            return;
        }
        self.proposing.update(|p| p.extend(names.iter().cloned()));
        self.said.clear();
        leptos::task::spawn_local(async move {
            for name in names {
                self.said.add(match propose_repair(&name).await {
                    Ok(plan) => Report::Made {
                        text: format!(
                            "Proposed repair #{} of {name}: {} items to review.",
                            plan.id,
                            plan.content.items.len()
                        ),
                        link: Some(Link::new(
                            repair_plan_href(&name, plan.id),
                            "Open the repair",
                        )),
                    },
                    Err(problem) => {
                        Report::Refused(format!("Couldn't propose a repair of {name}: {problem}"))
                    }
                });
                self.proposing.update(|p| p.retain(|n| *n != name));
            }
            self.refresh.update(|n| *n += 1);
        });
    }
}

fn set_actions(row: &SetRow, ctx: Ctx) -> Vec<RowAction> {
    let name = row.name().to_string();
    let mut actions = vec![{
        let name = name.clone();
        RowAction::run(Glyph::ShieldCheck, format!("Audit {name}"), move || {
            ctx.said.clear();
            start_audit(name.clone(), ctx.said)
        })
    }];
    if row.summary.latest.is_some() {
        let name = name.clone();
        actions.push(RowAction::run(
            Glyph::ScanSearch,
            "Check for changes",
            move || {
                ctx.said.clear();
                start_check(name.clone(), ctx.said)
            },
        ));
    }
    actions.push(match &row.summary.repair {
        Some(open) => RowAction::go(
            Glyph::Wrench,
            format!("Review its repair #{}", open.id),
            repair_plan_href(&name, open.id),
        ),
        None => {
            let name = name.clone();
            RowAction::run(Glyph::Wrench, "Propose a repair", move || {
                ctx.propose(vec![name.clone()])
            })
        }
    });
    actions.extend([
        RowAction::go(Glyph::Eye, "Preview a selection", preview_href(&name)),
        RowAction::go(
            Glyph::Library,
            "New library folder",
            new_library_href(&name),
        ),
        RowAction::go(
            Glyph::FolderTree,
            "Its library folders",
            tab_href(&name, PageTab::Libraries),
        ),
        RowAction::go(
            Glyph::CirclePlus,
            "Its additions",
            tab_href(&name, PageTab::Additions),
        ),
        RowAction::go(
            Glyph::History,
            "Its history",
            tab_href(&name, PageTab::History),
        ),
        RowAction::go(
            Glyph::Archive,
            "Its kept files",
            tab_href(&name, PageTab::Kept),
        ),
    ]);
    actions
}

/// What the row's cells don't say: why it stands as it does, where it is, its latest audit's revision and what it
/// left to review, and its open repair. Its status, DAT, coverage, games and when it was audited are in the cells.
fn set_detail(row: SetRow) -> AnyView {
    let s = row.summary;
    let outside = s
        .outside_coverage
        .as_ref()
        .filter(|_| !s.coverage_unavailable)
        .map(|o| view! { <p class="note">{coverage_model::outside_line(o)}</p> });
    let unavailable = (s.coverage_unavailable && s.set.coverage.is_some())
        .then(|| view! { <p class="note small">{UNAVAILABLE}</p> });
    view! {
        {stale_lines(&s.set)}
        <dl class="facts">
            <dt>"Root and path"</dt>
            <dd>
                <code class="path">{format!("{}: {}", s.set.root, s.set.path)}</code>
            </dd>
            <dt>"Registered"</dt>
            <dd>{when(&s.set.added_at)}</dd>
            {s.latest.as_ref().map(|audit| {
                let review = needs_review_from_counts(&audit.findings);
                view! {
                    <dt>"Latest audit"</dt>
                    <dd>
                        <A href=format!("/audits/{}", audit.id)>{format!("#{}", audit.id)}</A>
                        {format!(", against {}; {review} need review", audit.dat_revision_version)}
                    </dd>
                }
            })}
            <dt>"Last verified against"</dt>
            <dd>
                {s.set
                    .last_verified_against
                    .as_ref()
                    .map(|l| view! {
                        <super::dats::RevisionName sha256=l.sha256.clone() version=l.version.clone() />
                    }.into_any())
                    .unwrap_or_else(|| "Never".into_any())}
            </dd>
        </dl>
        {unavailable}
        {outside}
    }
    .into_any()
}

/// Why the source set is stale, one line each with its cause's icon, and whether it follows its DAT.
pub fn stale_lines(set: &SourceSetRecord) -> AnyView {
    let causes = model::stale_causes(&set.stale);
    (!causes.is_empty())
        .then(|| {
            view! {
                <ul class="plain reasons">
                    {causes
                        .into_iter()
                        .map(|(cause, line)| {
                            view! {
                                <li>
                                    <StatusIcon status=cause tab_stop=false />
                                    " "
                                    {line}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
        })
        .into_any()
}

async fn summaries() -> Result<Vec<SourceSetSummary>, String> {
    api::get("/source-set-summaries").await
}

/// The registered source sets with their status, every action on them, and the form to register another.
#[component]
pub fn SourceSets() -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        summaries()
    });
    let latest = last_good_rows(move || loaded.get());
    let rows = Memo::new(move |_| {
        let running = live
            .book
            .with(|b| model::running(b.newest_first().iter().map(|l| &l.job)));
        let rows: Vec<SetRow> = latest.with(|sets| {
            model::checked_in(sets, &running)
                .into_iter()
                .map(|(status, summary)| SetRow { status, summary })
                .collect()
        });
        Arc::new(rows)
    });
    let statuses = Signal::derive(move || present(rows.get().iter().map(|r| r.status)));
    let (only, show_only) = use_status_filter::<SourceSetStatus>("status");
    let counts = Signal::derive(move || rows.with(|r| kind_counts(r.iter().map(|r| r.status))));
    let keep = Signal::derive(move || -> Keep<SetRow> {
        only.get().map(|status| {
            let keep: Arc<dyn Fn(&SetRow) -> bool + Send + Sync> =
                Arc::new(move |r: &SetRow| r.status == status);
            keep
        })
    });
    let ctx = Ctx {
        said: Reports::new(),
        proposing: RwSignal::new(Vec::new()),
        refresh,
    };
    let registering = RwSignal::new(false);
    let go = navigator();
    // Previewing a selection or making a library folder is about one source set: the first checked.
    let one = move |chosen: Vec<SetRow>, href: fn(&str) -> String| match chosen.as_slice() {
        [row] => go.run(href(row.name())),
        [row, ..] => {
            ctx.said.clear();
            ctx.said.add(Report::Made {
                text: format!(
                    "That works on one source set at a time: going on with {}.",
                    row.name()
                ),
                link: None,
            });
            go.run(href(row.name()));
        }
        [] => {}
    };
    let actions: Vec<ToolAction<SetRow>> = vec![
        ToolAction::page(Glyph::Plus, "Register", move || registering.set(true)),
        ToolAction::<SetRow>::checked(Glyph::ShieldCheck, "Audit", move |chosen| {
            ctx.said.clear();
            for row in chosen.rows {
                start_audit(row.name().to_string(), ctx.said);
            }
        }),
        ToolAction::<SetRow>::checked(Glyph::ScanSearch, "Check for changes", move |chosen| {
            ctx.said.clear();
            for row in chosen.rows {
                start_check(row.name().to_string(), ctx.said);
            }
        }),
        ToolAction::<SetRow>::checked(Glyph::Wrench, "Propose repair", move |chosen| {
            ctx.propose(chosen.rows.iter().map(|r| r.name().to_string()).collect())
        }),
        ToolAction::<SetRow>::checked(Glyph::Eye, "Preview selection", move |chosen| {
            one(chosen.rows, preview_href)
        }),
        ToolAction::<SetRow>::checked(Glyph::Library, "New library folder", move |chosen| {
            one(chosen.rows, new_library_href)
        }),
    ];

    view! {
        <h1>"Source sets"</h1>
        <KindCounts counts active=only on_filter=show_only label="Source sets by status" />
        {move || match loaded.get() {
            None => view! { <Loading what="the source sets" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <Show when=move || ctx.proposing.with(|p| !p.is_empty())>
            <PendingStatus status=Signal::derive(move || ctx.proposing.with(|p| proposing_status(p))) />
        </Show>
        <ShowReports reports=ctx.said />
        <List
            id="source-sets"
            label="Source sets"
            rows
            empty="No source sets yet. Choose Register to add a folder in a root."
            placeholder="Filter by name, DAT, root, path or status"
            actions
            row_actions=Callback::new(move |row: SetRow| set_actions(&row, ctx))
            detail=Callback::new(set_detail)
            keep
            legend=move || view! { <Legend statuses list="source-sets" /> }
        />
        <Help summary="What a source set is">
            <p>
                "A source set is a folder in a root, audited against its DAT. The list shows those that need a look \
                first, then the rest by name; choose a column to sort by it instead. Open one for its games, \
                entries, library folders, history and repair."
            </p>
        </Help>
        <Drawer open=registering title="Register a source set">
            <Register on_added=Callback::new(move |name: String| {
                registering.set(false);
                go.run(set_href(&name))
            }) />
        </Drawer>
    }
}

/// The form to register a Source Set; `on_added` gets its name.
#[component]
pub(crate) fn Register(on_added: Callback<String>) -> impl IntoView {
    let roots = LocalResource::new(|| async {
        api::get::<Vec<Root>>("/roots")
            .await
            .map(registration::set_roots)
    });
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let defaults = LocalResource::new(|| api::get::<PathTemplateDefaults>("/path-templates"));
    let root = RwSignal::new(String::new());
    let dat = RwSignal::new(String::new());
    let template = RwSignal::new(None::<String>);
    // The path you typed over the proposal; `None` follows the template.
    let typed = RwSignal::new(None::<String>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let draft = RwSignal::new(Draft::every());
    let order = RwSignal::new(Vec::<String>::new());
    let may_change = RwSignal::new(true);
    let create = RwSignal::new(false);

    let proposal = Memo::new(move |_| {
        let Some(template) = template.get() else {
            return Err("Loading the path template…".to_string());
        };
        let dat = dat.get();
        let dats = dats.get().and_then(Result::ok).unwrap_or_default();
        match dats.iter().find(|d| d.dat == dat) {
            Some(d) => registration::propose(&template, d),
            None => Err("Choose a DAT to propose a path.".to_string()),
        }
    });
    let path = Memo::new(move |_| {
        registration::chosen_path(typed.get().as_deref(), &proposal.get()).unwrap_or_default()
    });
    let chosen_dat = Memo::new(move |_| {
        let dat = dat.get();
        dats.get()
            .and_then(Result::ok)
            .and_then(|dats| dats.into_iter().find(|d| d.dat == dat))
    });
    // A MAME ListXML's Source Set covers every Game: the editor is hidden, and starts over at every Game.
    let refused = Memo::new(move |_| registration::no_coverage(chosen_dat.get().as_ref()));
    Effect::new(move || {
        if refused.get().is_some() {
            draft.set(Draft::every());
        }
    });
    let within = Signal::derive(move || {
        roots
            .get()
            .and_then(Result::ok)?
            .into_iter()
            .find(|r| r.name == root.get())
            .map(|r| Within::new(r.path, r.name))
    });
    let preview = move || {
        let root_path = roots
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .find(|r| r.name == root.get())
            .map(|r| r.path)
            .unwrap_or_default();
        let proposal = proposal.get();
        match (typed.get(), &proposal) {
            (None, Err(reason)) => Err(reason.clone()),
            _ => registration::preview(&root_path, &path.get()),
        }
    };

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = path.get_untracked();
        if chosen.trim().is_empty() {
            problem.set(Some(
                "Type a path, or change the template so it proposes one.".into(),
            ));
            return;
        }
        let drafted = order.with_untracked(|o| draft.with_untracked(|d| d.coverage(o)));
        let coverage =
            match registration::coverage_for(chosen_dat.get_untracked().as_ref(), drafted) {
                Ok(coverage) => coverage,
                Err(message) => {
                    problem.set(Some(message));
                    return;
                }
            };
        let request = AddSourceSet {
            root: root.get_untracked(),
            path: Some(chosen),
            template: None,
            dat: dat.get_untracked(),
            coverage,
            repairs_may_change: may_change.get_untracked(),
            create: create.get_untracked(),
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let added = api::post::<_, SourceSetRecord>("/source-sets", &request).await;
            busy.set(false);
            match added {
                // The form may be gone by now.
                Ok(set) => {
                    on_added.try_run(set.name);
                }
                Err(message) => problem.set(Some(message)),
            }
        });
    };

    Effect::new(move || {
        if let Some(Ok(roots)) = roots.get()
            && root.get_untracked().is_empty()
            && let Some(first) = roots.first()
        {
            root.set(first.name.clone());
        }
    });
    Effect::new(move || {
        if let Some(Ok(dats)) = dats.get()
            && dat.get_untracked().is_empty()
            && let Some(first) = dats.first()
        {
            dat.set(first.dat.clone());
        }
    });
    Effect::new(move || {
        if let Some(Ok(defaults)) = defaults.get()
            && template.get_untracked().is_none()
        {
            template.set(Some(defaults.source_set));
        }
    });

    view! {
        <section aria-label="Register a source set">
            <form class="form" on:submit=submit>
                <label>
                    "Root"
                    <select required prop:value=root on:change=move |ev| root.set(event_target_value(&ev))>
                        {move || {
                            roots
                                .get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|r| {
                                    let label = format!("{} ({})", r.name, r.path);
                                    view! { <option value=r.name>{label}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                {move || {
                    roots
                        .get()
                        .and_then(Result::ok)
                        .filter(Vec::is_empty)
                        .map(|_| {
                            view! {
                                <p class="note">
                                    "A source set goes only in a source sets root, and you have none. Register \
                                     one under Settings, then Roots."
                                </p>
                            }
                        })
                }}
                <label>
                    "DAT"
                    <select required prop:value=dat on:change=move |ev| dat.set(event_target_value(&ev))>
                        {move || {
                            dats.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|d| view! { <option value=d.dat.clone()>{d.dat.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Path template"
                    <input
                        type="text"
                        class="data"
                        title="Tokens: {provider}, {dat} and {platform}"
                        prop:value=move || template.get().unwrap_or_default()
                        on:input=move |ev| template.set(Some(event_target_value(&ev)))
                    />
                </label>
                <div class="path-group">
                    <label for="set-path">"Path in the root"</label>
                    <div class="path-field">
                        <input
                            id="set-path"
                            type="text"
                            class="data"
                            placeholder="Type a path"
                            prop:value=path
                            on:input=move |ev| typed.set(Some(event_target_value(&ev)))
                        />
                        <FolderPicker
                            field="the source set's path"
                            value=path
                            within=within
                            disabled=Signal::derive(move || within.with(Option::is_none))
                            on_choose=move |chosen: String| typed.set(Some(chosen))
                            create=Some(Create {
                                label: CREATE_SET,
                                run: Callback::new(move |chosen: String| {
                                    typed.set(Some(chosen));
                                    create.set(true);
                                }),
                            })
                        />
                    </div>
                </div>
                {move || match refused.get() {
                    Some(why) => view! { <p class="note">{why}</p> }.into_any(),
                    None => view! { <CoverageEditor draft order /> }.into_any(),
                }}
                <Choice
                    checked=may_change
                    on_change=Callback::new(move |on| may_change.set(on))
                    note=|| MAY_CHANGE_NOTE
                >
                    {MAY_CHANGE}
                </Choice>
                <Choice checked=create on_change=Callback::new(move |on| create.set(on)) note=|| CREATE_SET_NOTE>
                    {CREATE_SET}
                </Choice>
                {move || {
                    (typed.get().is_some() && proposal.get().is_ok())
                        .then(|| {
                            view! {
                                <button type="button" on:click=move |_| typed.set(None)>
                                    "Use the proposed path"
                                </button>
                            }
                        })
                }}
                <button type="submit" class="primary" disabled=busy>
                    "Register"
                </button>
            </form>
            <p class="note" aria-live="polite">
                {move || match preview() {
                    Ok(folder) => view! { "Registers " <code>{folder}</code> }.into_any(),
                    Err(reason) => reason.into_any(),
                }}
            </p>
            <p class="note small">
                "The path template proposes a path from the DAT: {provider} is its provider, {dat} its name and \
                {platform} its front-end folder name. Change the template or type over the path. A registered \
                source set keeps its path when the template changes."
            </p>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}
