//! Coverage as the store reads it (ADR 0021): which Games of its DAT a Source Set covers. The store doesn't read
//! region tags itself; the caller hands it the Selection engine's reading ([`Store::set_regions`]), so Coverage and
//! Selection agree on every Game's regions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use title_select_api::SourceSetRecord;

use crate::{Error, Result, Store};

/// Reads a Game's regions from its name.
pub(crate) type Regions = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// Which Games a Source Set covers: every Game without a Coverage.
pub(crate) struct Covering {
    regions: Option<(Vec<String>, Regions)>,
    /// Each Game's answer, once asked.
    seen: Mutex<HashMap<String, bool>>,
}

impl Covering {
    /// Whether it covers the Game named `game`.
    pub fn covers(&self, game: &str) -> bool {
        let Some((coverage, regions)) = &self.regions else {
            return true;
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        *seen
            .entry(game.to_owned())
            .or_insert_with(|| title_select_api::coverage::covered(&regions(game), Some(coverage)))
    }
}

impl Store {
    /// Reads each Game's regions from its name with `regions`, as the Selection engine does, for whatever reads a
    /// Source Set's Coverage. Set once; later calls are ignored. Without it, reading a Source Set that has a Coverage
    /// is refused.
    pub fn set_regions(&self, regions: impl Fn(&str) -> Vec<String> + Send + Sync + 'static) {
        let _ = self.regions.set(Arc::new(regions));
    }

    /// Which Games `set` covers.
    pub(crate) fn covering(&self, set: &SourceSetRecord) -> Result<Covering> {
        let regions = match &set.coverage {
            None => None,
            Some(coverage) => {
                let regions = self.regions.get().cloned().ok_or_else(|| {
                    Error::Refused(format!(
                        "source set {} has a coverage, and reading games' regions needs the selection engine's \
                         clone lists, which aren't available; install them, or clear the coverage",
                        set.name
                    ))
                })?;
                Some((coverage.clone(), regions))
            }
        };
        Ok(Covering {
            regions,
            seen: Mutex::default(),
        })
    }
}
