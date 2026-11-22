//! Runs queued Jobs on blocking threads, at most as many at once as the pace allows of the concurrent Ceiling, and
//! broadcasts their progress and states.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use title_select_api::fetch::{
    FetchArtifact, FetchOutcome, FetchProvider, FetchReport, FetchTrigger, SUPPORT_SYSTEM,
};
use title_select_api::romm::{RommReconciled, RommReconciliation};
use title_select_api::{Job, JobEvent, JobKind, JobProgress, SettingsChanged};
use title_select_engine::Progress;
use title_select_engine::calibrate::Limits;
use title_select_selection::Engine;
use title_select_store::Store;
use title_select_store::fetch::Fetcher;
use title_select_store::romm::Released;
use title_select_store::romm::{PAGE, ScanStop, ScanTiming};
use tokio::sync::{Notify, broadcast};

use crate::follow_ups::{self, FollowUps};
use crate::notify::Notifier;
use crate::throughput::Latest;

/// The least time between two progress events of one Job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// How long the runner waits after the store fails to hand it a Job.
const RETRY: Duration = Duration::from_secs(1);

/// Wakes the runner and carries every Job's events to subscribers.
pub struct Jobs {
    wake: Notify,
    events: broadcast::Sender<JobEvent>,
    /// Told of each Job's end.
    pub notifier: Arc<Notifier>,
    /// The follow-up scan pending for each Ingest Folder.
    pub follow_ups: FollowUps,
    /// How the RomM scan an ended Build requests waits on RomM.
    romm_scan: ScanTiming,
    /// Stops every RomM scan still followed, so the Service can stop without waiting on RomM.
    romm_stop: ScanStop,
    /// Each Running Job's latest rates, for its progress events.
    pub latest: Latest,
    /// When each step of a Calibration stops; tests shorten them.
    pub calibration: Limits,
}

impl Jobs {
    pub fn new(notifier: Arc<Notifier>, romm_scan: ScanTiming) -> Jobs {
        Jobs {
            wake: Notify::new(),
            events: broadcast::channel(1024).0,
            notifier,
            follow_ups: FollowUps::default(),
            romm_scan,
            romm_stop: ScanStop::default(),
            latest: Latest::default(),
            calibration: Limits::default(),
        }
    }

    /// Compares RomM's view of the Library Folder `folder` with what its Builds made now, as you asked, waiting on
    /// RomM as the scan an ended Build requests does. `None` when RomM is off or doesn't scan the folder.
    pub fn reconcile_romm(
        &self,
        store: &Weak<Store>,
        folder: i64,
    ) -> title_select_store::Result<Option<RommReconciliation>> {
        let kept =
            Store::reconcile_romm(store, folder, None, PAGE, &self.romm_scan, &self.romm_stop)?;
        if let Some(kept) = &kept {
            self.reconciled(kept);
        }
        Ok(kept)
    }

    /// Announces that `kept` is now its Library Folder's latest comparison with RomM.
    fn reconciled(&self, kept: &RommReconciliation) {
        self.publish(JobEvent::RommReconciled(RommReconciled {
            folder_id: kept.folder_id,
        }));
    }

    /// Stops following every RomM scan, now and from now on, and records each in `store` as no longer followed.
    /// Called as the Service stops: a scan's task never holds the Store while it waits on RomM, so it may be gone
    /// before the task could record the stop itself.
    pub fn stop_romm_scans(&self, store: &Store) {
        self.romm_stop.stop();
        if let Err(err) = store.recover_romm_scans() {
            eprintln!("title-select: the RomM scans followed weren't recorded as stopped: {err}");
        }
    }

    /// Announces a Job's new state, and wakes the runner in case it can claim one now: this one, Queued, or one
    /// that waited for this one to end, as a stat walk waits for a Repair or Build of its Source Set.
    pub fn changed(&self, job: Job) {
        let wake = job.state == title_select_api::JobState::Queued || job.state.is_final();
        self.notifier.job_changed(&job);
        self.publish(JobEvent::State(Box::new(job)));
        if wake {
            self.wake.notify_one();
        }
    }

    /// Wakes the runner, as when the pace changes, in case it may start another Job now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn publish(&self, event: JobEvent) {
        let _ = self.events.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.events.subscribe()
    }
}

