//! Repair and build plans made in the background, so that the workbench can show how far each has got (#362).
//!
//! A plan being made is no Job: it changes nothing, so it neither waits behind a Build for the concurrent-jobs
//! ceiling nor outlives the Service. Each is kept here, in memory, while it is made and for a while after, and the
//! workbench polls it until it ends with a plan or an error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use stage_select_api::build::PlanBuild;
use stage_select_api::planning::{Planning, PlanningKind};
use stage_select_api::repair::PlanRepair;
use stage_select_store::{Error, PlanMeter, Store};

use super::{ApiError, ApiResult, AppState};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/source-sets/{name}/repair-plannings",
            post(start_repair_planning),
        )
        .route("/libraries/{id}/plannings", post(start_build_planning))
        .route("/plannings/{id}", get(planning))
}

/// How long a plan made, or a failure to make one, can still be read.
const KEPT: Duration = Duration::from_secs(10 * 60);

/// What a plan is being made of: only one plan of each is made at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Of {
    Repair(String),
    Build(i64),
}

struct Task {
    id: u64,
    of: Of,
    /// The request the plan is made from, as sent.
    request: serde_json::Value,
    meter: PlanMeter,
    /// The plan made, or why none was.
    result: OnceLock<(Result<i64, String>, Instant)>,
}

impl Task {
    fn view(&self) -> Planning {
        let (kind, source_set, library_folder) = match &self.of {
            Of::Repair(name) => (PlanningKind::Repair, Some(name.clone()), None),
            Of::Build(folder) => (PlanningKind::Build, None, Some(*folder)),
        };
        let result = self.result.get().map(|(result, _)| result);
        Planning {
            id: self.id,
            kind,
            source_set,
            library_folder,
            progress: self.meter.progress(),
            plan: result.and_then(|r| r.as_ref().ok().copied()),
            error: result.and_then(|r| r.as_ref().err().cloned()),
        }
    }

    fn ended_before(&self, now: Instant) -> bool {
        self.result
            .get()
            .is_some_and(|(_, at)| now.duration_since(*at) > KEPT)
    }
}

/// The plans being made, and those made lately.
#[derive(Default)]
pub struct Plannings {
    last: AtomicU64,
    tasks: Mutex<HashMap<u64, Arc<Task>>>,
}

impl Plannings {
    /// Starts making a plan of `of` from `request` with `make`, or joins the plan of it being made from the same
    /// request. A plan of it being made from another request is a conflict.
    fn start(
        &self,
        store: Arc<Store>,
        of: Of,
        request: serde_json::Value,
        make: impl FnOnce(&Store, &PlanMeter) -> Result<i64, Error> + Send + 'static,
    ) -> ApiResult<Planning> {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        tasks.retain(|_, task| !task.ended_before(now));
        if let Some(task) = tasks
            .values()
            .find(|t| t.of == of && t.result.get().is_none())
        {
            if task.request == request {
                return Ok(task.view());
            }
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!(
                    "a plan of {} is being made already, with other choices; plan again once it's ready",
                    match &of {
                        Of::Repair(name) => format!("source set {name}"),
                        Of::Build(folder) => format!("library folder {folder}"),
                    }
                ),
                None,
            ));
        }
        let task = Arc::new(Task {
            id: self.last.fetch_add(1, Ordering::Relaxed) + 1,
            of,
            request,
            meter: PlanMeter::default(),
            result: OnceLock::new(),
        });
        tasks.insert(task.id, task.clone());
        drop(tasks);
        let view = task.view();
        tokio::spawn(async move {
            let made = {
                let task = task.clone();
                tokio::task::spawn_blocking(move || make(&store, &task.meter)).await
            };
            let result = match made {
                Ok(made) => made.map_err(|e| ApiError::from(e).1),
                Err(e) => Err(e.to_string()),
            };
            let _ = task.result.set((result, Instant::now()));
        });
        Ok(view)
    }

    fn get(&self, id: u64) -> Option<Planning> {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks
            .get(&id)
            .filter(|task| !task.ended_before(Instant::now()))
            .map(|task| task.view())
    }
}

/// Starts making a repair plan of the Source Set, as `POST /source-sets/{name}/repairs` would make it; `GET
/// /plannings/{id}` shows how far it has got, and the plan once made.
async fn start_repair_planning(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<PlanRepair>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Planning>)> {
    let Path(name) = name?;
    let Json(request) = body?;
    let sent = serde_json::to_value(&request).unwrap_or_default();
    let planning = state.plannings.start(
        state.store.clone(),
        Of::Repair(name.clone()),
        sent,
        move |store, meter| Ok(store.plan_repair_metered(&name, &request, meter)?.id),
    )?;
    Ok((StatusCode::ACCEPTED, Json(planning)))
}

