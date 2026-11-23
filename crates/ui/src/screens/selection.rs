use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use stage_select_api::selection::{
    ARCADE_COVERAGE_REFUSAL, ChoiceStatus, Dependency, Outcome, ReleaseChange, ReleaseRow,
    SelectionGroup, SelectionProfile,
};
use stage_select_api::{
    Availability, GameStatus, Job, ProfileRecord, ProfileVersion, SaveProfile, SavedProfile,
    SelectionDiff, SelectionRecord, SourceSetRecord, StartSelection, StartSelectionFor,
};

use crate::api;
use crate::app::Loading;
use crate::arcade_model::{self, Needs};
use crate::model::{game_status_label, when};
use crate::parts::help::Help;
use crate::parts::icon::{Glyph, Svg};
use crate::parts::layout::Crumbs;
use crate::parts::legend::{LegendStrip, present, present_looks};
use crate::parts::list::{Clip, Column, Keep, List, ListRow, RowAction, Width};
use crate::parts::message::{Live, Message, MessageKind, Problem};
use crate::parts::outcome::JobOutcome;
use crate::parts::overlay::Tooltip;
use crate::parts::status::{
    AvailabilityStatus, GameFlag, GroupStatus, SelectionFlag, Status, StatusBadge, StatusCount,
    StatusIcon,
};
use crate::parts::url_state::use_list_query;
use crate::placement_model::FlagsNow;
use crate::screens::PlanABuild;
use crate::screens::audit::url_select;
use crate::screens::audit::{ReAudit, flags_notice, flags_now, placements_now};
use crate::selection_model::{
    AVAILABILITY_KEY, FLAG_KEY, FlagFilter, GroupAvailability, OUTCOME_KEY, ReleaseColumn,
    ReleaseFilter, SelectionIndex, UNKNOWN, checked, confirm, confirm_unresolved, flag_statuses,
    group_status, includable, include_and_check, kept_need_review, left_out_text, listed,
    rule_text, staged_changes, toggle, unresolved_explanation,
};

/// A release in a Selection's list, by its index.
#[derive(Clone)]
pub struct ReleaseItem {
    index: Arc<SelectionIndex>,
    i: u32,
}

// A Selection never changes once indexed, so a row is the same while it points at the same release.
impl PartialEq for ReleaseItem {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.index, &other.index) && self.i == other.i
    }
}

impl ReleaseItem {
    fn row(&self) -> &ReleaseRow {
        &self.index.rows[self.i as usize]
    }

    fn availability(&self) -> Option<AvailabilityStatus> {
        let i = self.i as usize;
        self.index
            .availability(i)
            .map(|s| AvailabilityStatus::of(s, self.index.claimed(i)))
    }

