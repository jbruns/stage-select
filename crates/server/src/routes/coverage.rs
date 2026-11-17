//! Audit results read through a Source Set's Coverage (ADR 0021): its summary counts, its counts by region, and a
//! preview of a change. The Audit itself is unchanged, so a change of Coverage needs no new Audit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use title_select_api::coverage::Tally;
use title_select_api::{CoveragePreview, GameStatus, RegionCounts, SetCoverage, SourceSetSummary};
use title_select_selection::{Engine, Regions};
use title_select_store::{Error, Store};

use super::{ApiError, ApiResult, AppState};

/// How many Audits' tallies are kept; past it, the cache starts again.
const KEPT: usize = 64;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/source-sets/{name}/regions", get(region_counts))
        .route("/source-sets/{name}/coverage/preview", post(preview))
}

/// The engine's region reader and its region names, read once, and each finished Audit's [`Tally`], read once: a
/// finished Audit never changes.
#[derive(Default)]
pub struct Tallies {
    regions: OnceLock<Arc<(Regions, Vec<String>)>>,
    audits: Mutex<HashMap<i64, Arc<Tally>>>,
}

impl Tallies {
    fn regions(&self, engine: &Engine) -> ApiResult<Arc<(Regions, Vec<String>)>> {
        if let Some(regions) = self.regions.get() {
            return Ok(regions.clone());
        }
        let read = Arc::new((engine.regions()?, engine.names()?.regions));
        Ok(self.regions.get_or_init(|| read).clone())
    }

    /// The finished Audit `audit`'s Games by their regions and Game Status. Blocks.
    fn tally(&self, store: &Store, engine: &Engine, audit: i64) -> ApiResult<Arc<Tally>> {
        if let Some(tally) = self.audits.lock().unwrap().get(&audit) {
            return Ok(tally.clone());
        }
        let regions = self.regions(engine)?;
        let mut tally = Tally::default();
        store.for_each_game_status(audit, |name, status| tally.add(regions.0.of(name), status))?;
        let tally = Arc::new(tally);
        let mut audits = self.audits.lock().unwrap();
        if audits.len() >= KEPT {
            audits.clear();
        }
        audits.insert(audit, tally.clone());
        Ok(tally)
    }

    /// Counts each Source Set's latest Audit through its Coverage: `games` only the covered Games, and the rest in
    /// `outside_coverage`. Without the engine, which reads Games' regions, the counts stay whole. Blocks.
    pub(super) fn apply(
        &self,
        store: &Store,
        engine: Option<&Engine>,
        summaries: &mut [SourceSetSummary],
    ) -> ApiResult<()> {
        let Some(engine) = engine else {
            return Ok(());
        };
        for summary in summaries {
            let (Some(coverage), Some(latest)) = (&summary.set.coverage, &mut summary.latest)
            else {
                continue;
            };
            let (covered, outside) = self.tally(store, engine, latest.id)?.split(Some(coverage));
            latest.games = covered;
            summary.outside_coverage = Some(outside);
        }
        Ok(())
    }
}

/// Runs `f` on a blocking thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))?
}

/// The id of the Source Set `name`'s latest finished Audit, if any.
fn latest_audit(store: &Store, name: &str) -> Result<Option<i64>, Error> {
    Ok(store.history(name)?.first().map(|a| a.id))
}

/// The Source Set's latest Audit counted by region, each region marked covered or not by its Coverage.
async fn region_counts(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<RegionCounts>> {
    let Path(name) = name?;
    let engine = state.engine()?;
    blocking(move || {
        let store = &state.store;
        let set = store.source_set(&name)?;
        let audit = latest_audit(store, &name)?.ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                format!("source set {name} has no audit yet"),
                None,
            )
        })?;
        let tally = state.coverage.tally(store, &engine, audit)?;
        let order = state.coverage.regions(&engine)?;
        Ok(Json(RegionCounts {
            audit,
            total: tally.total(),
            regions: tally.by_region(&order.1, set.coverage.as_deref()),
        }))
    })
    .await
}

/// What setting the Source Set's Coverage as asked would change: its covered Missing Games before and after, and the
/// Library Folders it would make Stale. Saves nothing.
async fn preview(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<SetCoverage>, JsonRejection>,
) -> ApiResult<Json<CoveragePreview>> {
    let Path(name) = name?;
    let Json(request) = body?;
    let engine = state.engine()?;
    let coverage = match &request.coverage {
        Some(regions) => Some(super::coverage(&state, regions).await?),
        None => None,
    };
    blocking(move || {
        let store = &state.store;
        let set = store.source_set(&name)?;
        let audit = latest_audit(store, &name)?;
        let missing = |tally: &Tally, coverage: Option<&[String]>| {
            tally
                .split(coverage)
                .0
                .get(&GameStatus::Missing)
                .copied()
                .unwrap_or(0)
        };
        let (missing_before, missing_after) = match audit {
            Some(audit) => {
                let tally = state.coverage.tally(store, &engine, audit)?;
                (
                    Some(missing(&tally, set.coverage.as_deref())),
                    Some(missing(&tally, coverage.as_deref())),
                )
            }
            None => (None, None),
        };
        let stale_library_folders = store.coverage_stale_folders(&name, coverage.as_deref())?;
        Ok(Json(CoveragePreview {
            coverage,
            audit,
            missing_before,
            missing_after,
            stale_library_folders,
        }))
    })
    .await
}
