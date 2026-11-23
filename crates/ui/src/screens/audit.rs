use std::cmp::Ordering;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use stage_select_api::build::LibraryFolder;
use stage_select_api::report::ReviewFlag;
use stage_select_api::{
    ALGORITHMS, AuditEvidence, AuditSummary, Entry, EvidenceLevel, Finding, Game, GameStatus,
    HeaderRule, Job, Placements, StartAudit,
};
use wasm_bindgen::JsCast;

use crate::api;
use crate::app::Loading;
use crate::events::use_live;
use crate::model::{
    ALGORITHM, AuditEntries, AuditGames, DEPENDENCIES_NOTE, Derived, Disks, EntryColumn,
    EntryFilter, FINDING, GameColumn, GameDisk, GameFilter, NO_GAME, NOTHING_TO_CHECK_NOTE, STATE,
    STATUS, absent_roms, audit_evidence_label, dependency_line, disk_text, entry_name,
    evidence_text, finding_label, game_status_label, games_found, games_of_status_label,
    header_rule_text, header_text, needs_review_from_counts, parse_ndjson, payload_read_text,
    unneeded, warning_counts, warning_text, when,
};
use crate::parts::help::Help;
use crate::parts::layout::Crumbs;
use crate::parts::legend::{LegendStrip, present_looks};
use crate::parts::list::{Clip, Column, Keep, List, ListRow, Width};
use crate::parts::message::{Message, MessageKind, Problem};
use crate::parts::pending::Pending;
use crate::parts::status::{
    AvailabilityStatus, FindingStatus, GameFlag, Placement, Status, StatusBadge, StatusCount,
    StatusIcon,
};
use crate::parts::tabs::{self, Tabs, use_tab};
use crate::parts::url_state::use_query_edit;
use crate::placement_model::{
    FlagsNow, READING_FLAGS, Stamped, flag_detail, flags_unknown, folder_name, game_placements,
    stamped_flags,
};

/// An audit's entries and game statuses, as its panels show them.
#[derive(Clone)]
pub struct Loaded {
    pub entries: Arc<AuditEntries>,
    pub games: Arc<AuditGames>,
}

/// Fetches the entries and game statuses of the audit `id`.
pub async fn load(id: i64) -> Result<Loaded, String> {
    let text = api::text(&format!("/audits/{id}/entries.ndjson")).await?;
    let rows = parse_ndjson(&text)?;
    let games = api::games(id).await?;
    let placed = game_placements(&rows);
    let found = games_found(&rows, &games);
    let disks = Disks::of(&rows);
    let entries = AuditEntries::new(rows, &games);
    Ok(Loaded {
        entries: Arc::new(entries),
        games: Arc::new(
            AuditGames::new(games)
                .with_placements(placed)
                .with_found(found)
                .with_disks(disks),
        ),
    })
}

/// A source set's placements and the library folders, read for its audits' tables, stamped with the job-settled
/// count the read started at.
pub type PlacementsNow = LocalResource<Stamped<(Placements, Vec<LibraryFolder>)>>;

/// Where source set `source_set`'s games are now, for its audits' tables: its placements, with the review flags builds
/// raised, and the library folders, by name. Read again whenever a job settles, since a build, audit or repair may
/// change them. Without the library folders a placement reads by the folder's number; without the placements the
/// tables say they can't tell which games need review.
pub fn placements_now(source_set: String) -> PlacementsNow {
    let live = use_live();
    LocalResource::new(move || {
        let at = live.settled.get();
        let path = format!("/source-sets/{}/placements", api::segment(&source_set));
        async move {
            let read = async {
                let placements = api::get::<Placements>(&path).await?;
                let folders = api::get::<Vec<LibraryFolder>>("/libraries")
                    .await
                    .unwrap_or_default();
                Ok((placements, folders))
            };
            (at, read.await)
        }
    })
}

/// The review flags among `now`, as a table of game statuses marks them: the last good read, and whether a newer
/// read is under way or failed.
pub fn flags_now(now: Option<PlacementsNow>) -> Memo<FlagsNow> {
    let Some(now) = now else {
        // A table with no source set to ask, such as the bench's, has no game a build flagged.
        return Memo::new(|_| FlagsNow::none());
    };
    let live = use_live();
    Memo::new(move |prev: Option<&FlagsNow>| {
        let read = now.get().map(|(at, read)| {
            stamped_flags(at, read.map(|(placements, _)| placements.review_flags))
        });
        prev.cloned()
            .unwrap_or_default()
            .next(read.as_ref(), live.settled.get())
    })
}

