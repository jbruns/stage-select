//! Stage Select's Service: the `/api/v1` JSON API over the store, durable Audit and Selection Jobs, live Selection
//! previews and Operator auth (ADR 0003).
//!
//! The Service owns the store, and its lock, for as long as it runs.

mod auth;
mod follow_ups;
mod jobs;
mod metrics;
mod notify;
mod routes;
mod selections;
mod throughput;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use stage_select_api::JobKind;
use stage_select_api::calibration::CalibrationStale;
pub use stage_select_engine::calibrate::Limits as CalibrationLimits;
use stage_select_store::Store;
use stage_select_store::fetch::{Endpoints, Fetcher};
use stage_select_store::romm::ScanTiming;

pub use auth::{AuthSettings, DEV_IDENTITY, IDENTITY_HEADER, TOKEN_ACTOR, TRUSTED_PROXIES};
pub use ipnet::IpNet;
pub use routes::UPLOAD_LIMIT;

/// The environment variable naming the file that holds the Operator token.
pub const TOKEN_FILE_ENV: &str = auth::TOKEN_FILE.1;

/// Everything the Service needs to start.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The store directory.
    pub store: PathBuf,
    /// How the Service knows a request comes from the Operator, beside the token.
    pub auth: AuthSettings,
    /// The chdman to run. When `None`, the program `STAGE_SELECT_CHDMAN` names, else `chdman` on the `PATH`.
    pub chdman: Option<PathBuf>,
    pub listen: SocketAddr,
    /// The file holding the Operator token, if the token is enabled.
    pub token_file: Option<PathBuf>,
    /// The workbench's built bundle, served at `/` (ADR 0005).
    pub ui: Option<PathBuf>,
    /// The clone lists the selection engine reads; by default `$STAGE_SELECT_CLONE_LISTS`, else
    /// `/opt/clone-lists`. Without them the Service still starts, and refuses Selections and previews.
    pub clone_lists: Option<PathBuf>,
    /// Leaves queued Jobs queued, so tests can observe them.
    #[doc(hidden)]
    pub hold_jobs: bool,
    /// Where fetches are sent instead of the official endpoints, so tests can stand in for a provider.
    #[doc(hidden)]
    pub fetch_endpoints: Option<Endpoints>,
    /// How a RomM scan a Build requests waits on RomM, so tests needn't wait minutes.
    #[doc(hidden)]
    pub romm_scan_timing: Option<ScanTiming>,
    /// When each step of a Calibration stops, so tests needn't wait seconds a step.
    #[doc(hidden)]
    pub calibration_limits: Option<CalibrationLimits>,
    /// The wait before the second and third attempt at a webhook event, so tests needn't wait seconds.
    #[doc(hidden)]
    pub notify_backoff: Option<[std::time::Duration; 2]>,
    /// The mount points whose folders the workbench suggests registering as Roots, instead of the common ones, so
    /// tests needn't read the Host's.
    #[doc(hidden)]
    pub mount_points: Option<Vec<PathBuf>>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("refusing to start: {0}")]
    Auth(String),
    #[error(transparent)]
    Store(#[from] stage_select_store::Error),
    #[error("cannot listen on {0}: {1}")]
    Listen(SocketAddr, std::io::Error),
    #[error("cannot start the runtime: {0}")]
    Runtime(std::io::Error),
}

/// A Service ready to serve: auth checked, the store open and its interrupted Jobs recovered, the address bound.
struct Prepared {
    listener: std::net::TcpListener,
    app: axum::Router,
    state: routes::AppState,
    hold_jobs: bool,
    /// The webhook's delivery.
    delivery: notify::Delivery,
}