    fn flags(&self) -> Vec<SelectionFlag> {
        flag_statuses(self.index.flags(self.i as usize))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseCol {
    Outcome,
    Group,
    Release,
    Availability,
    Flags,
}

fn outcome_view(outcome: Outcome) -> AnyView {
    view! { <StatusIcon status=outcome tab_stop=false /> }.into_any()
}

/// What you have of a Game, as its icon in a cell, or as a badge with its label.
fn availability_view(status: Option<GameStatus>, claimed: bool, badge: bool) -> AnyView {
    match status.map(|s| AvailabilityStatus::of(s, claimed)) {
        Some(status) if badge => view! { <StatusBadge status /> }.into_any(),
        Some(status) => view! { <StatusIcon status tab_stop=false /> }.into_any(),
        None => ().into_any(),
    }
}

impl ListRow for ReleaseItem {
    type Col = ReleaseCol;
    const COLUMNS: &'static [ReleaseCol] = &[
        ReleaseCol::Outcome,
        ReleaseCol::Group,
        ReleaseCol::Release,
        ReleaseCol::Availability,
        ReleaseCol::Flags,
    ];

    fn column(col: ReleaseCol) -> Column {
        match col {
            ReleaseCol::Outcome => Column::new("status", "Status", Width::Fixed(4.5))
                .sortable()
                .hint("Whether the selection keeps the game"),
            ReleaseCol::Group => Column::new(
                "group",
                "Game group",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            ReleaseCol::Release => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
            ReleaseCol::Availability => Column::new("availability", "Have", Width::Fixed(4.5))
                .sortable()
                .hint("What you have of the game, from the latest audit"),
            ReleaseCol::Flags => Column::new("flags", "Flags", Width::Fixed(6.0))
                .hint("What needs your attention in its game group"),
        }
    }

    fn key(&self) -> String {
        self.row().title.clone()
    }

    fn title(&self) -> String {
        self.row().title.clone()
    }

    fn search(&self) -> String {
        self.index.search(self.i as usize).to_string()
    }

    fn cell(&self, col: ReleaseCol) -> AnyView {
        match col {
            ReleaseCol::Outcome => outcome_view(self.row().outcome),
            ReleaseCol::Group => view! { <Clip text=self.row().group.clone() /> }.into_any(),
            ReleaseCol::Release => view! { <Clip text=self.row().title.clone() /> }.into_any(),
            ReleaseCol::Availability => self
                .availability()
                .map(|status| view! { <StatusIcon status tab_stop=false /> })
                .into_any(),
            ReleaseCol::Flags => self
                .flags()
                .into_iter()
                .map(|status| view! { <StatusIcon status tab_stop=false /> " " })
                .collect_view()
                .into_any(),
        }
    }

    fn compare(&self, other: &Self, col: ReleaseCol) -> std::cmp::Ordering {
        let column = match col {
            ReleaseCol::Outcome => ReleaseColumn::Outcome,
            ReleaseCol::Group => ReleaseColumn::Group,
            ReleaseCol::Release => ReleaseColumn::Release,
            ReleaseCol::Availability => ReleaseColumn::Availability,
            ReleaseCol::Flags => return std::cmp::Ordering::Equal,
        };
        self.index
            .compare(column, self.i as usize, other.i as usize)
    }
}

/// Why a release has its outcome: the rule's words, and where the rule comes from when it says.
fn rule_view(rule: &stage_select_api::selection::Rule) -> impl IntoView + use<> {
    let reference = rule.reference.clone();
    view! {
        <dt>"Why"</dt>
        <dd>
            {rule_text(rule)}
            {reference.map(|r| view! { <p class="note small">{r}</p> })}
        </dd>
    }
}

fn release_detail(item: &ReleaseItem) -> AnyView {
    let row = item.row().clone();
    let flags = item.flags();
    view! {
        <dl class="facts">
            <dt>"Game group"</dt>
            <dd>{row.group.clone()}</dd>
            <dt>"Status"</dt>
            <dd><StatusBadge status=row.outcome /></dd>
            {rule_view(&row.rule)}
            {item
                .availability()
                .map(|status| view! {
                    <dt>"What you have"</dt>
                    <dd><StatusBadge status /></dd>
                })}
            {(!flags.is_empty())
                .then(|| view! {
                    <dt>"Flags"</dt>
                    <dd>
                        {flags
                            .into_iter()
                            .map(|status| view! { <StatusBadge status /> " " })
                            .collect_view()}
                    </dd>
                })}
        </dl>
    }
    .into_any()
}

/// A Selection's releases in the shared list, with filters by status, availability and flag in the URL beside its
/// text. Filtering and sorting run in the browser on every keystroke.
#[component]
pub fn ReleasesPanel(
    index: Arc<SelectionIndex>,
    /// Whether an Audit says which releases are available.
    audited: bool,
) -> impl IntoView {
    let rows = Arc::new(
        (0..index.len() as u32)
            .map(|i| ReleaseItem {
                index: index.clone(),
                i,
            })
            .collect::<Vec<_>>(),
    );
    let mut looks = present_looks(rows.iter().map(|r| r.row().outcome));
    looks.extend(present_looks(
        rows.iter().filter_map(ReleaseItem::availability),
    ));
    looks.extend(present_looks(rows.iter().flat_map(ReleaseItem::flags)));
    let query = use_query_map();
    let filter = Memo::new(move |_| query.with(|q| ReleaseFilter::from_query(|k| q.get_str(k))));
    let keep: Signal<Keep<ReleaseItem>> = Signal::derive(move || {
        let filter = filter.get();
        (filter != ReleaseFilter::default()).then(|| {
            Arc::new(move |row: &ReleaseItem| row.index.keeps(row.i as usize, &filter))
                as Arc<dyn Fn(&ReleaseItem) -> bool + Send + Sync>
        })
    });
    let filters = move || {
        view! {
            {url_select(
                "Status",
                OUTCOME_KEY,
                Outcome::ALL.iter().map(|o| (o.as_str(), o.look().label)).collect(),
                false,
            )}
            {url_select(
                "Availability",
                AVAILABILITY_KEY,
                GameStatus::ALL
                    .iter()
                    .map(|s| (s.as_str(), game_status_label(*s)))
                    .chain([(UNKNOWN, "Not in the audit")])
                    .collect(),
                !audited,
            )}
            {url_select(
                "Flag",
                FLAG_KEY,
                FlagFilter::ALL.iter().map(|f| (f.key(), f.label())).collect(),
                false,
            )}
        }
    };
    view! {
        <List
            id="releases"
            label="Games"
            rows=Signal::stored(rows)
            empty="This selection lists no games."
            placeholder="Filter by game group or game name"
            filters
            keep
            legend=move || view! { <LegendStrip looks=Signal::stored(looks.clone()) list="releases" /> }
            detail=Callback::new(|row: ReleaseItem| release_detail(&row))
        />
    }
}

/// A change between two Selections, in a list.
#[derive(Clone, PartialEq)]
struct ChangeItem(ReleaseChange);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeCol {
    Before,
    After,
    Group,
    Release,
}

fn maybe_outcome(outcome: Option<Outcome>) -> AnyView {
    match outcome {
        Some(outcome) => outcome_view(outcome),
        None => view! { <span class="tag">"Not in the DAT"</span> }.into_any(),
    }
}

impl ListRow for ChangeItem {
    type Col = ChangeCol;
    const COLUMNS: &'static [ChangeCol] = &[
        ChangeCol::Before,
        ChangeCol::After,
        ChangeCol::Group,
        ChangeCol::Release,
    ];

    fn column(col: ChangeCol) -> Column {
        match col {
            ChangeCol::Before => Column::new("before", "Before", Width::Fixed(8.0))
                .sortable()
                .hint("Its status in the selection it is compared with"),
            ChangeCol::After => Column::new("now", "Now", Width::Fixed(8.0))
                .sortable()
                .hint("Its status now"),
            ChangeCol::Group => Column::new(
                "group",
                "Game group",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            ChangeCol::Release => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable(),
        }
    }

    fn key(&self) -> String {
        self.0.title.clone()
    }

    fn title(&self) -> String {
        self.0.title.clone()
    }

    fn search(&self) -> String {
        format!("{}\n{}", self.0.group, self.0.title)
    }

    fn cell(&self, col: ChangeCol) -> AnyView {
        let change = &self.0;
        match col {
            ChangeCol::Before => maybe_outcome(change.before),
            ChangeCol::After => maybe_outcome(change.after),
            ChangeCol::Group => view! { <Clip text=change.group.clone() /> }.into_any(),
            ChangeCol::Release => view! { <Clip text=change.title.clone() /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: ChangeCol) -> std::cmp::Ordering {
        let (a, b) = (&self.0, &other.0);
        match col {
            ChangeCol::Before => a.before.cmp(&b.before),
            ChangeCol::After => a.after.cmp(&b.after),
            ChangeCol::Group => a.group.to_lowercase().cmp(&b.group.to_lowercase()),
            ChangeCol::Release => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        }
    }
}

fn change_detail(item: &ChangeItem) -> AnyView {
    let change = &item.0;
    let outcome = |o: Option<Outcome>| match o {
        Some(status) => view! { <StatusBadge status /> }.into_any(),
        None => "Not in the DAT".into_any(),
    };
    view! {
        <dl class="facts">
            <dt>"Game group"</dt>
            <dd>{change.group.clone()}</dd>
            <dt>"Before"</dt>
            <dd>{outcome(change.before)}</dd>
            <dt>"Now"</dt>
            <dd>{outcome(change.after)}</dd>
            {rule_view(&change.rule)}
        </dl>
    }
    .into_any()
}

/// What changed, release by release, in a list whose state lives under the URL's `changes-` keys, beside the page's
/// own list.
#[component]
pub fn ChangesTable(id: &'static str, changes: Vec<ReleaseChange>) -> impl IntoView {
    let looks = present_looks(changes.iter().flat_map(|c| [c.before, c.after]).flatten());
    let rows = Arc::new(changes.into_iter().map(ChangeItem).collect::<Vec<_>>());
    view! {
        <List
            id
            scope="changes"
            label="Changes"
            rows=Signal::stored(rows)
            empty="No game changes its outcome."
            placeholder="Filter by game group or game name"
            legend=move || view! { <LegendStrip looks=Signal::stored(looks.clone()) list=id /> }
            detail=Callback::new(|row: ChangeItem| change_detail(&row))
        />
    }
}

/// Makes a preview, a Profile's Selection from a Source Set that no Library Folder uses yet, on a page of its own.
/// Opened from a Source Set (`?source_set=`, pick a Profile) or a Profile (`?profile=`, pick a Source Set).
#[component]
pub fn NewSelection() -> impl IntoView {
    let query = use_query_map();
    let given = |key: &'static str| query.read_untracked().get(key).filter(|v| !v.is_empty());
    let back = match (given("profile"), given("source_set")) {
        (Some(p), _) => Some((format!("/profiles/{}", api::segment(&p)), p)),
        (None, Some(s)) => Some((format!("/source-sets/{}", api::segment(&s)), s)),
        (None, None) => None,
    };
    view! {
        <h1>"Preview a selection"</h1>
        {back.map(|(href, name)| view! { <p><A href=href>{format!("Back to {name}")}</A></p> })}
        <SelectionForm profile=given("profile") source_set=given("source_set") />
    }
}

/// The form that makes a preview, starting at `profile` and `source_set` when given, and the outcome of its job,
/// which links to the selection once made. In a drawer, or on its page.
#[component]
pub fn SelectionForm(profile: Option<String>, source_set: Option<String>) -> impl IntoView {
    let source_set = RwSignal::new(source_set.unwrap_or_default());
    let profile = RwSignal::new(profile.unwrap_or_default());
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let request = StartSelection {
            source_set: source_set.get_untracked(),
            revision: None,
            profile: profile.get_untracked(),
            version: None,
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, Job>("/selections", &request).await));
            busy.set(false);
        });
    };
    let ready = move || !source_set.get().is_empty() && !profile.get().is_empty() && !busy.get();

