//! A job's outcome, wherever you start one: queued and running it says so, with the meter, and once it ends it links
//! to what it made (the audit, the selection, the build, the repair) or, if it failed, says how far it got and leads
//! to the job. Built on `message`: queued and running are announced politely, and a failure of a job you watched end
//! is an alert. A failure shown later, on a page you opened after it, is shown but not announced.
//!
//! `Reports` holds what a screen's actions said, in order, failures first: a job each started, something made at once
//! with its link, or a refusal.

use leptos::prelude::*;
use leptos_router::components::A;
use stage_select_api::build::{BuildDetail, ItemKind, ItemState};
use stage_select_api::{Job, JobKind, JobState};

use super::icon::Glyph;
use super::message::{Live, Message, MessageKind, Problem};
use super::meter::ProgressMeter;
use super::status::Status;
use super::tabs::tab_link;
use super::url_state::encode;
use crate::api;
use crate::build_model::publishing;
use crate::events::use_live;
use crate::library_model::{FolderTab, build_finished};
use crate::source_set_model::PageTab;

/// What a Job works on: the Ingest Folder it scans, the Library Folder it builds or checks, the Additions Set it
/// works on, or else its Source Set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    SourceSet(String),
    LibraryFolder(i64),
    IngestFolder(String),
    AdditionsSet(String),
}

impl Subject {
    pub fn of(job: &Job) -> Subject {
        if let Some(root) = &job.ingest_folder {
            Subject::IngestFolder(root.clone())
        } else if let Some(folder) = job.library_folder {
            Subject::LibraryFolder(folder)
        } else if let Some(system) = &job.additions_set {
            Subject::AdditionsSet(system.clone())
        } else {
            Subject::SourceSet(job.source_set.clone())
        }
    }

    pub fn href(&self) -> String {
        match self {
            Subject::SourceSet(name) => format!("/source-sets/{}", encode(name)),
            Subject::LibraryFolder(id) => format!("/libraries/{id}"),
            Subject::IngestFolder(root) => format!("/ingest/{}", encode(root)),
            Subject::AdditionsSet(_) => "/additions".to_string(),
        }
    }
}

/// What a Job recorded or carries out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Audit(i64),
    Selection(i64),
    /// The Build a Build Job records, once it has started.
    Build(i64),
    /// The repair a Repair Job carries out, by its plan's ID, on its Source Set's Repair tab.
    Repair {
        source_set: String,
        plan: i64,
    },
}

impl Outcome {
    pub fn of(job: &Job) -> Option<Outcome> {
        if let Some(plan) = job.repair {
            return Some(Outcome::Repair {
                source_set: job.source_set.clone(),
                plan,
            });
        }
        match (job.audit, job.selection, job.build) {
            (Some(audit), _, _) => Some(Outcome::Audit(audit)),
            (None, Some(selection), _) => Some(Outcome::Selection(selection)),
            (None, None, Some(build)) => Some(Outcome::Build(build)),
            (None, None, None) => None,
        }
    }

    pub fn href(&self) -> String {
        match self {
            Outcome::Audit(id) => format!("/audits/{id}"),
            Outcome::Selection(id) => format!("/selections/{id}"),
            Outcome::Build(id) => format!("/builds/{id}"),
            Outcome::Repair { source_set, plan } => tab_link(
                &format!("/source-sets/{}", encode(source_set)),
                Some(PageTab::Repair.slug()),
                &[("plan", &plan.to_string())],
            ),
        }
    }

    /// The glyph and label of a link to it from a list of Jobs.
    pub fn glyph(&self) -> Glyph {
        match self {
            Outcome::Audit(_) => Glyph::ClipboardList,
            Outcome::Selection(_) => Glyph::SlidersHorizontal,
            Outcome::Build(_) => Glyph::Package,
            Outcome::Repair { .. } => Glyph::Wrench,
        }
    }
}

/// A link onward from an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub href: String,
    pub label: String,
}