/// Says when a table can't be sure which games need review: while it reads them, the first time or again after a
/// job settles, or that it couldn't.
pub fn flags_notice(flags: Memo<FlagsNow>) -> impl IntoView {
    view! {
        <Pending
            pending=Signal::derive(move || flags.with(|f| f.reading))
            status=READING_FLAGS
            problem=Signal::derive(move || flags.with(|f| f.problem.as_deref().map(flags_unknown)))
        >
            ""
        </Pending>
    }
}

pub fn focus(id: &str) {
    if let Some(el) = document().get_element_by_id(id) {
        let _ = el.unchecked_into::<web_sys::HtmlElement>().focus();
    }
}

/// One Audit: its summary counts, Entries and Game Statuses.
#[component]
pub fn AuditScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let summary = LocalResource::new(move || {
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<AuditSummary>(&format!("/audits/{id}")).await,
                None => Err("That is not an audit number.".into()),
            }
        }
    });
    move || match summary.get() {
        None => view! { <Loading what="the audit" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Audit"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(audit)) => view! { <AuditView audit /> }.into_any(),
    }
}

#[component]
fn AuditView(audit: AuditSummary) -> impl IntoView {
    let id = audit.id;
    let pruned = audit.pruned;
    let set = StoredValue::new(audit.source_set.clone());
    let data =
        LocalResource::new(move || async move { if pruned { None } else { Some(load(id).await) } });
    let counts = Signal::derive(move || {
        data.get()
            .flatten()
            .and_then(|d| d.ok())
            .map(|d| d.entries.counts())
    });
    let reaudit = ReAudit::new(
        audit.source_set.clone(),
        audit.dat_revision_sha256.clone(),
        RwSignal::new(None),
    );
    let url_tab = use_tab();
    let tab = Memo::new(move |_| Tab::parse(url_tab.current().as_deref()));
    let audit_tabs = Signal::derive(move || {
        [Tab::Entries, Tab::Games]
            .into_iter()
            .map(|t| {
                let key = (t != Tab::Entries).then_some(t.key());
                tabs::Tab::new(url_tab.href(key), t.label(), t == tab.get())
            })
            .collect::<Vec<_>>()
    });
    let set_href = format!("/source-sets/{}", api::segment(&audit.source_set));
    let repair_href = format!("{set_href}?tab=repair");
    let findings = audit.findings.clone();
    let games = audit.games.clone();
    let needs_review = needs_review_from_counts(&findings);
    let rule = audit.header_rule.as_ref().and_then(|r| r.rule);
    let header_rule = audit.header_rule.as_ref().map(|named| {
        view! {
            <dt>"Header rule"</dt>
            <dd>{header_rule_text(named)}</dd>
        }
    });
    let warnings = warning_counts(&audit).map(|said| {
        view! {
            <dt>"Warnings"</dt>
            <dd>{said}</dd>
        }
    });

    let trail = vec![
        ("/source-sets".to_string(), "Source sets".to_string()),
        (set_href.clone(), audit.source_set.clone()),
    ];
    view! {
            <Crumbs trail here=format!("Audit #{id}") />
            <h1>{format!("Audit #{id}")}</h1>
            <dl class="facts">
                <dt>"Source set"</dt>
                <dd>
                    <A href=set_href>{audit.source_set.clone()}</A>
                </dd>
                <dt>"DAT revision"</dt>
                <dd>
                    <super::dats::RevisionName
                        sha256=audit.dat_revision_sha256.clone()
                        version=audit.dat_revision_version.clone()
                    />
                </dd>
                <dt>"Started"</dt>
                <dd>{when(&audit.started_at)}</dd>
                <dt>"Finished"</dt>
                <dd>{when(&audit.finished_at)}</dd>
                <dt>"Evidence"</dt>
                <dd>{audit_evidence_label(audit.evidence)}</dd>
                {header_rule}
                {warnings}
            </dl>
            <div class="actions">
                {(!pruned)
                    .then(|| {
                        view! {
                            <a
                                class="button"
                                href=api::url(&format!("/audits/{id}/report"))
                                download=format!("audit-{id}.json")
                            >
                                "Export JSON report"
                            </a>
                        }
                    })}
                {reaudit.button()}
                <A href=repair_href attr:class="button">"Repair the source set"</A>
            </div>
            {reaudit.outcome()}

            <section aria-labelledby="summary-title">
                <h2 id="summary-title">"Summary"</h2>
                <ul class="counts">
                    <StatusCount status=Derived::Verified>
                        {move || counts.get().map_or("—".to_string(), |c| c.verified.to_string())}
                    </StatusCount>
                    <StatusCount status=Derived::NeedsReview>{needs_review}</StatusCount>
                    {Finding::ALL
                        .iter()
                        .map(|f| {
                            let n = findings.get(f).copied().unwrap_or(0);
                            view! {
                                <StatusCount status=FindingStatus::from(*f) what=finding_label(*f)>
                                    {n}
                                </StatusCount>
                            }
                        })
                        .collect_view()}
                    {GameStatus::ALL
                        .iter()
                        .map(|s| {
                            let n = games.get(s).copied().unwrap_or(0);
                            view! {
                                <StatusCount status=*s what=games_of_status_label(*s)>
                                    {n}
                                </StatusCount>
                            }
                        })
                        .collect_view()}
                </ul>
                {pruned
                    .then(|| {
                        view! {
                            <p class="note">
                                "This audit is pruned: only its summary is kept, so its entries, game statuses and verified count are gone."
                            </p>
                        }
                    })}
            </section>

            {move || match data.get() {
                None => view! { <Loading what="the entries" /> }.into_any(),
                Some(None) => ().into_any(),
                Some(Some(Err(message))) => view! { <Problem message /> }.into_any(),
                Some(Some(Ok(loaded))) => {
                    view! {
                        <Tabs label="Audit details" tabs=audit_tabs />
    {move || match tab.get() {
                            Tab::Entries => {
                                view! { <EntriesPanel entries=loaded.entries.clone() source_set=set.get_value() /> }
                                    .into_any()
                            }
                            Tab::Games => {
                                view! { <GamesPanel games=loaded.games.clone() rule source_set=set.get_value() /> }
                                    .into_any()
                            }
                        }}
                    }
                        .into_any()
                }
            }}
        }
}