    view! {
        <form class="form" on:submit=submit>
            <label>
                "Source set"
                <select
                    required
                    prop:value=move || source_set.get()
                    on:change=move |ev| source_set.set(event_target_value(&ev))
                >
                    <option value="">"Choose a source set"</option>
                    {move || {
                        sets.get()
                            .and_then(Result::ok)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|s| {
                                let selected = s.name == source_set.get_untracked();
                                view! { <option value=s.name.clone() selected=selected>{s.name.clone()}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <label>
                "Selection profile"
                <select
                    required
                    prop:value=move || profile.get()
                    on:change=move |ev| profile.set(event_target_value(&ev))
                >
                    <option value="">"Choose a selection profile"</option>
                    {move || {
                        profiles
                            .get()
                            .and_then(Result::ok)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|p| p.latest.is_some() && p.retired_at.is_none())
                            .map(|p| {
                                let selected = p.name == profile.get_untracked();
                                let label = format!(
                                    "{} (version {})",
                                    p.name,
                                    p.latest.as_ref().map_or(0, |v| v.version),
                                );
                                view! { <option value=p.name.clone() selected=selected>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <button type="submit" class="primary" disabled=move || !ready()>
                {move || if busy.get() { "Queueing…" } else { "Make the selection" }}
            </button>
        </form>
        {move || match (sets.get(), profiles.get()) {
            (Some(Err(message)), _) | (_, Some(Err(message))) => {
                view! { <Problem message /> }.into_any()
            }
            _ => ().into_any(),
        }}
        {move || {
            outcome
                .get()
                .map(|outcome| match outcome {
                    Ok(job) => view! { <JobOutcome job=job.id seed=job /> }.into_any(),
                    Err(message) => view! { <Problem message /> }.into_any(),
                })
        }}
        <Help summary="A preview changes no library folder">
            <p>
                "A preview is a selection that shows what a selection profile would keep from a source set, \
                without changing any library folder. It's listed on its profile until a library folder uses it."
            </p>
        </Help>
    }
}

#[derive(Clone)]
struct Loaded {
    index: Arc<SelectionIndex>,
    availability: Availability,
}

async fn load(id: i64) -> Result<Loaded, String> {
    let selection = api::get(&format!("/selections/{id}/selection")).await?;
    let availability: Availability = api::get(&format!("/selections/{id}/availability")).await?;
    let index = SelectionIndex::new(selection, &availability.games, &availability.claimed);
    Ok(Loaded {
        index: Arc::new(index),
        availability,
    })
}

/// One Selection: what the engine and your choices selected, why, and what of it you have.
#[component]
pub fn SelectionScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let record = LocalResource::new(move || {
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<SelectionRecord>(&format!("/selections/{id}")).await,
                None => Err("That is not a selection number.".into()),
            }
        }
    });
    move || match record.get() {
        None => view! { <Loading what="the selection" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Selection"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(record)) => view! { <SelectionView record /> }.into_any(),
    }
}

/// One game group of a Selection as its list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupItem {
    name: String,
    status: GroupStatus,
    /// The Games kept.
    kept: Vec<String>,
    releases: usize,
    /// The rule that decided the first Game kept, or what left the group out.
    rule: String,
    availability: GroupAvailability,
    /// Whether a build flagged a Game it keeps as needing review.
    needs_review: bool,
}

impl GroupItem {
    fn new(group: &SelectionGroup, availability: &Availability) -> Self {
        let first = group.kept.first();
        let rule = match (first, group.left_out.first()) {
            (Some(w), _) => rule_text(&w.rule),
            (None, Some(e)) if e.rule.is_coverage() => rule_text(&e.rule),
            (None, Some(e)) => e.reason.clone(),
            (None, None) => String::new(),
        };
        GroupItem {
            name: group.group.clone(),
            status: group_status(group),
            kept: group.kept.iter().map(|w| w.title.clone()).collect(),
            releases: group.kept.len() + group.clones.len() + group.left_out.len(),
            rule,
            availability: GroupAvailability::of(group, &availability.games, &availability.claimed),
            needs_review: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupCol {
    Status,
    Group,
    Kept,
    Rule,
    Releases,
    Have,
}

impl ListRow for GroupItem {
    type Col = GroupCol;
    const COLUMNS: &'static [GroupCol] = &[
        GroupCol::Status,
        GroupCol::Group,
        GroupCol::Kept,
        GroupCol::Rule,
        GroupCol::Releases,
        GroupCol::Have,
    ];

    fn column(col: GroupCol) -> Column {
        match col {
            GroupCol::Status => Column::new("status", "Status", Width::Fixed(4.0))
                .sortable()
                .hint("Chosen, unresolved, your choice or left out"),
            GroupCol::Group => Column::new(
                "group",
                "Game group",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .sortable(),
            GroupCol::Kept => Column::new(
                "kept",
                "Kept",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable()
            .hint("The games the selection keeps"),
            GroupCol::Rule => Column::new(
                "rule",
                "Rule",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .hint("The rule that chose the kept game, or what left the game group out"),
            GroupCol::Releases => Column::new("releases", "Versions", Width::Fixed(6.0))
                .sortable()
                .numeric()
                .hint("How many versions the game group has"),
            GroupCol::Have => Column::new("have", "You have", Width::Fixed(11.0))
                .sortable()
                .hint("The worst of the kept games, from the latest audit"),
        }
    }

    fn key(&self) -> String {
        self.name.clone()
    }

    fn title(&self) -> String {
        self.name.clone()
    }

    fn search(&self) -> String {
        format!("{} {} {}", self.name, self.kept.join(" "), self.rule)
    }

    fn cell(&self, col: GroupCol) -> AnyView {
        match col {
            GroupCol::Status => {
                view! { <StatusIcon status=self.status tab_stop=false /> }.into_any()
            }
            GroupCol::Group => view! { <Clip text=self.name.clone() /> }.into_any(),
            GroupCol::Kept => {
                let kept = if self.kept.is_empty() {
                    "Nothing".to_string()
                } else {
                    self.kept.join(", ")
                };
                view! { <Clip text=kept /> }.into_any()
            }
            GroupCol::Rule => view! { <Clip text=self.rule.clone() /> }.into_any(),
            GroupCol::Releases => self.releases.to_string().into_any(),
            GroupCol::Have => {
                let flag = self.needs_review.then(
                    || view! { <StatusIcon status=GameFlag::NeedsReview tab_stop=false /> " " },
                );
                view! { {flag} <Clip text=self.availability.label() /> }.into_any()
            }
        }
    }

    fn compare(&self, other: &Self, col: GroupCol) -> std::cmp::Ordering {
        match col {
            GroupCol::Status => self.status.rank().cmp(&other.status.rank()),
            GroupCol::Group => self.name.cmp(&other.name),
            GroupCol::Kept => self.kept.cmp(&other.kept),
            GroupCol::Rule => self.rule.cmp(&other.rule),
            GroupCol::Releases => self.releases.cmp(&other.releases),
            GroupCol::Have => self.availability.label().cmp(&other.availability.label()),
        }
    }
}

#[component]
fn SelectionView(record: SelectionRecord) -> impl IntoView {
    let id = record.id;
    let pruned = record.pruned;
    let record = StoredValue::new(record);
    let r = record.get_value();
    let data =
        LocalResource::new(move || async move { if pruned { None } else { Some(load(id).await) } });
    let diff = LocalResource::new(move || async move {
        if pruned {
            None
        } else {
            api::get::<Option<SelectionDiff>>(&format!("/selections/{id}/diff"))
                .await
                .ok()
                .flatten()
        }
    });
    let version_path = format!(
        "/profiles/{}/versions/{}",
        api::segment(&r.profile.name),
        r.profile.version
    );
    let saved = LocalResource::new(move || {
        let path = version_path.clone();
        async move { api::get::<ProfileVersion>(&path).await }
    });
    let staged = RwSignal::new(None::<SelectionProfile>);
    Effect::new(move || {
        if let Some(Ok(version)) = saved.get()
            && staged.get_untracked().is_none()
        {
            staged.set(Some(version.profile));
        }
    });
    // A link to a group, such as a plan's `?group=`, opens the selection at it.
    let (list_query, set_list_query) = use_list_query();
    if let Some(group) = use_query_map().read_untracked().get("group")
        && list_query.get_untracked().selected.is_none()
    {
        let mut q = list_query.get_untracked();
        q.selected = Some(group);
        set_list_query.run(q);
    }
    let audit_outcome = RwSignal::new(None::<Result<Job, String>>);
    let reaudit = ReAudit::new(
        r.source_set.clone(),
        r.dat_revision_sha256.clone(),
        audit_outcome,
    );
    let newer = RwSignal::new(None::<Result<Job, String>>);
    let start_newer = move |_| {
        let r = record.get_value();
        let request = StartSelection {
            source_set: r.source_set,
            revision: Some(r.dat_revision_sha256),
            profile: r.profile.name,
            version: Some(r.latest_profile_version),
        };
        leptos::task::spawn_local(async move {
            newer.set(Some(api::post::<_, Job>("/selections", &request).await));
        });
    };
    let summary = r.summary;
    let set_href = format!("/source-sets/{}", api::segment(&r.source_set));
    let profile_href = format!("/profiles/{}", api::segment(&r.profile.name));
    let preview = r.library_folders.is_empty();
    let promote_href = format!("/libraries/new?selection={id}");

    let trail = vec![
        ("/source-sets".to_string(), "Source sets".to_string()),
        (set_href.clone(), r.source_set.clone()),
    ];
    view! {
        <Crumbs trail here=format!("Selection #{id}") />
        <h1>{format!("Selection #{id}")}</h1>
        <dl class="facts">
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href>{r.source_set.clone()}</A>
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                <super::dats::RevisionName
                    sha256=r.dat_revision_sha256.clone()
                    version=r.dat_revision_version.clone()
                />
            </dd>
            <dt>"Selection profile"</dt>
            <dd>
                <A href=profile_href>{r.profile.name.clone()}</A>
                {format!(", version {}", r.profile.version)}
            </dd>
            <dt>"Coverage"</dt>
            <dd>
                {match r.pins.arcade {
                    Some(_) => ARCADE_COVERAGE_REFUSAL.to_owned(),
                    None => crate::coverage_model::made_under(r.pins.coverage()),
                }}
            </dd>
            {r.pins.arcade.clone().map(|pins| view! {
                <dt>"Support files"</dt>
                <dd>
                    <ul class="plain">
                        {arcade_model::support_files(&pins)
                            .into_iter()
                            .map(|f| view! {
                                <li>
                                    <code>{f.file}</code>
                                    " "
                                    <StatusBadge status=f.status />
                                    {f.mame_version.map(|v| format!(" {v}"))}
                                    {f.sha256.map(|sha| view! { " · SHA-256 " <code class="path">{sha}</code> })}
                                </li>
                            })
                            .collect_view()}
                    </ul>
                </dd>
                {pins.curated.map(|curated| view! {
                    <dt>"Curated list"</dt>
                    <dd>
                        {arcade_model::curated_line(&curated)}
                        {(!curated.unmatched.is_empty()).then(|| view! {
                            <Help summary="The names it lacks">
                                <ul class="plain">
                                    {curated.unmatched.into_iter().map(|n| view! { <li><code>{n}</code></li> }).collect_view()}
                                </ul>
                            </Help>
                        })}
                    </dd>
                })}
            })}
            <dt>"System"</dt>
            <dd>
                {r.system.clone()} " · "
                {if r.system_settings {
                    "your system settings applied"
                } else {
                    "only the global settings applied"
                }}
            </dd>
            <dt>"Made"</dt>
            <dd>{when(&r.created_at)}</dd>
            <dt>"Library folders"</dt>
            <dd>
                {if preview {
                    view! { "None: this selection is a preview" }.into_any()
                } else {
                    r.library_folders
                        .iter()
                        .enumerate()
                        .map(|(i, folder)| {
                            view! {
                                {(i > 0).then_some(", ")}
                                <A href=format!("/libraries/{folder}")>{format!("Library folder #{folder}")}</A>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </dd>
        </dl>
        {r.pins.arcade.as_ref().map(arcade_model::notices).map(|notices| {
            view! {
                <Message kind=MessageKind::NOTICE live=Live::Off>
                    {notices
                        .into_iter()
                        .map(|notice| view! { <p>{notice}</p> })
                        .collect_view()}
                </Message>
            }
        })}
        {(r.latest_profile_version > r.profile.version)
            .then(|| {
                view! {
                    <Message kind=MessageKind::NOTICE live=Live::Off>
                        <p>
                            {format!(
                                "Version {} of this selection profile is available. This selection was made with version {}.",
                                r.latest_profile_version,
                                r.profile.version,
                            )}
                        </p>
                        <button type="button" on:click=start_newer>
                            {format!("Make a selection with version {}", r.latest_profile_version)}
                        </button>
                    </Message>
                }
            })}
        {(!pruned)
            .then(|| {
                view! {
                    <div class="actions">
                        <A href=promote_href attr:class="button">
                            "Create library folder from this"
                        </A>
                    </div>
                    <PlanABuild selection=record.get_value() />
                }
            })}
        {move || {
            newer
                .get()
                .map(|outcome| match outcome {
                    Ok(job) => view! { <JobOutcome job=job.id seed=job /> }.into_any(),
                    Err(message) => view! { <Problem message /> }.into_any(),
                })
        }}

        {pruned
            .then(|| {
                view! {
                    <ul class="counts">
                        <li class="count">
                            <span class="n">{summary.groups}</span>
                            <span class="what">"Game groups"</span>
                        </li>
                        <StatusCount status=Outcome::Kept>{summary.kept}</StatusCount>
                        // Only the summary's counts are kept, and a group can carry more than one of these flags,
                        // so they show as flags, not as each group's one status.
                        <StatusCount status=SelectionFlag::Unresolved>{summary.unresolved}</StatusCount>
                        <StatusCount status=SelectionFlag::StaleChoice what="Stale choices">
                            {summary.stale}
                        </StatusCount>
                        <StatusCount status=SelectionFlag::ConflictingChoice what="Conflicting choices">
                            {summary.conflicting}
                        </StatusCount>
                    </ul>
                    <p class="note">
                        "This selection is pruned: only its summary is kept, so its list of games is gone."
                    </p>
                }
            })}

        {move || match data.get() {
            None => view! { <Loading what="the game groups" /> }.into_any(),
            Some(None) => ().into_any(),
            Some(Some(Err(message))) => view! { <Problem message /> }.into_any(),
            Some(Some(Ok(loaded))) => {
                view! {
                    <AvailabilityNote audit=loaded.availability.audit reaudit />
                    <StaleGroups index=loaded.index.clone() staged />
                    <Groups loaded staged source_set=record.get_value().source_set />
                    <StagedChoices record staged saved=Signal::derive(move || saved.get().and_then(Result::ok)) />
                }
                    .into_any()
            }
        }}

        {move || match diff.get() {
            None | Some(None) => ().into_any(),
            Some(Some(diff)) => {
                let count = diff.changes.len();
                view! {
                    <section aria-labelledby="changes-title">
                        <h2 id="changes-title">"Changes"</h2>
                        <p class="lede">
                            "Since "
                            <A href=format!("/selections/{}", diff.from)>{format!("selection #{}", diff.from)}</A>
                            {format!(": {count} {} changed outcome.", if count == 1 { "game" } else { "games" })}
                        </p>
                        {(count > 0).then(|| view! { <ChangesTable id="changes" changes=diff.changes /> })}
                    </section>
                }
                    .into_any()
            }
        }}
    }
}

/// Where the availability comes from, or the offer to audit when nothing says.
#[component]
fn AvailabilityNote(audit: Option<i64>, reaudit: ReAudit) -> impl IntoView {
    match audit {
        Some(audit) => view! {
            <p class="lede">
                "What you have is from "
                <A href=format!("/audits/{audit}")>{format!("audit #{audit}")}</A>
                ", the latest audit of this source set against this DAT revision. A kept game that is missing stays missing: to fall back to another version you have, check it."
            </p>
        }
        .into_any(),
        None => view! {
            <Message kind=MessageKind::NOTICE live=Live::Off>
                <p>"Not audited against this revision, so it is unknown which games you have."</p>
                <button type="button" on:click=move |_| reaudit.start()>
                    "Audit against this revision"
                </button>
            </Message>
            {reaudit.outcome()}
        }
        .into_any(),
    }
}

/// The game groups: a strip of counts that narrows the list and confirms every unresolved pick at once, and the
/// list, with the open group's releases beside it.
#[component]
fn Groups(
    loaded: Loaded,
    staged: RwSignal<Option<SelectionProfile>>,
    source_set: String,
) -> impl IntoView {
    let index = loaded.index.clone();
    let flags = flags_now(Some(placements_now(source_set)));
    let shown: Vec<usize> = (0..index.selection.groups.len())
        .filter(|&i| listed(&index.selection.groups[i]))
        .collect();
    let items: Vec<GroupItem> = shown
        .iter()
        .map(|&i| GroupItem::new(&index.selection.groups[i], &loaded.availability))
        .collect();
    let mut counts = vec![0usize; GroupStatus::ALL.len()];
    for item in &items {
        counts[item.status.rank()] += 1;
    }
    let statuses = present(items.iter().map(|i| i.status));
    let rows = Signal::derive({
        let index = index.clone();
        move || {
            flags.with(|flags| {
                let mut items = items.clone();
                for (item, &i) in items.iter_mut().zip(&shown) {
                    item.needs_review = kept_need_review(&index.selection.groups[i], flags);
                }
                Arc::new(items)
            })
        }
    });
    let legend_looks = Signal::derive(move || {
        rows.with(|rows| {
            let mut looks = present_looks(rows.iter().map(|i| i.status));
            looks.extend(present_looks(
                rows.iter()
                    .filter(|i| i.needs_review)
                    .map(|_| GameFlag::NeedsReview),
            ));
            looks
        })
    });
    let only = RwSignal::new(None::<GroupStatus>);
    let keep = Signal::derive(move || -> Keep<GroupItem> {
        only.get()
            .map(|status| Arc::new(move |i: &GroupItem| i.status == status) as _)
    });
    let system = StoredValue::new(index.selection.system.clone());
    let groups = StoredValue::new(index.selection.groups.clone());
    let unresolved = counts[GroupStatus::Unresolved.rank()];
    // How many unresolved groups have no staged choice yet.
    let open = move || {
        staged.with(|p| {
            p.as_ref().map_or(0, |p| {
                groups.with_value(|gs| {
                    gs.iter()
                        .filter(|g| {
                            g.unresolved
                                && p.choices
                                    .get(&system.get_value())
                                    .is_none_or(|c| !c.contains_key(&g.group))
                        })
                        .count()
                })
            })
        })
    };
    let confirm_all = move |_| {
        staged.update(|p| {
            if let Some(p) = p {
                groups.with_value(|gs| confirm_unresolved(p, &system.get_value(), gs));
            }
        })
    };
    let availability = StoredValue::new(loaded.availability.clone());
    let all_dependencies: Vec<Dependency> = arcade_model::dependencies(&index.selection)
        .into_iter()
        .cloned()
        .collect();
    let arcade = index.selection.pins.arcade.is_some();
    let detail = Callback::new({
        let index = index.clone();
        move |item: GroupItem| -> AnyView {
            match index.group(&item.name) {
                Some(g) => {
                    let a = availability.get_value();
                    let needs = g
                        .kept
                        .iter()
                        .map(|w| {
                            let of = arcade_model::dependencies_of(&index.selection, &w.title);
                            (w.title.clone(), of.into_iter().cloned().collect())
                        })
                        .collect();
                    view! {
                        <GroupPanel
                            group=g.clone()
                            system=system.get_value()
                            availability=a.games
                            claimed=a.claimed
                            flags=flags.get()
                            staged
                            needs
                            arcade
                        />
                    }
                    .into_any()
                }
                None => ().into_any(),
            }
        }
    });

    view! {
        <section aria-labelledby="groups-title">
            <h2 id="groups-title">"Game groups"</h2>
            <p class="lede">
                {arcade.then_some("Each game group is a family: a parent machine and its clones. ")}
                "Open a game group to see its versions, the rule that decided each, and to choose the ones you want. Your choices are staged until you save them."
            </p>
            <DependencyLine
                lead="Your selected games need"
                dependencies=all_dependencies
                games=loaded.availability.games.clone()
                claimed=loaded.availability.claimed.clone()
            />
            <div class="count-strip" role="group" aria-label="Show game groups by status">
                <ul class="counts">
                    {statuses
                        .into_iter()
                        .map(|status| {
                            let look = status.look();
                            let n = counts[status.rank()];
                            let tone = format!("count status-count {}", look.tone.class());
                            view! {
                                <li>
                                    <Tooltip label=look.hint>
                                        <button
                                            type="button"
                                            class=tone
                                            aria-pressed=move || (only.get() == Some(status)).to_string()
                                            on:click=move |_| {
                                                only.update(|o| {
                                                    *o = if *o == Some(status) { None } else { Some(status) }
                                                })
                                            }
                                        >
                                            <span class="n">{n}</span>
                                            <span class="what">
                                                <span class=format!("status {}", look.tone.class())>
                                                    <Svg glyph=look.glyph />
                                                </span>
                                                " "
                                                {look.label}
                                            </span>
                                        </button>
                                    </Tooltip>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
                {(unresolved > 0)
                    .then(|| {
                        view! {
                            <div class="actions">
                                <button
                                    type="button"
                                    disabled=move || open() == 0
                                    on:click=confirm_all
                                >
                                    {move || match open() {
                                        0 => "Every unresolved pick is confirmed".to_string(),
                                        1 => "Apply to all: confirm the 1 unresolved pick".to_string(),
                                        n => format!("Apply to all: confirm the {n} unresolved picks"),
                                    }}
                                </button>
                                <span class="note small">
                                    "Confirming keeps the selection's pick, or the games you checked, as your choice."
                                </span>
                            </div>
                        }
                    })}
            </div>
            {flags_notice(flags)}
            <List
                id="groups"
                label="Game groups"
                rows
                empty="This selection has no game groups."
                placeholder="Filter by game group, game or rule"
                keep
                detail
                legend=move || view! { <LegendStrip looks=legend_looks list="groups" /> }
            />
        </section>
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StaleCol {
    Group,
    Titles,
    Choice,
}

/// A choice of yours for a group gone from this DAT Revision, and whether the staged profile drops it.
#[derive(Clone, PartialEq)]
struct StaleRow {
    group: String,
    titles: Vec<String>,
    dropped: bool,
}

impl StaleRow {
    fn choice(&self) -> &'static str {
        if self.dropped {
            "Dropped until you save"
        } else {
            "Kept"
        }
    }
}

impl ListRow for StaleRow {
    type Col = StaleCol;
    const COLUMNS: &'static [StaleCol] = &[StaleCol::Group, StaleCol::Titles, StaleCol::Choice];

    fn column(col: StaleCol) -> Column {
        match col {
            StaleCol::Group => Column::new(
                "group",
                "Game group",
                Width::Flex {
                    min: 12.0,
                    share: 2,
                },
            )
            .sortable(),
            StaleCol::Titles => Column::new(
                "titles",
                "Releases you chose",
                Width::Flex {
                    min: 14.0,
                    share: 3,
                },
            ),
            StaleCol::Choice => Column::new("choice", "Your choice", Width::Fixed(12.0)).sortable(),
        }
    }

    fn key(&self) -> String {
        self.group.clone()
    }

    fn title(&self) -> String {
        self.group.clone()
    }

    fn search(&self) -> String {
        format!("{} {}", self.group, self.titles.join(" "))
    }

    fn cell(&self, col: StaleCol) -> AnyView {
        match col {
            StaleCol::Group => view! { <Clip text=self.group.clone() /> }.into_any(),
            StaleCol::Titles => view! { <Clip text=self.titles.join(", ") /> }.into_any(),
            StaleCol::Choice => self.choice().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: StaleCol) -> std::cmp::Ordering {
        match col {
            StaleCol::Group | StaleCol::Titles => self.group.cmp(&other.group),
            StaleCol::Choice => self.dropped.cmp(&other.dropped),
        }
    }
}

fn stale_detail(row: StaleRow) -> AnyView {
    view! {
        <p>{row.choice()}</p>
        <dl class="facts">
            <dt>"Releases you chose"</dt>
            <dd>
                <ul class="plain">
                    {row.titles.into_iter().map(|t| view! { <li>{t}</li> }).collect_view()}
                </ul>
            </dd>
        </dl>
    }
    .into_any()
}

/// Choices of yours for groups that are gone from this DAT Revision: stale, kept until you drop them.
#[component]
fn StaleGroups(
    index: Arc<SelectionIndex>,
    staged: RwSignal<Option<SelectionProfile>>,
) -> impl IntoView {
    let stale = StoredValue::new(index.selection.stale_choices.clone());
    let system = StoredValue::new(index.selection.system.clone());
    let rows = Signal::derive(move || {
        staged.with(|p| {
            let kept = p.as_ref().map(|p| {
                p.choices
                    .get(&system.get_value())
                    .cloned()
                    .unwrap_or_default()
            });
            stale.with_value(|stale| {
                Arc::new(
                    stale
                        .iter()
                        .map(|(group, titles)| StaleRow {
                            group: group.clone(),
                            titles: titles.iter().cloned().collect(),
                            dropped: kept.as_ref().is_some_and(|c| !c.contains_key(group)),
                        })
                        .collect::<Vec<_>>(),
                )
            })
        })
    });
    let row_actions = Callback::new(move |row: StaleRow| {
        if row.dropped {
            vec![RowAction::unavailable(
                Glyph::CircleMinus,
                format!("Your choice for {} is dropped until you save", row.group),
            )]
        } else {
            let group = row.group.clone();
            vec![RowAction::run(
                Glyph::CircleMinus,
                format!("Drop your choice for {}", row.group),
                move || {
                    staged.update(|p| {
                        if let Some(p) = p {
                            p.reset_choices(&system.get_value(), Some(&group));
                        }
                    })
                },
            )]
        }
    });
    stale.with_value(|s| !s.is_empty()).then(|| {
        view! {
            <section aria-labelledby="stale-title">
                <h2 id="stale-title">"Stale choices"</h2>
                <p class="lede">
                    "These game groups are gone from this DAT revision. Your choices for them are kept until you drop them."
                </p>
                <List
                    id="stale"
                    label="Stale choices"
                    rows
                    empty="No stale choices."
                    placeholder="Filter by game group or release"
                    detail=Callback::new(stale_detail)
                    row_actions
                    scope="stale"
                />
            </section>
        }
    })
}

/// An edit of the staged Profile for one group.
type GroupEdit = dyn FnOnce(&mut SelectionProfile, &str, &SelectionGroup);

/// One group: its releases with checkboxes for your choice, and what the choice does.
#[component]
fn GroupPanel(
    group: SelectionGroup,
    system: String,
    availability: std::collections::BTreeMap<String, GameStatus>,
    claimed: std::collections::BTreeSet<String>,
    /// The review flags builds raised, to mark a release whose game needs review.
    flags: FlagsNow,
    staged: RwSignal<Option<SelectionProfile>>,
    /// Each Game kept, with the Dependencies it needs.
    needs: Vec<(String, Vec<Dependency>)>,
    /// An arcade Selection, whose include list is in the system's arcade settings.
    arcade: bool,
) -> impl IntoView {
    let needs_view = needs
        .into_iter()
        .map(|(title, dependencies)| {
            view! {
                <DependencyLine
                    lead=format!("{title} needs")
                    dependencies
                    games=availability.clone()
                    claimed=claimed.clone()
                />
            }
        })
        .collect_view();
    let group = StoredValue::new(group);
    let system = StoredValue::new(system);
    let g = group.get_value();
    let checked_now = move || {
        staged.with(|p| {
            p.as_ref()
                .map(|p| checked(p, &system.get_value(), &group.get_value()))
        })
    };
    let has_choice = move || {
        staged.with(|p| {
            p.as_ref().is_some_and(|p| {
                p.choices
                    .get(&system.get_value())
                    .is_some_and(|c| c.contains_key(&group.get_value().group))
            })
        })
    };
    let ready = move || staged.with(Option::is_some);
    let update = move |f: Box<GroupEdit>| {
        staged.update(|p| {
            if let Some(p) = p {
                f(p, &system.get_value(), &group.get_value());
            }
        });
    };
    let picks: Vec<String> = g.picks().into_iter().collect();
    let explanation = match &g.choice {
        Some(choice) if choice.status == ChoiceStatus::Applied && g.unresolved => {
            unresolved_explanation(&g)
        }
        Some(choice) => match choice.status {
            ChoiceStatus::Applied => format!(
                "Your choice decides this game group. Without it the selection would be: {}.",
                list_or_none(&picks)
            ),
            ChoiceStatus::MatchesDefault => {
                "Your choice matches the default now, so you can drop it and nothing changes."
                    .to_string()
            }
            ChoiceStatus::Conflicting => format!(
                "Your choice includes a game your settings leave out: {}. It is not applied: the default applies until you change your choice or your settings.",
                choice.problems.join(", ")
            ),
            ChoiceStatus::Stale => format!(
                "Your choice names a game that is gone from this DAT revision: {}. It is not applied, and it is kept until you change it.",
                choice.problems.join(", ")
            ),
        },
        None if g.unresolved => unresolved_explanation(&g),
        None => "The selection's default applies to this game group.".to_string(),
    };
    let status = g.choice.as_ref().map(|c| c.status);
    let unresolved = g.engine_unresolved();
    // Each release, with whether you can include it.
    let releases: Vec<(String, Outcome, String, bool)> = g
        .kept
        .iter()
        .map(|w| (w.title.clone(), Outcome::Kept, rule_text(&w.rule), false))
        .chain(
            g.clones
                .iter()
                .map(|a| (a.title.clone(), Outcome::Clone, rule_text(&a.rule), false)),
        )
        .chain(g.left_out.iter().map(|e| {
            (
                e.title.clone(),
                Outcome::LeftOut,
                left_out_text(e),
                includable(e, arcade),
            )
        }))
        .chain(g.dependencies.iter().map(|d| {
            (
                d.title.clone(),
                Outcome::Dependency,
                rule_text(&d.rule),
                false,
            )
        }))
        .collect();

    view! {
        <p>
            {status.map(|status| view! { <StatusBadge status /> " " })}
            {g.unresolved.then(|| view! { <StatusBadge status=SelectionFlag::Unresolved /> " " })}
            {g.drift.then(|| view! { <StatusBadge status=SelectionFlag::CoverageDrift /> " " })}
            {explanation}
        </p>
        {g.drift
            .then(|| {
                view! {
                    <p class="note small">
                        "Coverage drift: a game in this game group has no metadata entry, so the selection may know less about it."
                    </p>
                }
            })}
        {needs_view}
        <fieldset class="releases" disabled=move || !ready()>
            <legend>"Versions: check the ones to keep"</legend>
            <ul class="plain">
                {releases
                    .into_iter()
                    .map(|(title, outcome, rule, can_include)| {
                        let t = StoredValue::new(title.clone());
                        let is_checked = move || {
                            checked_now().is_some_and(|c| c.contains(&t.get_value()))
                        };
                        let avail = availability.get(&title).copied();
                        let claimed_only = claimed.contains(&title);
                        let flag = flags.get(&title).is_some().then(|| {
                            view! { " " <StatusIcon status=GameFlag::NeedsReview tab_stop=false /> }
                        });
                        // A Dependency is never elected, so it can't be checked either.
                        let excluded = matches!(outcome, Outcome::LeftOut | Outcome::Dependency);
                        view! {
                            <li class="release">
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        disabled=excluded
                                        prop:checked=is_checked
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            update(
                                                Box::new(move |p, system, g| toggle(p, system, g, &t.get_value(), on)),
                                            );
                                        }
                                    />
                                    {title.clone()}
                                </label>
                                " "
                                <StatusBadge status=outcome />
                                " "
                                {availability_view(avail, claimed_only, true)}
                                {flag}
                                <span class="note small rule">{rule}</span>
                                {can_include
                                    .then(|| {
                                        view! {
                                            " "
                                            <button
                                                type="button"
                                                class="small"
                                                on:click=move |_| {
                                                    update(
                                                        Box::new(move |p, system, g| {
                                                            include_and_check(p, system, g, &t.get_value(), arcade)
                                                        }),
                                                    )
                                                }
                                            >
                                                "Include and check"
                                            </button>
                                        }
                                    })}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </fieldset>
        <div class="actions">
            {unresolved
                .then(|| {
                    view! {
                        <button
                            type="button"
                            disabled=move || !ready() || has_choice()
                            on:click=move |_| update(Box::new(confirm))
                        >
                            "Confirm these games"
                        </button>
                    }
                })}
            <button
                type="button"
                disabled=move || !has_choice()
                on:click=move |_| {
                    update(Box::new(|p, system, g| p.reset_choices(system, Some(&g.group))))
                }
            >
                {if status == Some(ChoiceStatus::MatchesDefault) {
                    "Drop your choice"
                } else {
                    "Reset to default choices for this game group"
                }}
            </button>
        </div>
        <p class="note small">
            {if arcade {
                "Including a machine that was left out adds it to the always include list in this system's arcade settings, which are then set for this system and replace your global ones. Leaving every box unchecked leaves the game group out."
            } else {
                "Including a game that was left out adds an include override for it to this system's settings. Leaving every box unchecked leaves the game group out."
            }}
        </p>
    }
}

/// Dependencies summed up in one line, "Your selected games need 3 dependencies, all complete.", which opens to list
/// each with what you have of it. Nothing when there are none.
#[component]
fn DependencyLine(
    #[prop(into)] lead: String,
    dependencies: Vec<Dependency>,
    games: BTreeMap<String, GameStatus>,
    claimed: BTreeSet<String>,
) -> impl IntoView {
    let summary = Needs::of(&dependencies, &games, &claimed).summary()?;
    Some(view! {
        <details class="dependencies">
            <summary>{format!("{lead} {summary}.")}</summary>
            <ul class="plain">
                {dependencies
                    .into_iter()
                    .map(|d| {
                        let status = games.get(&d.title).copied();
                        let claimed_only = claimed.contains(&d.title);
                        view! {
                            <li class="release">
                                {d.title.clone()}
                                " "
                                {match status {
                                    Some(_) => availability_view(status, claimed_only, true),
                                    None => view! { <span class="note small">"Not audited"</span> }.into_any(),
                                }}
                                <span class="note small rule">{format!("Needed by {}", d.needed_by.join(", "))}</span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </details>
    })
}

fn list_or_none(titles: &[String]) -> String {
    if titles.is_empty() {
        "nothing".to_string()
    } else {
        titles.join(", ")
    }
}

/// Your staged choices: how many, and the buttons to save or reset them.
#[component]
fn StagedChoices(
    record: StoredValue<SelectionRecord>,
    staged: RwSignal<Option<SelectionProfile>>,
    saved: Signal<Option<ProfileVersion>>,
) -> impl IntoView {
    let r = record.get_value();
    let system = StoredValue::new(r.system.clone());
    let count = Memo::new(move |_| {
        let saved = saved.get()?;
        staged.with(|p| {
            p.as_ref()
                .map(|p| staged_changes(&saved.profile, p, &system.get_value()))
        })
    });
    let choices_in = move |whole: bool| {
        staged.with(|p| {
            p.as_ref().map_or(0, |p| {
                if whole {
                    p.choices.values().map(|c| c.len()).sum()
                } else {
                    p.choices.get(&system.get_value()).map_or(0, |c| c.len())
                }
            })
        })
    };
    let outcome = RwSignal::new(None::<Result<SavedProfile, String>>);
    let saving = RwSignal::new(false);
    let save = move |_| {
        let Some(profile) = staged.get_untracked() else {
            return;
        };
        let r = record.get_value();
        let request = SaveProfile {
            profile,
            base_version: Some(r.profile.version),
            queue: vec![StartSelectionFor {
                source_set: r.source_set.clone(),
                revision: Some(r.dat_revision_sha256.clone()),
            }],
        };
        let path = format!("/profiles/{}/versions", api::segment(&r.profile.name));
        saving.set(true);
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, SavedProfile>(&path, &request).await));
            saving.set(false);
        });
    };
    let discard = move |_| {
        if let Some(saved) = saved.get_untracked() {
            staged.set(Some(saved.profile));
        }
        outcome.set(None);
    };
    let next = r.profile.version + 1;

    view! {
        <section class="staged" aria-labelledby="staged-title">
            <h2 id="staged-title">"Your choices"</h2>
            <p aria-live="polite">
                {move || match count.get() {
                    None => "Loading your selection profile…".to_string(),
                    Some(0) => "Nothing staged. Check games in a game group to stage a choice.".to_string(),
                    Some(1) => "1 change staged.".to_string(),
                    Some(n) => format!("{n} changes staged."),
                }}
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="primary"
                    disabled=move || saving.get() || count.get().unwrap_or(0) == 0
                    on:click=save
                >
                    {format!("Save as version {next} and make a selection")}
                </button>
                <button type="button" disabled=move || count.get().unwrap_or(0) == 0 on:click=discard>
                    "Discard staged changes"
                </button>
                <button
                    type="button"
                    disabled=move || choices_in(false) == 0
                    on:click=move |_| {
                        staged
                            .update(|p| {
                                if let Some(p) = p {
                                    p.reset_choices(&system.get_value(), None);
                                }
                            })
                    }
                >
                    "Reset to default choices for this system"
                </button>
                <button
                    type="button"
                    disabled=move || choices_in(true) == 0
                    on:click=move |_| {
                        staged
                            .update(|p| {
                                if let Some(p) = p {
                                    p.reset_all_choices();
                                }
                            })
                    }
                >
                    "Reset to default choices for the whole profile"
                </button>
            </div>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(saved) => {
                            let message = if saved.created {
                                format!("Saved version {}.", saved.version.version)
                            } else {
                                format!(
                                    "Nothing changed, so no new version was saved; version {} stays the newest.",
                                    saved.version.version,
                                )
                            };
                            view! {
                                <Message kind=MessageKind::DONE>{message}</Message>
                                {saved
                                    .jobs
                                    .into_iter()
                                    .map(|job| view! { <JobOutcome job=job.id seed=job /> })
                                    .collect_view()}
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}