/// Requests the RomM scan the ended Build Job `id` owes, if any, on a task of its own: it may wait minutes on RomM,
/// and neither holds the Library Folder nor counts against the Jobs that may run at once (#141). Each step of the
/// scan is announced as a change of the Job.
fn request_romm_scan(store: &Arc<Store>, jobs: &Arc<Jobs>, id: i64) {
    let (store, jobs) = (Arc::downgrade(store), jobs.clone());
    tokio::task::spawn_blocking(move || {
        let timing = jobs.romm_scan;
        let ended =
            match Store::request_romm_scan(&store, id, &timing, &jobs.romm_stop, &mut |job| {
                jobs.changed(job.clone())
            }) {
                Ok(ended) => ended,
                Err(err) => {
                    eprintln!("title-select: job {id}: the RomM scan wasn't requested: {err}");
                    None
                }
            };
        // The Additions sync is bounded and quick; the comparison may page through a large platform, so it goes last.
        sync_romm_additions(&store, &jobs, id);
        if let Some(job) = ended {
            reconcile_romm(&store, &jobs, &job);
        }
    });
}

/// Once the scan the Build Job `id` requested has finished, keeps RomM's "Additions" collection in step (#142), on
/// the scan's own task. Each step is announced as a change of the Job.
fn sync_romm_additions(store: &Weak<Store>, jobs: &Arc<Jobs>, id: i64) {
    let timing = jobs.romm_scan;
    if let Err(err) = Store::sync_romm_additions(store, id, &timing, &jobs.romm_stop, &mut |job| {
        jobs.changed(job.clone())
    }) {
        eprintln!("title-select: job {id}: RomM's Additions collection wasn't kept in step: {err}");
    }
}

/// Deletes, on a task of its own, RomM's "Additions" collection a settings update left to delete, if any (#142). One
/// RomM refuses stays recorded, and is tried again at the next start and before the next sync.
pub fn delete_released_romm_collection(store: &Arc<Store>) {
    let store = Arc::downgrade(store);
    tokio::task::spawn_blocking(
        move || match Store::delete_released_romm_collection(&store) {
            Ok(Released::Failed {
                collection_id,
                error,
            }) => eprintln!(
                "title-select: RomM's Additions collection {collection_id} wasn't deleted: {error}. It is tried again \
                 at the next start and before the next sync"
            ),
            Err(err) => {
                eprintln!("title-select: RomM's Additions collection wasn't deleted: {err}")
            }
            Ok(_) => {}
        },
    );
}

/// Compares RomM's view of the Library Folder the ended Build Job `job` changed with what its Builds made, once the
/// scan it requested finished (#143). A comparison that fails is kept saying why; it never fails the Job.
fn reconcile_romm(store: &Weak<Store>, jobs: &Jobs, job: &Job) {
    match Store::reconcile_romm_scan(store, job, &jobs.romm_scan, &jobs.romm_stop) {
        Ok(Some(kept)) => jobs.reconciled(&kept),
        Ok(None) => {}
        Err(err) => eprintln!(
            "title-select: job {}: RomM's view of its library folder wasn't compared: {err}",
            job.id
        ),
    }
}

