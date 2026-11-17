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
use title_select_api::report::EvidenceLevel;
use title_select_api::{
    AuditSummary, CoveragePreview, CoverageStaleFolder, GameStatus, RegionCounts, SetCoverage,
    SourceSetSummary,
};
use title_select_selection::Engine;
use title_select_store::{Error, Store};

use super::{ApiError, ApiResult, AppState};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/source-sets/{name}/regions", get(region_counts))
        .route("/source-sets/{name}/coverage/preview", post(preview))
}

/// What tells one finished Audit from any other: Audit ids are reused once an Audit is forgotten, so the id alone
/// doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    id: i64,
    started_at: String,
    finished_at: String,
    dat_revision_sha256: String,
}

impl From<&AuditSummary> for Fingerprint {
    fn from(audit: &AuditSummary) -> Self {
        Fingerprint {
            id: audit.id,
            started_at: audit.started_at.clone(),
            finished_at: audit.finished_at.clone(),
            dat_revision_sha256: audit.dat_revision_sha256.clone(),
        }
    }
}

/// The engine's region names in its order, read once, and the [`Tally`] of each Source Set's latest Audit, read once
/// per Audit: a finished Audit never changes. A Source Set keeps one tally, replaced when a newer Audit supersedes it
/// and dropped when the Source Set leaves the registry. Games' regions are read with the store's reader
/// ([`Store::region_reader`]), so Coverage reads them as Selection does.
#[derive(Default)]
pub struct Tallies {
    order: OnceLock<Arc<Vec<String>>>,
    sets: Mutex<HashMap<String, (Fingerprint, Arc<Tally>)>>,
}

impl Tallies {
    fn order(&self, engine: &Engine) -> ApiResult<Arc<Vec<String>>> {
        if let Some(order) = self.order.get() {
            return Ok(order.clone());
        }
        let read = Arc::new(engine.names()?.regions);
        Ok(self.order.get_or_init(|| read).clone())
    }

    /// The Source Set `set`'s finished Audit `audit` counted by its Games' regions. [`Error::RegionsUnavailable`]
    /// without the store's region reader. Blocks.
    fn tally(&self, store: &Store, set: &str, audit: &AuditSummary) -> ApiResult<Arc<Tally>> {
        let fingerprint = Fingerprint::from(audit);
        if let Some((kept, tally)) = self.sets.lock().unwrap().get(set)
            && *kept == fingerprint
        {
            return Ok(tally.clone());
        }
        let regions = store.region_reader()?;
        let mut tally = Tally::default();
        let mut games: HashMap<String, Vec<String>> = HashMap::new();
        store.for_each_game_status(audit.id, |name, status| {
            let of = regions(name);
            tally.add(of.clone(), status);
            games.insert(name.to_owned(), of);
        })?;
        if !audit.pruned {
            store.for_each_entry(audit.id, |row| {
                let entry = row.entry;
                let of = entry
                    .roms
                    .iter()
                    .chain(&entry.named_rom)
                    .map(|rom| {
                        games
                            .get(&rom.game)
                            .cloned()
                            .unwrap_or_else(|| regions(&rom.game))
                    })
                    .collect();
                let claimed = entry
                    .evidence
                    .as_ref()
                    .is_some_and(|e| e.level == EvidenceLevel::Claim);
                tally.add_entry(of, entry.finding, claimed);
                true
            })?;
        }
        let tally = Arc::new(tally);
        self.sets
            .lock()
            .unwrap()
            .insert(set.to_owned(), (fingerprint, tally.clone()));
        Ok(tally)
    }

    /// Counts each Source Set's latest Audit through its Coverage: `games` only the covered Games, the rest in
    /// `outside_coverage`, and the Entries its status ranks on in `covered_entries`. Without the store's region
    /// reader, a covered Source Set's counts stay whole and `coverage_unavailable` says so. Drops the tallies of
    /// Source Sets no longer registered. Blocks.
    pub(super) fn apply(&self, store: &Store, summaries: &mut [SourceSetSummary]) -> ApiResult<()> {
        self.sets
            .lock()
            .unwrap()
            .retain(|name, _| summaries.iter().any(|s| s.set.name == *name));
        let readable = store.region_reader().is_ok();
        for summary in summaries {
            let (Some(coverage), Some(latest)) = (&summary.set.coverage, &mut summary.latest)
            else {
                continue;
            };
            if !readable {
                summary.coverage_unavailable = true;
                continue;
            }
            let tally = self.tally(store, &summary.set.name, latest)?;
            let (covered, outside) = tally.split(Some(coverage));
            latest.games = covered;
            summary.outside_coverage = Some(outside);
            if !latest.pruned {
                summary.covered_entries = Some(tally.covered_entries(Some(coverage)));
            }
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

/// The Source Set `name`'s latest finished Audit, if any.
fn latest_audit(store: &Store, name: &str) -> Result<Option<AuditSummary>, Error> {
    Ok(store.history(name)?.into_iter().next())
}

/// The Source Set's latest Audit counted by region, each region marked covered or not by its Coverage.
async fn region_counts(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<RegionCounts>> {
    let Path(name) = name?;
    state.store.region_reader()?;
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
        let tally = state.coverage.tally(store, &name, &audit)?;
        let order = state.coverage.order(&engine)?;
        Ok(Json(RegionCounts {
            audit: audit.id,
            total: tally.total(),
            regions: tally.by_region(&order, set.coverage.as_deref()),
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
    state.store.region_reader()?;
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
        let (missing_before, missing_after) = match &audit {
            Some(audit) => {
                let tally = state.coverage.tally(store, &name, audit)?;
                (
                    Some(missing(&tally, set.coverage.as_deref())),
                    Some(missing(&tally, coverage.as_deref())),
                )
            }
            None => (None, None),
        };
        let stale_library_folders = store
            .folders_stale_under(&state.config, &name, coverage.as_deref())?
            .into_iter()
            .map(|f| CoverageStaleFolder {
                id: f.id,
                root: f.root,
                path: f.path,
            })
            .collect();
        Ok(Json(CoveragePreview {
            coverage,
            audit: audit.map(|a| a.id),
            missing_before,
            missing_after,
            stale_library_folders,
        }))
    })
    .await
}
