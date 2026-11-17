//! Audit results read through a Source Set's Coverage (ADR 0021): its summary counts, its counts by region, and a
//! preview of a change. The Audit itself is unchanged, so a change of Coverage needs no new Audit.

use std::cell::RefCell;
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

/// How many times a summary is read again while newer Audits keep finishing.
const ATTEMPTS: usize = 3;

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
    sets: Mutex<HashMap<String, (AuditSummary, Arc<Tally>)>>,
}

impl Tallies {
    fn order(&self, engine: &Engine) -> ApiResult<Arc<Vec<String>>> {
        if let Some(order) = self.order.get() {
            return Ok(order.clone());
        }
        let read = Arc::new(engine.names()?.regions);
        Ok(self.order.get_or_init(|| read).clone())
    }

    /// The Source Set `set`'s latest Audit and its count by its Games' regions; None before its first Audit.
    /// `seen` is the latest Audit as the caller last read it: its tally is reused when kept. Otherwise the latest
    /// Audit is read again with its rows in one snapshot, so a newer Audit that finished meanwhile, and pruned `seen`,
    /// is counted instead. [`Error::RegionsUnavailable`] without the store's region reader. Blocks.
    fn tally(
        &self,
        store: &Store,
        set: &str,
        seen: Option<&AuditSummary>,
    ) -> ApiResult<Option<(AuditSummary, Arc<Tally>)>> {
        if let Some(seen) = seen
            && let Some((kept, tally)) = self.sets.lock().unwrap().get(set)
            && Fingerprint::from(kept) == Fingerprint::from(seen)
        {
            return Ok(Some((kept.clone(), tally.clone())));
        }
        let regions = store.region_reader()?;
        // Both callbacks count into these; the store calls them one at a time.
        let tally = RefCell::new(Tally::default());
        let games: RefCell<HashMap<String, Vec<String>>> = RefCell::default();
        let latest = store.latest_audit_rows(
            set,
            |name, status| {
                let of = regions(name);
                tally.borrow_mut().add(of.clone(), status);
                games.borrow_mut().insert(name.to_owned(), of);
            },
            |row| {
                let entry = row.entry;
                let games = games.borrow();
                // An Entry belongs to the Games of the ROMs it counts as and of the ROM its name gives. A covered
                // Game's ROM its bytes also match inside an uncovered Game's ZIP or folder is only Available Elsewhere
                // for that Game, which never counts toward its status (ADR 0017), so the Entry stays the uncovered
                // Game's and is left out of a Coverage that doesn't cover it.
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
                tally.borrow_mut().add_entry(of, entry.finding, claimed);
            },
        )?;
        let Some(latest) = latest else {
            self.sets.lock().unwrap().remove(set);
            return Ok(None);
        };
        let tally = Arc::new(tally.into_inner());
        self.sets
            .lock()
            .unwrap()
            .insert(set.to_owned(), (latest.clone(), tally.clone()));
        Ok(Some((latest, tally)))
    }

    /// Counts each Source Set's latest Audit through its Coverage: `games` only the covered Games, the rest in
    /// `outside_coverage`, and the Entries its status ranks on in `covered_entries`. When a newer Audit finished
    /// since `summaries` were read, that Source Set's summary is read again whole. Where the Coverage can't be applied, without the store's
    /// region reader or on a failed read, that Source Set's counts stay whole and `coverage_unavailable` says so;
    /// the others are still counted. Drops the tallies of Source Sets no longer registered. Blocks.
    pub(super) fn apply(&self, store: &Store, summaries: &mut [SourceSetSummary]) {
        self.sets
            .lock()
            .unwrap()
            .retain(|name, _| summaries.iter().any(|s| s.set.name == *name));
        let readable = store.region_reader().is_ok();
        for summary in summaries {
            if summary.set.coverage.is_none() || summary.latest.is_none() {
                continue;
            }
            if !readable {
                summary.coverage_unavailable = true;
                continue;
            }
            if let Err(err) = self.apply_one(store, summary) {
                eprintln!(
                    "title-select: source set {}'s coverage couldn't be applied: {err}",
                    summary.set.name
                );
                summary.coverage_unavailable = true;
            }
        }
    }