impl Link {
    pub fn new(href: impl Into<String>, label: impl Into<String>) -> Link {
        Link {
            href: href.into(),
            label: label.into(),
        }
    }
}

/// The Job's row in Activity.
pub fn job_href(id: i64) -> String {
    format!("/activity?row={id}")
}

/// How far a Job got before it stopped: `done` of `total`, counted in `unit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reached {
    pub done: usize,
    pub total: usize,
    pub unit: &'static str,
}

/// What a Build that stopped left: how many of its files it wrote, and how many built files it removed, when that
/// is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub reached: Reached,
    pub removed: Option<usize>,
}

/// A Build's tally from its items: written of every file it set out to write or remove, and the removals it made.
/// Placing deletes Source Set originals and returning deletes placed files, and no item records those, so a Build
/// that places or returns claims nothing about removal.
pub fn tally(detail: &BuildDetail) -> Tally {
    let b = &detail.build;
    let disposes = b.planned.place > 0 || b.planned.returned > 0;
    let removed = (!disposes).then(|| {
        detail
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Remove && i.state == ItemState::Applied)
            .count()
    });
    Tally {
        reached: Reached {
            done: b.applied as usize,
            total: (b.applied + b.failed + b.pending + b.uncertain + publishing(detail)) as usize,
            unit: "files",
        },
        removed,
    }
}

/// Whether the outcome part fetches its Job: when the live book lacks it and the part knows of no ending, as when
/// a resync dropped it while it ran.
pub fn needs_fetch(in_book: bool, held: Option<&Job>) -> bool {
    !in_book && held.is_none_or(|j| !j.state.is_final())
}

/// The unit a Job's progress counts.
pub fn unit(kind: JobKind) -> &'static str {
    if kind == JobKind::Repair {
        "steps"
    } else {
        "files"
    }
}

/// What the outcome part shows for a Job: its kind, how it is announced, the sentence, the Job's error (its first
/// line) and the link onward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub kind: MessageKind,
    pub live: Live,
    pub text: String,
    pub error: Option<String>,
    pub link: Option<Link>,
}

/// The message kind of a Job that hasn't ended: its state's glyph and tone, so the message reads like the badge.
fn waiting(state: JobState) -> MessageKind {
    let look = state.look();
    MessageKind {
        tone: look.tone,
        glyph: look.glyph,
        label: look.label,
        live: Live::Polite,
    }
}

/// What the Job does, as the subject of a sentence: "the audit of NES".
pub fn what(job: &Job) -> String {
    let set = &job.source_set;
    match job.kind {
        JobKind::Audit => format!("the audit of {set}"),
        JobKind::Selection => format!("the selection of {set}"),
        JobKind::Build => "the build".to_string(),
        JobKind::Check => "the check of the library folder".to_string(),
        JobKind::IngestScan => match &job.ingest_folder {
            Some(root) => format!("the scan of {root}"),
            None => "the scan".to_string(),
        },
        JobKind::Repair => format!("the repair of {set}"),
        JobKind::Additions => match &job.additions_set {
            Some(system) => format!("publishing the additions of {system}"),
            None => "publishing the additions".to_string(),
        },
        JobKind::StatWalk => format!("the check of {set} for changes"),
        JobKind::Calibration => "the calibration".to_string(),
    }
}

fn sentence(s: String) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => s,
    }
}