/// Claims and runs Queued Jobs forever, starting one only while fewer run than the pace allows now. A slower pace
/// never stops a running Job: the runner waits for enough to end.
pub async fn run(store: Arc<Store>, jobs: Arc<Jobs>, engine: Option<Arc<Engine>>) {
    delete_released_romm_collection(&store);
    let running = Arc::new(AtomicUsize::new(0));
    loop {
        // The pace's share of the concurrent Jobs Ceiling, as it is now: a change wakes the runner.
        let allowed = store.concurrent_in_force().unwrap_or(1);
        if running.load(Ordering::SeqCst) >= allowed {
            jobs.wake.notified().await;
            continue;
        }
        let slot = Slot::take(&running);
        let claimed = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.claim_job()).await
        };
        let job = match claimed {
            Ok(Ok(Some(job))) => job,
            Ok(Ok(None)) => {
                drop(slot);
                jobs.wake.notified().await;
                continue;
            }
            Ok(Err(err)) => {
                eprintln!("title-select: cannot claim a job: {err}");
                drop(slot);
                tokio::time::sleep(RETRY).await;
                continue;
            }
            Err(err) => {
                eprintln!("title-select: cannot claim a job: {err}");
                drop(slot);
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        if job.kind == JobKind::Calibration {
            eprintln!("title-select: job {} started: calibration", job.id);
        } else {
            eprintln!(
                "title-select: job {} started: {} of {}",
                job.id,
                job.kind.as_str(),
                job.ingest_folder.as_deref().unwrap_or(&job.source_set)
            );
        }
        // Metered from now, against the readers and hash workers the Job may use before the pace.
        store.meter().start(job.id, job.workers);
        jobs.changed(job.clone());
        let (store, jobs, engine) = (store.clone(), jobs.clone(), engine.clone());
        tokio::spawn(async move {
            let (id, version, kind) = (job.id, job.version, job.kind);
            let ended = {
                let (store, jobs) = (store.clone(), jobs.clone());
                tokio::task::spawn_blocking(move || match kind {
                    JobKind::Audit
                    | JobKind::Build
                    | JobKind::Check
                    | JobKind::Repair
                    | JobKind::Calibration => run_one(&store, &jobs, kind, id, version),
                    JobKind::Additions => run_additions(&store, id),
                    JobKind::Selection => run_selection(&store, engine, id),
                    JobKind::IngestScan => run_ingest_scan(&store, id),
                    JobKind::StatWalk => run_stat_walk(&store, id),
                })
                .await
            };
            let job = match ended {
                Ok(job) => job,
                Err(err) => {
                    let store = store.clone();
                    let message = format!("the {} stopped unexpectedly: {err}", kind.as_str());
                    tokio::task::spawn_blocking(move || store.fail_job(id, &message).ok())
                        .await
                        .ok()
                        .flatten()
                }
            };
            // Its throughput was recorded as it ended, in the same write.
            store.end_metering(id);
            if let Some(job) = job {
                eprintln!("title-select: job {} {}", job.id, job.state.as_str());
                // Set before the scan's end is announced, so that whoever sees it end sees its follow-up too.
                if job.kind == JobKind::IngestScan {
                    let (store, jobs, job) = (store.clone(), jobs.clone(), job.clone());
                    let _ = tokio::task::spawn_blocking(move || {
                        follow_ups::scan_ended(&store, &jobs.follow_ups, &job)
                    })
                    .await;
                }
                let build = job.kind == JobKind::Build;
                jobs.changed(job);
                if build {
                    request_romm_scan(&store, &jobs, id);
                }
            }
            // An Audit a DAT imported from an Ingest Folder owes waits for the Job it met to end.
            let owed = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.fulfil_ingest_reaudits()).await
            };
            match owed {
                Ok(Ok(owed)) => {
                    for why in owed.failed {
                        eprintln!("title-select: cannot queue an owed audit: {why}");
                    }
                    owed.queued.into_iter().for_each(|job| jobs.changed(job));
                }
                Ok(Err(err)) => eprintln!("title-select: cannot queue an owed audit: {err}"),
                Err(err) => eprintln!("title-select: cannot queue an owed audit: {err}"),
            }
            // An Audit a finished repair owes waits for the Job it met to end too.
            let owed = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.fulfil_repair_audits()).await
            };
            match owed {
                Ok(Ok(owed)) => {
                    for why in owed.failed {
                        eprintln!("title-select: cannot queue an owed audit: {why}");
                    }
                    owed.queued.into_iter().for_each(|job| jobs.changed(job));
                }
                Ok(Err(err)) => eprintln!("title-select: cannot queue an owed audit: {err}"),
                Err(err) => eprintln!("title-select: cannot queue an owed audit: {err}"),
            }
            // Additions Jobs a Repair or Build Job queued as it ended publish what it kept aside.
            let queued = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.queued_additions_jobs()).await
            };
            if let Ok(Ok(queued)) = queued {
                queued.into_iter().for_each(|job| jobs.changed(job));
            }
            drop(slot);
            jobs.wake.notify_one();
        });
    }
}

/// One Job counted as running until dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(running: &Arc<AtomicUsize>) -> Slot {
        running.fetch_add(1, Ordering::SeqCst);
        Slot(running.clone())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs one claimed Audit, Build, Check, Repair or Calibration Job, publishing its progress, and returns the Job as it ended.