/// Starts making a build plan of the Library Folder, as `POST /libraries/{id}/plans` would make it; `GET
/// /plannings/{id}` shows how far it has got, and the plan once made.
async fn start_build_planning(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<PlanBuild>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Planning>)> {
    let Path(folder) = id?;
    let Json(request) = body?;
    let sent = serde_json::to_value(&request).unwrap_or_default();
    let planning = state.plannings.start(
        state.store.clone(),
        Of::Build(folder),
        sent,
        move |store, meter| Ok(store.plan_build_metered(folder, &request, meter)?.id),
    )?;
    Ok((StatusCode::ACCEPTED, Json(planning)))
}

async fn planning(
    State(state): State<AppState>,
    id: Result<Path<u64>, PathRejection>,
) -> ApiResult<Json<Planning>> {
    let Path(id) = id?;
    state.plannings.get(id).map(Json).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("no plan {id} is being made, or was made lately"),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::planning::PlanStep;
    use std::sync::mpsc;

    /// Makes plans that end as each is let go.
    struct Env {
        _tmp: tempfile::TempDir,
        store: Arc<Store>,
        plannings: Plannings,
    }

    impl Env {
        fn new() -> Env {
            let tmp = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::open(&tmp.path().join("store")).unwrap());
            Env {
                _tmp: tmp,
                store,
                plannings: Plannings::default(),
            }
        }

        /// Starts a plan of `of` from `request` that waits to be let go, then ends as `then` says.
        fn start(
            &self,
            of: Of,
            request: serde_json::Value,
            then: Result<i64, Error>,
        ) -> (ApiResult<Planning>, mpsc::Sender<()>) {
            let (go, wait) = mpsc::channel();
            let started = self
                .plannings
                .start(self.store.clone(), of, request, move |_, _| {
                    let _ = wait.recv();
                    then
                });
            (started, go)
        }

        async fn ended(&self, id: u64) -> Planning {
            loop {
                let planning = self.plannings.get(id).unwrap();
                if planning.ended() {
                    return planning;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plan_is_made_once_while_it_is_being_made_and_another_request_waits_for_it() {
        let env = Env::new();
        let set = || Of::Repair("Alpha".into());
        let mine = serde_json::json!({"choices": {}});
        let other = serde_json::json!({"choices": {"game:Pong (USA)": "leave"}});

        let (first, go) = env.start(set(), mine.clone(), Ok(7));
        let first = first.ok().unwrap();
        assert_eq!(first.kind, PlanningKind::Repair);
        assert_eq!(first.source_set.as_deref(), Some("Alpha"));
        assert_eq!(first.progress.step, PlanStep::Reading);
        assert!(!first.ended());

        let (again, _) = env.start(set(), mine.clone(), Ok(8));
        assert_eq!(
            again.ok().unwrap().id,
            first.id,
            "the same request joins it"
        );
        let (refused, _) = env.start(set(), other.clone(), Ok(8));
        let refused = refused.err().unwrap();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(refused.1.contains("source set Alpha"), "{}", refused.1);
        let (elsewhere, go_elsewhere) = env.start(Of::Build(1), other.clone(), Ok(9));
        assert_ne!(
            elsewhere.ok().unwrap().id,
            first.id,
            "another plan is made alongside"
        );

        go.send(()).unwrap();
        let made = env.ended(first.id).await;
        assert_eq!((made.plan, made.error), (Some(7), None));
        let (next, go_next) = env.start(set(), other, Ok(10));
        assert_ne!(
            next.ok().unwrap().id,
            first.id,
            "once made, the next request plans again"
        );
        go_next.send(()).unwrap();
        go_elsewhere.send(()).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plan_that_fails_says_why_and_an_unknown_one_is_not_found() {
        let env = Env::new();

        let (started, go) = env.start(
            Of::Build(3),
            serde_json::json!({}),
            Err(Error::NotFound("no library folder 3".into())),
        );
        go.send(()).unwrap();
        let failed = env.ended(started.ok().unwrap().id).await;

        assert_eq!(failed.plan, None);
        assert!(failed.error.unwrap().contains("no library folder 3"));
        assert!(env.plannings.get(999).is_none());
    }
}