/// The audit page's tabs, by the URL's `tab`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Entries,
    Games,
}

impl Tab {
    fn parse(key: Option<&str>) -> Self {
        if key == Some("games") {
            Tab::Games
        } else {
            Tab::Entries
        }
    }

    fn key(self) -> &'static str {
        match self {
            Tab::Entries => "entries",
            Tab::Games => "games",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Entries => "Entries",
            Tab::Games => "Game statuses",
        }
    }
}

/// Starts an Audit of a Source Set against one DAT Revision, and says how that went.
#[derive(Clone, Copy)]
pub struct ReAudit {
    source_set: StoredValue<String>,
    revision: StoredValue<String>,
    outcome: RwSignal<Option<Result<Job, String>>>,
}

impl ReAudit {
    /// `outcome` receives the queued Job or the Service's refusal.
    pub fn new(
        source_set: String,
        revision: String,
        outcome: RwSignal<Option<Result<Job, String>>>,
    ) -> ReAudit {
        ReAudit {
            source_set: StoredValue::new(source_set),
            revision: StoredValue::new(revision),
            outcome,
        }
    }

    pub fn start(self) {
        let request = StartAudit {
            source_set: self.source_set.get_value(),
            revision: Some(self.revision.get_value()),
            evidence: AuditEvidence::Incremental,
        };
        leptos::task::spawn_local(async move {
            let job = api::post::<_, Job>("/audits", &request).await;
            self.outcome.set(Some(job));
        });
    }

    pub fn button(self) -> impl IntoView {
        view! {
            <button type="button" on:click=move |_| self.start()>
                "Re-audit against this revision"
            </button>
        }
    }

    pub fn outcome(self) -> impl IntoView {
        move || {
            self.outcome.get().map(|outcome| match outcome {
                Ok(job) => queued(&job).into_any(),
                Err(message) => view! { <Problem message /> }.into_any(),
            })
        }
    }
}

/// Says that a Job was queued, with the way to follow it.
pub fn queued(job: &Job) -> impl IntoView + use<> {
    let said = format!(
        "Job #{} is queued to audit {} against ",
        job.id, job.source_set
    );
    let sha256 = job.dat_revision.clone();
    view! {
        <Message kind=MessageKind::DONE>
            {said}
            <super::dats::RevisionName sha256 />
            ". "
            <A href="/activity">"Follow it in Activity"</A>
        </Message>
    }
}

/// An Entry in an Audit's Entries list, by its index.
#[derive(Clone)]
pub struct EntryItem {
    entries: Arc<AuditEntries>,
    flags: FlagsNow,
    i: u32,
}