fn run_one(store: &Store, jobs: &Jobs, kind: JobKind, id: i64, version: u64) -> Option<Job> {
    let last = Mutex::new(None::<Instant>);
    let progress = |p: Progress| {
        let mut last = last.lock().unwrap();
        let done = p.files_done == p.files_total;
        if done || last.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
            *last = Some(Instant::now());
            jobs.publish(JobEvent::Progress(JobProgress {
                job: id,
                version,
                files_done: p.files_done,
                files_total: p.files_total,
                rates: jobs.latest.of(id),
            }));
        }
    };
    let ran = match kind {
        JobKind::Build => store.run_build_job(id, Some(&progress)),
        JobKind::Check => store.run_check_job(id, Some(&progress)),
        JobKind::Repair => store.run_repair_job(id, Some(&progress)),
        JobKind::Calibration => store.run_calibration(
            id,
            &title_select_store::calibration::Calibrating {
                limits: jobs.calibration,
                progress: Some(&progress),
                ..Default::default()
            },
        ),
        _ => store.run_audit_job(id, Some(&progress)),
    };
    let ended = match ran {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    };
    // What it measured may have changed the Ceilings in force, so every client fetches the settings again, as it
    // does when someone saves them.
    if kind == JobKind::Calibration
        && let Ok(settings) = store.settings()
    {
        jobs.publish(JobEvent::Settings(SettingsChanged {
            pace: settings.pace,
        }));
    }
    ended
}

/// Runs one claimed Additions Job, and returns the Job as it ended.
fn run_additions(store: &Store, id: i64) -> Option<Job> {
    match store.run_additions_job(id) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// Runs one claimed scan of an Ingest Folder, and returns the Job as it ended.
fn run_ingest_scan(store: &Store, id: i64) -> Option<Job> {
    match store.run_ingest_scan_job(id) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// Runs one claimed stat walk of a Source Set, and returns the Job as it ended.
fn run_stat_walk(store: &Store, id: i64) -> Option<Job> {
    match store.run_stat_walk_job(id) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}

/// How often the schedule looks for Source Sets due a stat walk.
const CHECK_DUE_EVERY: Duration = Duration::from_secs(60);

/// Queues a stat walk of each audited Source Set last checked, or else last audited, more than the settings'
/// `check_every_minutes` ago, looking every minute from now; none while it is 0. A walk still queued or running is
/// left to finish.
pub async fn schedule_stat_walks(store: Arc<Store>, jobs: Arc<Jobs>) {
    let mut tick = tokio::time::interval(CHECK_DUE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let store = store.clone();
        let queued = tokio::task::spawn_blocking(move || {
            let minutes = store.settings()?.source_sets.check_every_minutes;
            if minutes == 0 {
                return Ok(Vec::new());
            }
            store.enqueue_stat_walks("schedule", Duration::from_secs(minutes.saturating_mul(60)))
        })
        .await;
        match queued {
            Ok(Ok(queued)) => queued.into_iter().for_each(|job| jobs.changed(job)),
            Ok(Err(err)) => eprintln!("title-select: cannot queue scheduled checks: {err}"),
            Err(err) => eprintln!("title-select: cannot queue scheduled checks: {err}"),
        }
    }
}

/// How often the schedule looks for fetches due, from a minute after the Service starts.
const FETCH_DUE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Fetches, from each DAT Provider turned on in the settings then, what is due for the systems your Source Sets
/// Follow: each at most once a day (ADR 0012). A fetch runs alongside Jobs, and never queues one.
pub async fn schedule_fetches(store: Arc<Store>, fetcher: Arc<Fetcher>) {
    let start = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut tick = tokio::time::interval_at(start, FETCH_DUE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let settings = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.settings()).await
        };
        let fetch = match settings {
            Ok(Ok(settings)) => settings.fetch,
            Ok(Err(err)) => {
                eprintln!("title-select: cannot read the settings to fetch: {err}");
                continue;
            }
            Err(err) => {
                eprintln!("title-select: cannot read the settings to fetch: {err}");
                continue;
            }
        };
        for provider in FetchProvider::ALL.into_iter().filter(|p| fetch.enabled(*p)) {
            let (store, fetcher) = (store.clone(), fetcher.clone());
            let fetched = tokio::task::spawn_blocking(move || {
                log_fetch(
                    provider,
                    store.fetch(&fetcher, provider, FetchTrigger::Schedule, "schedule"),
                )
            })
            .await;
            match fetched {
                Ok(Ok(_)) | Ok(Err(title_select_store::Error::Conflict(_))) => {}
                Ok(Err(err)) => eprintln!(
                    "title-select: cannot fetch from {}: {err}",
                    provider.dat_provider()
                ),
                Err(err) => eprintln!(
                    "title-select: cannot fetch from {}: {err}",
                    provider.dat_provider()
                ),
            }
        }
    }
}