    /// Counts `summary`'s latest Audit through its Coverage. When a newer Audit finished since `summary` was read,
    /// reads the whole summary again, so its staleness, last verification and Repair agree with the Audit counted,
    /// up to [`ATTEMPTS`] times. On an error, `summary` is left as it was read.
    fn apply_one(&self, store: &Store, summary: &mut SourceSetSummary) -> Result<(), String> {
        let mut current = summary.clone();
        for _ in 0..ATTEMPTS {
            let (Some(coverage), Some(latest)) = (&current.set.coverage, &mut current.latest)
            else {
                *summary = current;
                return Ok(());
            };
            let Some((read, tally)) = self
                .tally(store, &current.set.name, Some(latest))
                .map_err(|ApiError(_, err, _)| err)?
            else {
                return Err("its latest audit is gone".into());
            };
            if Fingerprint::from(&read) != Fingerprint::from(&*latest) {
                current = store
                    .source_set_summary(&summary.set.name)
                    .map_err(|err| err.to_string())?;
                continue;
            }
            let (covered, outside) = tally.split(Some(coverage));
            latest.games = covered;
            current.outside_coverage = Some(outside);
            if !latest.pruned {
                current.covered_entries = Some(tally.covered_entries(Some(coverage)));
            }
            *summary = current;
            return Ok(());
        }
        Err("newer audits kept finishing while it was read".into())
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
        let seen = latest_audit(store, &name)?;
        let (audit, tally) = state
            .coverage
            .tally(store, &name, seen.as_ref())?
            .ok_or_else(|| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    format!("source set {name} has no audit yet"),
                    None,
                )
            })?;
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
        let seen = latest_audit(store, &name)?;
        let read = match seen {
            Some(seen) => state.coverage.tally(store, &name, Some(&seen))?,
            None => None,
        };
        let missing = |tally: &Tally, coverage: Option<&[String]>| {
            tally
                .split(coverage)
                .0
                .get(&GameStatus::Missing)
                .copied()
                .unwrap_or(0)
        };
        let (missing_before, missing_after) = match &read {
            Some((_, tally)) => (
                Some(missing(tally, set.coverage.as_deref())),
                Some(missing(tally, coverage.as_deref())),
            ),
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
            audit: read.map(|(a, _)| a.id),
            missing_before,
            missing_after,
            stale_library_folders,
        }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use title_select_api::AuditEvidence;
    use title_select_store::config::Config;

    use super::*;

    fn dat(version: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<datafile><header><name>Test - System</name><version>{version}</version><homepage>No-Intro</homepage></header>
<game name="Pong (USA)"><rom name="Pong (USA).a26" size="4" crc="00000000"/></game>
<game name="Tennis (Europe)"><rom name="Tennis (Europe).a26" size="4" crc="00000000"/></game></datafile>"#
        )
    }

    /// A summary read before a newer Audit finished is read again whole, not only its latest Audit: the Source Set's
    /// staleness and last verification then agree with the Audit counted.
    #[test]
    fn a_summary_read_before_a_newer_audit_finished_is_read_again_whole() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sets/Test - System")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            dir.path().join("sets").display().to_string()
        ))
        .unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        store.set_regions(|game| {
            vec![
                if game.contains("(Europe)") {
                    "Europe"
                } else {
                    "USA"
                }
                .to_owned(),
            ]
        });
        store
            .import_dat_bytes(dat("1").as_bytes(), "1.dat", None)
            .unwrap();
        let set = "Test - System";
        store
            .add_source_set(&config, "sets", set, "No-Intro/Test - System")
            .unwrap();
        store.set_follow(set, true).unwrap();
        store
            .set_coverage(set, Some(&["Europe".to_owned()]))
            .unwrap();
        let audit = |revision: Option<&str>| {
            store
                .audit_source_set(&config, set, revision, AuditEvidence::Incremental)
                .unwrap()
        };
        audit(None);
        let (newer, _) = store
            .import_dat_bytes(dat("2").as_bytes(), "2.dat", None)
            .unwrap();
        let mut summaries = store.source_set_summaries().unwrap();
        assert!(!summaries[0].set.stale.is_empty(), "a newer DAT revision");
        audit(Some(&newer.sha256));

        Tallies::default().apply(&store, &mut summaries);

        assert_eq!(summaries[0], {
            let mut now = store.source_set_summary(set).unwrap();
            let latest = now.latest.as_mut().unwrap();
            latest.games = [(GameStatus::Missing, 1)].into();
            now.outside_coverage = Some(title_select_api::OutsideCoverage {
                games: [(GameStatus::Missing, 1)].into(),
                unknown_region: 0,
            });
            now.covered_entries = Some(Default::default());
            now
        });
        assert!(summaries[0].set.stale.is_empty(), "verified against it");
    }
}