// The entries never change once loaded, so a row is the same while it points at the same entry and its games' flag
// stands.
impl PartialEq for EntryItem {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.entries, &other.entries)
            && self.i == other.i
            && self.flagged() == other.flagged()
    }
}

impl EntryItem {
    fn entry(&self) -> &Entry {
        &self.entries.rows[self.i as usize].entry
    }

    fn finding_status(&self) -> FindingStatus {
        let entry = self.entry();
        let payload = entry
            .evidence
            .as_ref()
            .is_some_and(|e| e.level == EvidenceLevel::Payload);
        FindingStatus::of(entry.finding, payload)
    }

    fn game_status(&self) -> Option<GameStatus> {
        self.entries.primary_status(self.i as usize)
    }

    /// Whether a build flagged any game the entry counts towards as needing review.
    fn flagged(&self) -> bool {
        self.flags
            .any(self.entry().roms.iter().map(|r| r.game.as_str()))
    }

    fn games(&self) -> String {
        let mut games: Vec<&str> = self.entry().roms.iter().map(|r| r.game.as_str()).collect();
        games.dedup();
        games.join(", ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryCol {
    Finding,
    Name,
    Game,
    GameStatus,
    Evidence,
}

/// A game's status as its icon in a cell, with the review flag a build raised beside it.
fn game_status_cell<S: Status>(status: Option<S>, flagged: bool) -> AnyView {
    let flag =
        flagged.then(|| view! { " " <StatusIcon status=GameFlag::NeedsReview tab_stop=false /> });
    view! {
        {status.map(|status| view! { <StatusIcon status tab_stop=false /> })}
        {flag}
    }
    .into_any()
}

impl ListRow for EntryItem {
    type Col = EntryCol;
    const COLUMNS: &'static [EntryCol] = &[
        EntryCol::Finding,
        EntryCol::Name,
        EntryCol::Game,
        EntryCol::GameStatus,
        EntryCol::Evidence,
    ];

    fn column(col: EntryCol) -> Column {
        match col {
            EntryCol::Finding => Column::new("finding", "Finding", Width::Fixed(5.5))
                .sortable()
                .hint("What the audit found the entry to be"),
            EntryCol::Name => Column::new(
                "name",
                "Entry",
                Width::Flex {
                    min: 14.0,
                    share: 3,
                },
            )
            .sortable()
            .hint("The file, and the member inside it"),
            EntryCol::Game => Column::new(
                "game",
                "Game",
                Width::Flex {
                    min: 10.0,
                    share: 2,
                },
            )
            .hint("The games its payload counts towards"),
            EntryCol::GameStatus => Column::new("status", "Game status", Width::Fixed(6.5))
                .sortable()
                .hint("The status of the first game it counts towards"),
            EntryCol::Evidence => {
                Column::new("evidence", "Evidence", Width::Flex { min: 9.0, share: 1 })
                    .sortable()
                    .hint("What the audit compared it by")
            }
        }
    }

    fn key(&self) -> String {
        self.entries.rows[self.i as usize].seq.to_string()
    }

    fn title(&self) -> String {
        entry_name(self.entry())
    }

    fn search(&self) -> String {
        self.entries.search(self.i as usize).to_string()
    }

    fn cell(&self, col: EntryCol) -> AnyView {
        match col {
            EntryCol::Finding => {
                view! { <StatusIcon status=self.finding_status() tab_stop=false /> }.into_any()
            }
            EntryCol::Name => view! { <Clip text=entry_name(self.entry()) /> }.into_any(),
            EntryCol::Game => view! { <Clip text=self.games() /> }.into_any(),
            EntryCol::GameStatus => game_status_cell(self.game_status(), self.flagged()),
            EntryCol::Evidence => view! { <Clip text=evidence_text(self.entry()) /> }.into_any(),
        }
    }

    fn compare(&self, other: &Self, col: EntryCol) -> Ordering {
        let key = match col {
            EntryCol::Finding => EntryColumn::Finding,
            EntryCol::Name => EntryColumn::Name,
            EntryCol::GameStatus => EntryColumn::GameStatus,
            EntryCol::Evidence => EntryColumn::Evidence,
            EntryCol::Game => return Ordering::Equal,
        };
        self.entries.compare(key, self.i as usize, other.i as usize)
    }
}

/// Every row of a loaded list, rebuilt with the flags whenever they change.
fn all_rows<T: Send + Sync + 'static>(
    n: usize,
    flags: Memo<FlagsNow>,
    row: impl Fn(FlagsNow, u32) -> T + Send + Sync + 'static,
) -> Memo<Arc<Vec<T>>> {
    Memo::new_with_compare(
        move |_| {
            let flags = flags.get();
            Arc::new((0..n as u32).map(|i| row(flags.clone(), i)).collect())
        },
        |_, _| true,
    )
}