/// Logs what a fetch brought, and where it stopped if it did.
pub fn log_fetch(
    provider: FetchProvider,
    fetched: Result<FetchReport, title_select_store::Error>,
) -> Result<FetchReport, title_select_store::Error> {
    if let Ok(report) = &fetched {
        let name = provider.dat_provider();
        for attempt in &report.attempts {
            if attempt.outcome == Some(FetchOutcome::Added) {
                let what = match attempt.artifact {
                    FetchArtifact::Release => "release",
                    FetchArtifact::Dat => "DAT revision",
                    FetchArtifact::Cues => "cue pack",
                    FetchArtifact::Sbi => "SBI pack",
                    FetchArtifact::SupportCommit => "support files commit",
                    FetchArtifact::Catver => "catver.ini",
                    FetchArtifact::Languages => "languages.ini",
                    FetchArtifact::Bestgames => "bestgames.ini",
                };
                let version = attempt
                    .version
                    .as_deref()
                    .or(attempt.release.as_deref())
                    .or(attempt.commit.as_deref());
                let version = version.map_or_else(String::new, |v| format!(" ({v})"));
                let of = if attempt.system == SUPPORT_SYSTEM {
                    String::new()
                } else {
                    format!(" of {}", attempt.system)
                };
                eprintln!("title-select: fetched a new {what}{of}{version} from {name}");
            }
        }
        if let Some(why) = &report.stopped {
            eprintln!("title-select: a fetch from {name} stopped, and kept what it had: {why}");
        }
    }
    fetched
}

/// How often the schedule looks for Ingest Folders due a scan.
const SCAN_DUE_EVERY: Duration = Duration::from_secs(60);

/// Queues a scan of each Ingest Folder with a schedule in the settings, every `scan_every_minutes` from the Service's
/// start or the schedule's. A scan still queued or running is left to finish.
pub async fn schedule_ingest_scans(store: Arc<Store>, jobs: Arc<Jobs>) {
    let mut last = HashMap::<String, Instant>::new();
    let mut tick = tokio::time::interval(SCAN_DUE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let settings = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.settings()).await
        };
        let settings = match settings {
            Ok(Ok(settings)) => settings,
            Ok(Err(err)) => {
                eprintln!("title-select: cannot read the settings to scan: {err}");
                continue;
            }
            Err(err) => {
                eprintln!("title-select: cannot read the settings to scan: {err}");
                continue;
            }
        };
        // The Roots are read afresh at each look, so one registered or unregistered since is seen without a restart.
        let roots = store.roots();
        last.retain(|root, _| roots.root(root).is_some_and(|r| r.is_ingest()));
        for root in roots.ingest().map(|r| &r.name) {
            let Some(minutes) = settings.ingest(root).scan_every_minutes else {
                last.remove(root);
                continue;
            };
            let every = Duration::from_secs(minutes.saturating_mul(60));
            if last.get(root).is_some_and(|at| at.elapsed() < every) {
                continue;
            }
            last.insert(root.clone(), Instant::now());
            let (store, root) = (store.clone(), root.clone());
            let queued =
                tokio::task::spawn_blocking(move || store.enqueue_ingest_scan(&root, "schedule"))
                    .await;
            match queued {
                Ok(Ok(job)) => jobs.changed(job),
                Ok(Err(title_select_store::Error::JobActive { .. }))
                | Ok(Err(title_select_store::Error::Conflict(_))) => {}
                Ok(Err(err)) => eprintln!("title-select: cannot queue a scheduled scan: {err}"),
                Err(err) => eprintln!("title-select: cannot queue a scheduled scan: {err}"),
            }
        }
    }
}

/// Runs one claimed Selection Job: the engine on its DAT Revision with its Profile version. Returns the Job as it
/// ended.
fn run_selection(store: &Arc<Store>, engine: Option<Arc<Engine>>, id: i64) -> Option<Job> {
    let made = (|| {
        let engine = engine.ok_or_else(|| crate::selections::UNAVAILABLE.to_owned())?;
        let input = store.selection_input(id).map_err(|e| e.to_string())?;
        let (selection, _) = crate::selections::select(
            store,
            &engine,
            &input.dat,
            &input.dat_revision.file,
            &input.profile,
            input.coverage,
            None,
        )
        .map_err(|e| e.to_string())?;
        Ok::<_, String>(selection)
    })();
    let ended = match made {
        Ok(selection) => store
            .record_selection(id, &selection)
            .map_err(|e| e.to_string()),
        Err(message) => Err(message),
    };
    match ended {
        Ok(job) => Some(job),
        Err(message) => {
            eprintln!("title-select: job {id}: {message}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &message).ok(),
            }
        }
    }
}