/// Where a Job that finished leads: what it made, or else what it worked on.
fn onward(job: &Job) -> Option<Link> {
    if let Some(outcome) = Outcome::of(job) {
        let label = match outcome {
            Outcome::Audit(_) => "Open the audit",
            Outcome::Selection(_) => "Open the selection",
            Outcome::Build(_) => "Open the build",
            Outcome::Repair { .. } => "Open the repair",
        };
        return Some(Link::new(outcome.href(), label));
    }
    match (job.kind, Subject::of(job)) {
        (JobKind::Calibration, _) => None,
        (JobKind::Check, Subject::LibraryFolder(id)) => Some(Link::new(
            tab_link(
                &format!("/libraries/{id}"),
                Some(FolderTab::Drift.key()),
                &[],
            ),
            "Open the check",
        )),
        (_, subject @ Subject::LibraryFolder(_)) => {
            Some(Link::new(subject.href(), "Open the library folder"))
        }
        (_, subject @ Subject::IngestFolder(_)) => {
            Some(Link::new(subject.href(), "Open the ingest folder"))
        }
        (_, subject @ Subject::AdditionsSet(_)) => {
            Some(Link::new(subject.href(), "Open the additions"))
        }
        (_, Subject::SourceSet(name)) if name.is_empty() => None,
        (_, subject @ Subject::SourceSet(_)) => {
            Some(Link::new(subject.href(), "Open the source set"))
        }
    }
}

/// "stopped at 412 of 620 files", or "stopped" when how far it got isn't known.
fn how_far(verb: &str, reached: Option<Reached>) -> String {
    match reached {
        Some(r) if r.total > 0 => format!("{verb} at {} of {} {}", r.done, r.total, r.unit),
        _ => verb.to_string(),
    }
}

fn removals(removed: usize) -> &'static str {
    match removed {
        0 => " Nothing was removed.",
        1 => " 1 file was removed.",
        _ => "",
    }
}

/// What the part shows for `job`. `reached` is how far it got, as its last progress or its Build's tally said, and
/// `removed`, for a Build, the built files it removed. `watched` is whether the part saw it before it ended: only
/// then is an ending announced.
pub fn shown(job: &Job, reached: Option<Reached>, removed: Option<usize>, watched: bool) -> Shown {
    let what = what(job);
    let ended = if watched { Live::Polite } else { Live::Off };
    let to_job = Link::new(job_href(job.id), "Open the job");
    let why = Link::new(job_href(job.id), "Open the job to see why.");
    let error = job
        .error
        .as_deref()
        .and_then(|e| e.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(str::to_string);
    let removed = |text: String| match removed {
        Some(n) if n > 1 => format!("{text} {n} files were removed."),
        Some(n) => format!("{text}{}", removals(n)),
        None => text,
    };
    match job.state {
        JobState::Queued => Shown {
            kind: waiting(JobState::Queued),
            live: Live::Polite,
            text: sentence(format!(
                "{what} is queued. It starts when the jobs ahead of it finish."
            )),
            error: None,
            link: Some(to_job),
        },
        JobState::Running => Shown {
            kind: waiting(JobState::Running),
            live: Live::Polite,
            text: sentence(format!("{what} is running.")),
            error: None,
            link: Some(to_job),
        },
        JobState::Succeeded => Shown {
            kind: MessageKind::DONE,
            live: ended,
            text: sentence(format!("{what} finished.")),
            error: None,
            link: onward(job),
        },
        JobState::Failed => Shown {
            kind: MessageKind::PROBLEM,
            live: if watched { Live::Alert } else { Live::Off },
            text: removed(sentence(format!(
                "{}.",
                how_far(&format!("{what} stopped"), reached)
            ))),
            error,
            link: Some(why),
        },
        JobState::Interrupted => {
            let cancelled = job.cancel_requested_at.is_some();
            let verb = if cancelled {
                "was cancelled"
            } else {
                "was interrupted"
            };
            Shown {
                kind: MessageKind::NOTICE,
                live: ended,
                text: removed(sentence(format!(
                    "{}.",
                    how_far(&format!("{what} {verb}"), reached)
                ))),
                error,
                link: Some(if cancelled { to_job } else { why }),
            }
        }
    }
}

/// What a screen's action said: a Job it started, followed to its end; something made at once, with where it is;
/// or a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Job(Box<Job>),
    Made { text: String, link: Option<Link> },
    Refused(String),
}

/// The reports of a screen's actions, failures first, for `ShowReports` to draw. An action clears them as it starts,
/// and each request it makes adds its own.
#[derive(Debug, Clone, Copy)]
pub struct Reports(RwSignal<Vec<Report>>);