/// A select of one of the list's filters in the URL: `key`'s value, or none for "Any".
pub(crate) fn url_select(
    label: &'static str,
    key: &'static str,
    options: Vec<(&'static str, &'static str)>,
    disabled: bool,
) -> impl IntoView {
    let query = use_query_map();
    let edit = use_query_edit();
    let current = Memo::new(move |_| query.with(|q| q.get(key).unwrap_or_default()));
    view! {
        <label>
            {label}
            <select disabled=disabled on:change=move |ev| {
                let v = event_target_value(&ev);
                edit.run(vec![(key, (!v.is_empty()).then_some(v))]);
            }>
                <option value="" selected=move || current.with(String::is_empty)>
                    "Any"
                </option>
                {options
                    .into_iter()
                    .map(|(value, text)| {
                        view! {
                            <option value=value selected=move || current.with(|c| c == value)>
                                {text}
                            </option>
                        }
                    })
                    .collect_view()}
            </select>
        </label>
    }
}

/// An Audit's Entries in the shared list, with filters by finding, state, game status and algorithm in the URL beside
/// its text. Filtering and sorting run in the browser on every keystroke.
#[component]
pub fn EntriesPanel(
    entries: Arc<AuditEntries>,
    /// Its source set, to mark the games a build flagged as needing review; none where there's none to ask.
    #[prop(optional)]
    source_set: Option<String>,
) -> impl IntoView {
    let flags = flags_now(source_set.map(placements_now));
    let rows = all_rows(entries.len(), flags, {
        let entries = entries.clone();
        move |flags, i| EntryItem {
            entries: entries.clone(),
            flags,
            i,
        }
    });
    let query = use_query_map();
    let filter = Memo::new(move |_| query.with(|q| EntryFilter::from_query(|k| q.get_str(k))));
    let keep: Signal<Keep<EntryItem>> = Signal::derive(move || {
        let filter = filter.get();
        (filter != EntryFilter::default()).then(|| {
            Arc::new(move |row: &EntryItem| row.entries.keeps(row.i as usize, &filter))
                as Arc<dyn Fn(&EntryItem) -> bool + Send + Sync>
        })
    });
    let looks = Memo::new(move |_| {
        rows.with(|r| {
            let mut looks = present_looks(r.iter().map(EntryItem::finding_status));
            looks.extend(present_looks(r.iter().filter_map(EntryItem::game_status)));
            if r.iter().any(EntryItem::flagged) {
                looks.push(GameFlag::NeedsReview.look());
            }
            looks
        })
    });
    let filters = move || {
        view! {
            {url_select(
                "Finding",
                FINDING,
                Finding::ALL.iter().map(|f| (f.as_str(), finding_label(*f))).collect(),
                false,
            )}
            {url_select(
                "State",
                STATE,
                [Derived::Verified, Derived::NeedsReview].map(|d| (d.slug(), d.label())).to_vec(),
                false,
            )}
            {url_select(
                "Game status",
                STATUS,
                GameStatus::ALL
                    .iter()
                    .map(|s| (s.as_str(), game_status_label(*s)))
                    .chain([(NO_GAME, "No game")])
                    .collect(),
                false,
            )}
            {url_select("Algorithm", ALGORITHM, ALGORITHMS.iter().map(|a| (*a, *a)).collect(), false)}
        }
    };
    view! {
        {flags_notice(flags)}
        <List
            id="entries"
            label="Entries"
            rows
            empty="This audit has no entries."
            placeholder="Filter by entry or game name"
            filters
            keep
            legend=move || view! { <LegendStrip looks list="entries" /> }
            detail=Callback::new(|row: EntryItem| entry_detail(&row))
        />
    }
}