fn prepare(settings: Settings) -> Result<Prepared, StartError> {
    let Settings {
        store,
        auth,
        chdman,
        listen,
        token_file,
        ui,
        clone_lists,
        hold_jobs,
        fetch_endpoints,
        romm_scan_timing,
        calibration_limits,
        notify_backoff,
        mount_points,
    } = settings;
    let engine = match stage_select_selection::Engine::locate(clone_lists.as_deref()) {
        Ok(engine) => Some(Arc::new(engine)),
        Err(err) => {
            eprintln!("stage-select: selections are unavailable: {err}");
            None
        }
    };
    let auth = auth::Auth::new(&auth, token_file.as_deref(), listen)?;
    let store = Store::open(&store)?;
    store.use_chdman(chdman);
    if let Some(engine) = &engine {
        match engine.regions() {
            Ok(regions) => store.set_regions(move |game| regions.of(game)),
            Err(err) => eprintln!("stage-select: coverage is unavailable: {err}"),
        }
    }
    let (notifier, mut delivery) = notify::Notifier::new(&store.settings()?.notify);
    delivery.backoff = notify_backoff.unwrap_or(notify::BACKOFF);
    // The Ceilings are worked out before any Job runs. Each Job records the Ceilings it starts with; the pools it
    // shares with other Jobs say how many of its workers work, file by file.
    store.refresh_ceilings()?;
    for interruption in store.recover_jobs(store.job_workers())? {
        let job = &interruption.job;
        match &interruption.requeued {
            Some(again) => eprintln!(
                "stage-select: job {} was interrupted by a stop; queued again as job {}",
                job.id, again.id
            ),
            None if job.kind == JobKind::Build => eprintln!(
                "stage-select: build job {} was interrupted by a stop; plan again to build the rest",
                job.id
            ),
            None if job.kind == JobKind::Check => eprintln!(
                "stage-select: check job {} was interrupted by a stop; check the folder again",
                job.id
            ),
            None if job.kind == JobKind::Calibration => eprintln!(
                "stage-select: calibration job {} was interrupted by a stop; it kept what it measured. Calibrate \
                 again to measure the rest",
                job.id
            ),
            None => eprintln!(
                "stage-select: job {} was interrupted by a stop again; it failed",
                job.id
            ),
        }
    }
    for job in store.recover_romm_scans()? {
        eprintln!(
            "stage-select: job {job}: the RomM scan was still being followed at the last stop; check it in RomM"
        );
    }
    for (build, reason) in store.recover_builds()? {
        eprintln!(
            "stage-select: build {build} stopped and its folder was left as it is: {reason}. Plans for the folder are \
             blocked until it is cleaned up when the service next starts"
        );
    }
    for (deletion, reason) in store.recover_kept_deletions()? {
        eprintln!(
            "stage-select: deletion {deletion} of kept files stopped and was not finished: {reason}. Its kept files \
             stay recorded, and plans for the folder are blocked, until it is finished when the service next starts"
        );
    }
    for (id, reason) in store.recover_library_folder_moves()? {
        eprintln!(
            "stage-select: move {id} of a play library folder stopped and was not finished: {reason}. The folder \
             stays held until it is settled when the service next starts"
        );
    }
    for (id, reason) in store.recover_ingest_moves()? {
        match id {
            0 => eprintln!("stage-select: after the last stop: {reason}"),
            _ => eprintln!(
                "stage-select: a move out of an ingest folder stopped and was not finished: {reason}. It is \
                 tried again when the service next starts"
            ),
        }
    }
    for job in store.recover_repairs()? {
        eprintln!(
            "stage-select: repair {} was not finished; job {} was queued to finish it",
            job.repair.unwrap_or_default(),
            job.id
        );
    }
    for (deletion, reason) in store.recover_source_kept_deletions()? {
        eprintln!(
            "stage-select: deletion {deletion} of kept files of a source set stopped and was not finished: {reason}. \
             It is finished when the service next starts"
        );
    }
    for job in store.recover_additions()? {
        eprintln!(
            "stage-select: the additions of {} were not all settled; job {} was queued to settle them",
            job.additions_set.as_deref().unwrap_or_default(),
            job.id
        );
    }
    // Audits owed when the service stopped: queued now, or once the Job they wait for ends.
    for why in store.fulfil_repair_audits()?.failed {
        eprintln!("stage-select: cannot queue an owed audit: {why}");
    }
    for why in store.fulfil_ingest_reaudits()?.failed {
        eprintln!("stage-select: cannot queue an owed audit: {why}");
    }
    // Only what happens from here on is notified: the store reports each occurrence as it commits.
    let notifier = Arc::new(notifier);
    {
        let notifier = notifier.clone();
        store.set_observer(move |o| notifier.observed(o));
    }
    let listener =
        std::net::TcpListener::bind(listen).map_err(|e| StartError::Listen(listen, e))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| StartError::Listen(listen, e))?;
    // Never calibrated again by itself: Stale results keep applying until you calibrate.
    let calibration = store.calibration(stage_select_store::calibration::cpus())?;
    if !calibration.stale.is_empty() {
        eprintln!(
            "stage-select: the calibration is stale ({}); calibrate again to measure what changed",
            calibration
                .stale
                .iter()
                .map(stale_reason)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let mut jobs = jobs::Jobs::new(notifier, romm_scan_timing.unwrap_or_default());
    jobs.calibration = calibration_limits.unwrap_or_default();
    let state = routes::AppState {
        store: Arc::new(store),
        jobs: Arc::new(jobs),
        engine,
        previews: Arc::default(),
        fetcher: Arc::new(Fetcher::new(
            fetch_endpoints.unwrap_or_else(Endpoints::official),
        )),
        coverage: Arc::default(),
        mount_points: mount_points
            .unwrap_or_else(|| {
                stage_select_store::roots::MOUNT_POINTS
                    .iter()
                    .map(PathBuf::from)
                    .collect()
            })
            .into(),
    };
    let app = routes::router(state.clone(), Arc::new(auth), ui.as_deref());
    Ok(Prepared {
        listener,
        app,
        state,
        hold_jobs,
        delivery,
    })
}

/// Why the calibration is Stale, as the log says it.
fn stale_reason(stale: &CalibrationStale) -> String {
    match stale {
        CalibrationStale::Cpus { measured, now } => {
            format!("it measured {measured} CPUs and the service has {now}")
        }
        CalibrationStale::Unmeasured { root } => format!("root {root} isn't measured"),
        CalibrationStale::Moved {
            root,
            measured,
            now,
        } => {
            format!("root {root} moved from {measured} to {now}")
        }
    }
}

impl Prepared {
    fn addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("a bound listener")
    }

    /// Serves until the process ends; must run inside the runtime.
    async fn serve(self) -> std::io::Result<()> {
        let state = self.state;
        tokio::spawn(notify::deliver(self.delivery));
        if !self.hold_jobs {
            tokio::spawn(jobs::schedule_ingest_scans(
                state.store.clone(),
                state.jobs.clone(),
            ));
            tokio::spawn(follow_ups::run(state.store.clone(), state.jobs.clone()));
            tokio::spawn(jobs::schedule_stat_walks(
                state.store.clone(),
                state.jobs.clone(),
            ));
            tokio::spawn(jobs::schedule_fetches(
                state.store.clone(),
                state.fetcher.clone(),
            ));
            tokio::spawn(throughput::run(state.store.clone(), state.jobs.clone()));
            tokio::spawn(jobs::run(state.store, state.jobs, state.engine));
        }
        let listener = tokio::net::TcpListener::from_std(self.listener)?;
        axum::serve(
            listener,
            self.app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, StartError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Runtime)
}

/// Runs the Service until SIGTERM or Ctrl-C. A Job still Running then is left for the next start to recover.
pub fn run(settings: Settings) -> Result<(), StartError> {
    let prepared = prepare(settings)?;
    let addr = prepared.addr();
    let runtime = runtime()?;
    let (jobs, store) = (prepared.state.jobs.clone(), prepared.state.store.clone());
    let served = runtime.block_on(async {
        eprintln!(
            "stage-select: serving {} on http://{addr}",
            stage_select_api::PREFIX
        );
        tokio::select! {
            served = prepared.serve() => served,
            () = stop_signal() => Ok(()),
        }
    });
    jobs.stop_romm_scans(&store);
    drop((jobs, store));
    runtime.shutdown_background();
    served.map_err(|e| StartError::Listen(addr, e))
}

async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
    eprintln!("stage-select: stopping");
}

/// A Service running on its own runtime thread, stopped when dropped.
pub struct Handle {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Starts the Service on a background thread, for tests and embedding.
pub fn spawn(settings: Settings) -> Result<Handle, StartError> {
    let prepared = prepare(settings)?;
    let addr = prepared.addr();
    let runtime = runtime()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let (jobs, store) = (prepared.state.jobs.clone(), prepared.state.store.clone());
    let thread = std::thread::spawn(move || {
        runtime.block_on(async {
            tokio::select! {
                served = prepared.serve() => {
                    if let Err(err) = served {
                        eprintln!("stage-select: {err}");
                    }
                }
                _ = stopped => {}
            }
        });
        // A RomM scan's task stops waiting at once; an Audit can't be stopped.
        jobs.stop_romm_scans(&store);
        drop((jobs, store));
        // Lets a running Audit finish and release the store, so it can be opened again.
        runtime.shutdown_timeout(std::time::Duration::from_secs(60));
    });
    Ok(Handle {
        addr,
        stop: Some(stop),
        thread: Some(thread),
    })
}

impl Handle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The Service's base URL, such as `http://127.0.0.1:8080`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