impl Default for Reports {
    fn default() -> Self {
        Self::new()
    }
}

impl Reports {
    pub fn new() -> Reports {
        Reports(RwSignal::new(Vec::new()))
    }

    pub fn clear(self) {
        self.0.set(Vec::new());
    }

    pub fn add(self, report: Report) {
        self.0.update(|r| {
            r.push(report);
            r.sort_by_key(|r| !matches!(r, Report::Refused(_)));
        });
    }

    /// `started` as a Job you follow, or the problem it was refused with, prefixed by `doing`: "Couldn't audit NES".
    pub fn started(self, started: Result<Job, String>, doing: &str) {
        self.add(match started {
            Ok(job) => Report::Job(Box::new(job)),
            Err(problem) => Report::Refused(format!("{doing}: {problem}")),
        });
    }

    pub fn get(self) -> Vec<Report> {
        self.0.get()
    }
}

/// Draws `reports`.
#[component]
pub fn ShowReports(reports: Reports) -> impl IntoView {
    move || {
        reports
            .get()
            .into_iter()
            .map(|report| match report {
                Report::Job(job) => view! { <JobOutcome job=job.id seed=*job /> }.into_any(),
                Report::Made { text, link } => view! { <Made text link /> }.into_any(),
                Report::Refused(message) => view! { <Problem message /> }.into_any(),
            })
            .collect_view()
    }
}

/// Something you asked for, done at once, and the link to it.
#[component]
pub fn Made(
    #[prop(into)] text: String,
    #[prop(default = None)] link: Option<Link>,
) -> impl IntoView {
    view! {
        <Message kind=MessageKind::DONE>
            {text}
            {link.map(|l| view! { " " <A href=l.href>{l.label}</A> })}
        </Message>
    }
}

/// What the part holds of the Job: the last the book or a fetch showed, whether it saw the Job before it ended, and
/// the last progress it saw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    job: Job,
    watched: bool,
    reached: Option<Reached>,
}