fn entry_detail(row: &EntryItem) -> AnyView {
    let entry = row.entry().clone();
    let evidence = evidence_text(&entry);
    let read = payload_read_text(&entry);
    let header = header_text(&entry);
    let game_status = row.game_status().map(|status| {
        let flag = row
            .flagged()
            .then(|| view! { " " <StatusBadge status=GameFlag::NeedsReview /> });
        view! {
            <dt>"Game status"</dt>
            <dd><StatusBadge status /> {flag}</dd>
        }
    });
    let roms = entry
        .roms
        .iter()
        .map(|r| view! { <li>{r.game.clone()} " › " <code>{r.name.clone()}</code></li> })
        .collect_view();
    view! {
        <dl class="facts">
            <dt>"Path"</dt>
            <dd>
                <code class="path">{entry.path.clone()}</code>
            </dd>
            {entry
                .member
                .clone()
                .map(|m| {
                    view! {
                        <dt>"Member"</dt>
                        <dd>
                            <code class="path">{m}</code>
                        </dd>
                    }
                })}
            <dt>"Finding"</dt>
            <dd><StatusBadge status=row.finding_status() /></dd>
            {entry
                .reason
                .clone()
                .map(|r| {
                    view! {
                        <dt>"Reason"</dt>
                        <dd>{r}</dd>
                    }
                })}
            <dt>"Evidence"</dt>
            <dd>{if evidence.is_empty() { "None".to_string() } else { evidence }}</dd>
            {header
                .map(|header| {
                    view! {
                        <dt>"Header"</dt>
                        <dd>{header}</dd>
                    }
                })}
            {read
                .map(|read| {
                    view! {
                        <dt>"Payload last read"</dt>
                        <dd>{read}</dd>
                    }
                })}
            {entry
                .named_rom
                .clone()
                .map(|r| {
                    view! {
                        <dt>"Its name gives"</dt>
                        <dd>{r.game} " › " <code>{r.name}</code></dd>
                    }
                })}
            {(!entry.roms.is_empty())
                .then(|| {
                    view! {
                        <dt>"Its payload matches"</dt>
                        <dd>
                            <ul>{roms}</ul>
                        </dd>
                    }
                })}
            {game_status}
            {disk_text(&entry)
                .map(|disk| {
                    view! {
                        <dt>"Disk"</dt>
                        <dd>{disk}</dd>
                    }
                })}
        </dl>
    }
    .into_any()
}

/// A Game's Dependencies in one line, which opens to list each with its own Game Status; nothing when it has none.
fn dependencies_view(game: &Game, games: &AuditGames) -> Option<AnyView> {
    let line = dependency_line(game, |name| games.claimed(name))?;
    let each = game
        .dependencies
        .iter()
        .map(|d| view! { <li>{d.game.clone()} " " <StatusBadge status=d.status /></li> })
        .collect_view();
    Some(
        view! {
            <h4>"Dependencies"</h4>
            <Help summary=format!("{line}.")>
                <p>{DEPENDENCIES_NOTE}</p>
                <ul class="plain">{each}</ul>
            </Help>
        }
        .into_any(),
    )
}

/// A Game's Disks, each with the CHD that holds it and its Evidence, or, when absent, any CHD named for it that
/// matched no Disk, such as a delta CHD.
fn disks_view(disks: Vec<GameDisk>) -> Option<AnyView> {
    if disks.is_empty() {
        return None;
    }
    let each = disks
        .into_iter()
        .map(|d| {
            let found = match d.held {
                Some(entry) => {
                    let payload = entry
                        .evidence
                        .as_ref()
                        .is_some_and(|e| e.level == EvidenceLevel::Payload);
                    view! {
                        " "
                        <StatusBadge status=FindingStatus::of(entry.finding, payload) />
                        " " <code class="path">{entry.path.clone()}</code>
                        <p class="note small">{disk_text(&entry)}</p>
                    }
                    .into_any()
                }
                None => view! {
                    {d.needed.then(|| view! { " " <StatusBadge status=GameStatus::Missing /> })}
                    {(!d.notes.is_empty())
                        .then(|| view! { ": " <span class="note">{d.notes.join("; ")}</span> })}
                    {d.unusable
                        .into_iter()
                        .map(|entry| {
                            let why = disk_text(&entry).or(entry.reason.clone());
                            view! {
                                <p class="note small">
                                    "A CHD named for it, " <code class="path">{entry.path.clone()}</code>
                                    ", matches no disk. " {why}
                                </p>
                            }
                        })
                        .collect_view()}
                }
                .into_any(),
            };
            view! { <li><code>{d.name}</code> {found}</li> }
        })
        .collect_view();
    Some(
        view! {
            <h4>"Disks"</h4>
            <ul class="plain">{each}</ul>
        }
        .into_any(),
    )
}