/// The outcome of Job `job`, live. `seed`, the Job a request returned, goes into the live book so the screen knows of
/// it before its event arrives. `cancel` offers to cancel a Build or a Repair while it runs.
#[component]
pub fn JobOutcome(
    job: i64,
    #[prop(optional)] seed: Option<Job>,
    #[prop(optional)] cancel: bool,
) -> impl IntoView {
    let live = use_live();
    if let Some(seed) = seed.clone() {
        live.book.update(|b| {
            b.accept(seed);
        });
    }
    let fetched = RwSignal::new(None::<Job>);
    let unfetched = RwSignal::new(None::<String>);
    let retry = RwSignal::new(0u64);
    let held = Memo::new(move |prev: Option<&Option<Held>>| {
        let prev = prev.cloned().flatten();
        let now = live.book.with(|b| b.get(job).cloned());
        let (current, progress) = match now {
            Some(l) => (Some(l.job), l.progress),
            None => {
                let known = prev.as_ref().map(|h| h.job.clone());
                let newer = match (fetched.get(), known) {
                    (Some(f), Some(k)) => Some(if f.version >= k.version { f } else { k }),
                    (f, k) => f.or(k),
                };
                (newer, None)
            }
        };
        let current = current?;
        let watched = prev.as_ref().is_some_and(|h| h.watched) || !current.state.is_final();
        let reached = progress
            .map(|p| Reached {
                done: p.files_done,
                total: p.files_total,
                unit: unit(current.kind),
            })
            .or_else(|| prev.and_then(|h| h.reached));
        Some(Held {
            job: current,
            watched,
            reached,
        })
    });
    // Fetched when the book doesn't have it: at first, and after a resync dropped it before its ending arrived.
    Effect::new(move |_| {
        live.settled.track();
        retry.track();
        let in_book = live.book.with_untracked(|b| b.get(job).is_some());
        if !held.with_untracked(|h| needs_fetch(in_book, h.as_ref().map(|h| &h.job))) {
            return;
        }
        leptos::task::spawn_local(async move {
            match api::get::<Job>(&format!("/jobs/{job}")).await {
                Ok(found) => {
                    unfetched.set(None);
                    fetched.set(Some(found));
                }
                Err(message) => unfetched.set(Some(message)),
            }
        });
    });
    let progress = Memo::new(move |_| {
        live.book.with(|b| {
            b.get(job)
                .and_then(|l| l.progress.map(|p| (p.files_done, p.files_total)))
        })
    });
    // A Build that stopped says how far it got, and what it removed, from its items.
    let build = Memo::new(move |_| {
        held.with(|h| {
            h.as_ref().and_then(|h| {
                (h.job.kind == JobKind::Build
                    && matches!(h.job.state, JobState::Failed | JobState::Interrupted))
                .then_some(h.job.build)
                .flatten()
            })
        })
    });
    let tallied = LocalResource::new(move || {
        let build = build.get();
        async move {
            match build {
                Some(id) => api::get::<BuildDetail>(&format!("/builds/{id}"))
                    .await
                    .ok()
                    .map(|d| tally(&d)),
                None => None,
            }
        }
    });
    let shown = Memo::new(move |_| {
        held.get().map(|h| {
            let tally = tallied.get().flatten();
            let reached = tally.map(|t| t.reached).or(h.reached);
            (
                shown(&h.job, reached, tally.and_then(|t| t.removed), h.watched),
                h.job,
            )
        })
    });
    let problem = RwSignal::new(None::<String>);
    let cancelling = RwSignal::new(false);
    let ask_cancel = move |_| {
        cancelling.set(true);
        leptos::task::spawn_local(async move {
            let done =
                api::post::<_, Job>(&format!("/jobs/{job}/cancellation"), &serde_json::json!({}))
                    .await;
            cancelling.set(false);
            match done {
                Ok(_) => problem.set(None),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        {move || {
            shown.get().map(|(s, j)| {
                let running = j.state == JobState::Running;
                let control = (cancel && running && matches!(j.kind, JobKind::Build | JobKind::Repair)).then(|| {
                    if j.cancel_requested_at.is_some() {
                        let note = if j.kind == JobKind::Build {
                            " You cancelled it: the file it is writing finishes, files already in place stay, and \
                             the rest are left for a new plan."
                        } else {
                            " You cancelled it: the change it started finishes, and the rest are skipped."
                        };
                        view! { <span class="note">{note}</span> }.into_any()
                    } else {
                        let label = if j.kind == JobKind::Build { "Cancel the build" } else { "Cancel the repair" };
                        view! {
                            " "
                            <button type="button" disabled=move || cancelling.get() on:click=ask_cancel>
                                {label}
                            </button>
                        }
                        .into_any()
                    }
                });
                let meter = running.then(|| {
                    let unit = unit(j.kind);
                    move || match progress.get() {
                        Some((done, total)) => view! {
                            <span class="progress">
                                <ProgressMeter done total unit />
                                <span class="data" aria-hidden="true">{format!("{done} / {total} {unit}")}</span>
                            </span>
                        }
                        .into_any(),
                        None => view! { <span class="note">"Starting"</span> }.into_any(),
                    }
                });
                let ready = build_finished(&j).then(|| {
                    view! {
                        " "
                        <img class="ready" src="/stage-select-ready.svg" alt="" width="58" height="14" />
                    }
                });
                view! {
                    <Message kind=s.kind live=s.live>
                        <p>
                            {s.text}
                            {ready}
                            {s.link.map(|l| view! { " " <A href=l.href>{l.label}</A> })}
                            {control}
                        </p>
                        {s.error.map(|e| view! { <p class="note">{e}</p> })}
                        {meter}
                    </Message>
                }
            })
        }}
        {move || problem.get().map(|message| view! { <Problem message /> })}
        {move || {
            unfetched.get().map(|message| {
                view! {
                    <Problem message=format!("Couldn't load job #{job}: {message}") />
                    <div class="form">
                        <button type="button" on:click=move |_| retry.update(|n| *n += 1)>"Try again"</button>
                    </div>
                }
            })
        }}
    }
}

#[cfg(test)]
mod tests {
    use stage_select_api::build::{BuildItem, BuildState, BuildSummary};

    use super::*;

    fn job(kind: JobKind, state: JobState) -> Job {
        serde_json::from_value(serde_json::json!({
            "id": 41,
            "kind": kind,
            "state": state,
            "source_set": "NES",
            "dat_revision": "",
            "actor": "Operator",
            "attempt": 1,
            "workers": {"readers": 1, "hashers": 1},
            "created_at": "2025-01-01T00:00:00Z",
            "version": 1,
        }))
        .unwrap()
    }

    #[test]
    fn a_queued_or_running_job_is_announced_politely_and_links_to_the_job() {
        for state in [JobState::Queued, JobState::Running] {
            let s = shown(&job(JobKind::Audit, state), None, None, false);
            assert_eq!(s.live, Live::Polite);
            assert_eq!(s.kind.glyph, state.look().glyph);
            assert_eq!(s.link, Some(Link::new("/activity?row=41", "Open the job")));
        }
        let s = shown(&job(JobKind::Audit, JobState::Queued), None, None, false);
        assert_eq!(
            s.text,
            "The audit of NES is queued. It starts when the jobs ahead of it finish."
        );
    }

    #[test]
    fn a_finished_job_links_to_what_it_made() {
        let mut audit = job(JobKind::Audit, JobState::Succeeded);
        audit.audit = Some(12);
        let s = shown(&audit, None, None, true);
        assert_eq!((s.kind, s.live), (MessageKind::DONE, Live::Polite));
        assert_eq!(s.text, "The audit of NES finished.");
        assert_eq!(s.link, Some(Link::new("/audits/12", "Open the audit")));

        let mut build = job(JobKind::Build, JobState::Succeeded);
        build.library_folder = Some(7);
        build.build = Some(3);
        assert_eq!(
            shown(&build, None, None, true).link,
            Some(Link::new("/builds/3", "Open the build"))
        );

        let mut repair = job(JobKind::Repair, JobState::Succeeded);
        repair.repair = Some(9);
        assert_eq!(
            shown(&repair, None, None, true).link.unwrap().href,
            "/source-sets/NES?tab=repair&plan=9"
        );
    }

    #[test]
    fn a_finished_job_that_made_nothing_links_to_what_it_worked_on() {
        let mut check = job(JobKind::Check, JobState::Succeeded);
        check.library_folder = Some(7);
        assert_eq!(
            shown(&check, None, None, true).link,
            Some(Link::new("/libraries/7?tab=drift", "Open the check"))
        );
        let mut scan = job(JobKind::IngestScan, JobState::Succeeded);
        scan.ingest_folder = Some("My Drop".into());
        let s = shown(&scan, None, None, true);
        assert_eq!(s.text, "The scan of My Drop finished.");
        assert_eq!(
            s.link,
            Some(Link::new("/ingest/My%20Drop", "Open the ingest folder"))
        );
        let mut calibration = job(JobKind::Calibration, JobState::Succeeded);
        calibration.source_set = String::new();
        assert_eq!(shown(&calibration, None, None, true).link, None);
    }

    #[test]
    fn a_build_that_failed_says_how_far_it_got_what_it_removed_and_leads_to_the_job() {
        let mut build = job(JobKind::Build, JobState::Failed);
        build.error = Some("the disk is full\nwhile writing Pong (USA).zip".into());
        let reached = Some(Reached {
            done: 412,
            total: 620,
            unit: "files",
        });
        let s = shown(&build, reached, Some(0), true);
        assert_eq!(s.kind, MessageKind::PROBLEM);
        assert_eq!(
            s.text,
            "The build stopped at 412 of 620 files. Nothing was removed."
        );
        assert_eq!(s.error.as_deref(), Some("the disk is full"));
        assert_eq!(
            s.link,
            Some(Link::new("/activity?row=41", "Open the job to see why."))
        );
        assert_eq!(
            shown(&build, reached, Some(3), true).text,
            "The build stopped at 412 of 620 files. 3 files were removed."
        );
        assert_eq!(
            shown(&build, reached, Some(1), true).text,
            "The build stopped at 412 of 620 files. 1 file was removed."
        );
        assert_eq!(shown(&build, None, None, true).text, "The build stopped.");
    }

    #[test]
    fn only_a_failure_you_watched_happen_is_an_alert() {
        let failed = job(JobKind::Selection, JobState::Failed);
        assert_eq!(shown(&failed, None, None, true).live, Live::Alert);
        assert_eq!(shown(&failed, None, None, false).live, Live::Off);
        let done = job(JobKind::Selection, JobState::Succeeded);
        assert_eq!(shown(&done, None, None, false).live, Live::Off);
    }

    #[test]
    fn an_interrupted_job_is_a_notice_and_a_cancelled_one_says_so() {
        let mut stopped = job(JobKind::Repair, JobState::Interrupted);
        let reached = Some(Reached {
            done: 3,
            total: 10,
            unit: "steps",
        });
        let s = shown(&stopped, reached, None, true);
        assert_eq!((s.kind, s.live), (MessageKind::NOTICE, Live::Polite));
        assert_eq!(
            s.text,
            "The repair of NES was interrupted at 3 of 10 steps."
        );
        assert_eq!(s.link.unwrap().label, "Open the job to see why.");
        stopped.cancel_requested_at = Some("2025-01-01T00:01:00Z".into());
        let s = shown(&stopped, reached, None, true);
        assert_eq!(s.text, "The repair of NES was cancelled at 3 of 10 steps.");
        assert_eq!(s.link.unwrap().label, "Open the job");
    }

    #[test]
    fn a_job_missing_from_the_live_book_is_fetched_until_it_ends() {
        let running = job(JobKind::Build, JobState::Running);
        let failed = job(JobKind::Build, JobState::Failed);
        assert!(needs_fetch(false, None), "a job the book never had");
        assert!(
            needs_fetch(false, Some(&running)),
            "a resync dropped it while it ran: its ending was missed"
        );
        assert!(!needs_fetch(false, Some(&failed)), "its ending is known");
        assert!(!needs_fetch(true, None), "the book has it");
        assert!(!needs_fetch(true, Some(&running)));
    }

    #[test]
    fn a_builds_tally_counts_its_written_files_of_all_and_the_files_it_removed() {
        let item = |kind, state| BuildItem {
            target: "x.zip".into(),
            game: "x".into(),
            kind,
            state,
            error: None,
        };
        let summary = BuildSummary {
            id: 3,
            folder: 7,
            plan: 2,
            job: 41,
            state: BuildState::Failed,
            generation: 1,
            selection: 1,
            audit: 1,
            dat_revision: String::new(),
            started_at: "t".into(),
            finished_at: None,
            applied: 2,
            failed: 1,
            pending: 3,
            uncertain: 1,
            planned: Default::default(),
        };
        let mut detail = BuildDetail {
            build: summary,
            items: vec![
                item(ItemKind::Remove, ItemState::Applied),
                item(ItemKind::Copy, ItemState::Applied),
                item(ItemKind::Remove, ItemState::Pending),
                item(ItemKind::Copy, ItemState::Failed),
                item(ItemKind::Copy, ItemState::Publishing),
            ],
        };
        assert_eq!(
            tally(&detail),
            Tally {
                reached: Reached {
                    done: 2,
                    total: 8,
                    unit: "files"
                },
                removed: Some(1)
            },
            "a file still being put in place counts toward the whole"
        );
        detail.build.planned.place = 1;
        assert_eq!(
            tally(&detail).removed,
            None,
            "placing deletes source set originals, which the items don't count"
        );
        detail.build.planned.place = 0;
        detail.build.planned.returned = 1;
        assert_eq!(
            tally(&detail).removed,
            None,
            "returning deletes placed files, which the items don't count"
        );
    }
}