/// A Game Status in an Audit's Game Statuses list, by its index.
#[derive(Clone)]
pub struct GameItem {
    games: Arc<AuditGames>,
    flags: FlagsNow,
    i: u32,
}

// As for `EntryItem`: the same game, with the same flag.
impl PartialEq for GameItem {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.games, &other.games) && self.i == other.i && self.flag() == other.flag()
    }
}

impl GameItem {
    fn game(&self) -> &Game {
        &self.games.rows[self.i as usize].game
    }

    fn status(&self) -> AvailabilityStatus {
        let game = self.game();
        AvailabilityStatus::of(game.status, !game.claimed_roms.is_empty())
    }

    fn flag(&self) -> Option<&ReviewFlag> {
        self.flags.get(&self.game().name)
    }

    fn placement(&self) -> Option<Placement> {
        self.games.placement(self.i as usize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameCol {
    Status,
    Name,
    Placement,
    Absent,
}

impl ListRow for GameItem {
    type Col = GameCol;
    const COLUMNS: &'static [GameCol] = &[
        GameCol::Status,
        GameCol::Name,
        GameCol::Placement,
        GameCol::Absent,
    ];

    fn column(col: GameCol) -> Column {
        match col {
            GameCol::Status => Column::new("status", "Status", Width::Fixed(5.5))
                .sortable()
                .hint("The game's status in this audit, and whether a build flagged it"),
            GameCol::Name => Column::new(
                "name",
                "Game",
                Width::Flex {
                    min: 16.0,
                    share: 1,
                },
            )
            .sortable(),
            GameCol::Placement => Column::new("placement", "Placement", Width::Fixed(6.5))
                .sortable()
                .hint("Where the audit found the game's files"),
            GameCol::Absent => Column::new("absent", "Absent", Width::Fixed(6.0))
                .sortable()
                .numeric()
                .hint("How many of its ROMs are absent"),
        }
    }

    fn key(&self) -> String {
        self.games.rows[self.i as usize].seq.to_string()
    }

    fn title(&self) -> String {
        self.game().name.clone()
    }

    fn search(&self) -> String {
        self.games.search(self.i as usize).to_string()
    }

    fn cell(&self, col: GameCol) -> AnyView {
        match col {
            GameCol::Status => game_status_cell(Some(self.status()), self.flag().is_some()),
            GameCol::Name => view! { <Clip text=self.game().name.clone() /> }.into_any(),
            GameCol::Placement => self
                .placement()
                .map(|p| view! { <StatusIcon status=p tab_stop=false /> })
                .into_any(),
            GameCol::Absent => match self.game().missing_roms.len() {
                0 => ().into_any(),
                n => n.to_string().into_any(),
            },
        }
    }

    fn compare(&self, other: &Self, col: GameCol) -> Ordering {
        let key = match col {
            GameCol::Status => GameColumn::Status,
            GameCol::Name => GameColumn::Name,
            GameCol::Placement => GameColumn::Placement,
            GameCol::Absent => GameColumn::Absent,
        };
        self.games.compare(key, self.i as usize, other.i as usize)
    }
}

/// Why a past audit shows a review flag it didn't raise.
const FLAGGED_NOW: &str = "This flag is the game's now, not part of this audit: every audit of the source set \
shows it until a full audit verifies the file or the game returns.";

/// An Audit's Game Statuses in the shared list, with a filter by status in the URL beside its text.
#[component]
pub fn GamesPanel(
    games: Arc<AuditGames>,
    rule: Option<HeaderRule>,
    source_set: String,
) -> impl IntoView {
    let now = placements_now(source_set);
    let flags = flags_now(Some(now));
    let folders = Memo::new(move |_| {
        now.get()
            .and_then(|(_, read)| read.ok())
            .map(|(_, f)| f)
            .unwrap_or_default()
    });
    let rows = all_rows(games.len(), flags, {
        let games = games.clone();
        move |flags, i| GameItem {
            games: games.clone(),
            flags,
            i,
        }
    });
    let query = use_query_map();
    let filter = Memo::new(move |_| query.with(|q| GameFilter::from_query(|k| q.get_str(k))));
    let keep: Signal<Keep<GameItem>> = Signal::derive(move || {
        let filter = filter.get();
        (filter != GameFilter::default()).then(|| {
            Arc::new(move |row: &GameItem| row.games.keeps(row.i as usize, &filter))
                as Arc<dyn Fn(&GameItem) -> bool + Send + Sync>
        })
    });
    let looks = Memo::new(move |_| {
        rows.with(|r| {
            let mut looks = present_looks(r.iter().map(GameItem::status));
            if r.iter().any(|g| g.flag().is_some()) {
                looks.push(GameFlag::NeedsReview.look());
            }
            looks.extend(present_looks(r.iter().filter_map(GameItem::placement)));
            looks
        })
    });
    let filters = move || {
        url_select(
            "Game status",
            STATUS,
            GameStatus::ALL
                .iter()
                .map(|s| (s.as_str(), game_status_label(*s)))
                .collect(),
            false,
        )
    };
    view! {
        {flags_notice(flags)}
        <List
            id="games"
            label="Game statuses"
            rows
            empty="This audit has no game statuses."
            placeholder="Filter by game or absent ROM name"
            filters
            keep
            legend=move || view! { <LegendStrip looks list="games" /> }
            detail=Callback::new(move |row: GameItem| game_detail(&row, rule, folders))
        />
    }
}

fn game_detail(
    row: &GameItem,
    rule: Option<HeaderRule>,
    folders: Memo<Vec<LibraryFolder>>,
) -> AnyView {
    let (games, i) = (&row.games, row.i as usize);
    let game = row.game().clone();
    let placement = row.placement().map(|p| {
        let place = match games.placed_in(i) {
            Some(id) => view! {
                "Placed in "
                <A href=format!("/libraries/{id}")>{move || folders.with(|f| folder_name(f, id))}</A>
                " under single copy: its file there was the game's only copy."
            }
            .into_any(),
            None => "In your source set's own folder.".into_any(),
        };
        view! {
            <h4>"Placement"</h4>
            <p><StatusBadge status=p /></p>
            <p>{place}</p>
        }
    });
    let flag = row.flag().cloned().map(|flag| {
        view! {
            <h4>"Flagged now"</h4>
            <p><StatusBadge status=GameFlag::NeedsReview /></p>
            <p>{flag_detail(&flag)}</p>
            <p class="note">{FLAGGED_NOW}</p>
        }
    });
    let absent = (!game.missing_roms.is_empty()).then(|| {
        let elsewhere = (!game.available_elsewhere.is_empty()).then(|| {
            view! {
                <p class="note">"A ROM available in another game is inside that game's ZIP or folder, so it doesn't count towards this game. A repair can fill this game from it unless the file is named for the other game."</p>
            }
        });
        let unneeded_note = game.missing_roms.iter().any(|r| unneeded(&game, r)).then(|| {
            view! {
                <p class="note">"An absent ROM that is optional, or that has no good dump known, doesn't keep this game from being complete."</p>
            }
        });
        view! {
            <h4>"Absent ROMs"</h4>
            {unneeded_note}
            {elsewhere}
            <ul class="data">{absent_roms(&game).into_iter().map(|r| view! { <li>{r}</li> }).collect_view()}</ul>
        }
    });
    let claimed = (!game.claimed_roms.is_empty()).then(|| {
        view! {
            <h4>"Present only on claim evidence"</h4>
            <p class="note">"A quick inventory read these ROMs' names, sizes and CRC32s from a ZIP directory, or disks' SHA-1s from their CHD headers, without reading their payloads. Audit the source set incrementally or in full to verify them; builds never use them."</p>
            <ul class="data">{game.claimed_roms.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
        }
    });
    let bad_dumps = (!game.bad_dumps.is_empty()).then(|| {
        view! {
            <h4>"Known bad dumps"</h4>
            <p class="note">"The DAT lists these ROMs by the checksums of dumps known to be bad. A file that matches one counts like any other."</p>
            <ul class="data">{game.bad_dumps.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
        }
    });
    let warnings = (!game.warnings.is_empty()).then(|| {
        view! {
            <h4>"Warnings"</h4>
            <ul class="data">
                {game
                    .warnings
                    .iter()
                    .map(|w| view! { <li>{format!("It {}", warning_text(*w, rule))}</li> })
                    .collect_view()}
            </ul>
        }
    });
    let nothing_to_check = (game.status == GameStatus::NothingToCheck)
        .then(|| view! { <p class="note">{NOTHING_TO_CHECK_NOTE}</p> });
    let dependencies = dependencies_view(&game, games);
    let disks = disks_view(games.disks(i));
    view! {
        <p><StatusBadge status=row.status() /></p>
        {nothing_to_check}
        {flag}
        {placement}
        {warnings}
        {dependencies}
        {disks}
        {absent}
        {bad_dumps}
        {claimed}
    }
    .into_any()
}
