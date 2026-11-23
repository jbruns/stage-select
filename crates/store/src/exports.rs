//! Exports of what a Source Set lacks, from its latest Audit: the Fixdat and the have/miss CSV. They only read; a
//! Fixdat names what to acquire, and Stage Select never acquires it.

use std::collections::HashSet;

use stage_select_api::AuditSummary;
use stage_select_api::selection::Role;
use stage_select_engine::dat::Dat;
use stage_select_engine::fixdat::{Lacking, Scope};

use super::sets::set_by_name;
use super::{Error, Result, Store};

/// What a Fixdat lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixdatScope {
    /// Every Game of the DAT.
    Dat,
    /// The Games the Source Set's Coverage covers (ADR 0021).
    Coverage,
    /// The winners of the Selection with this id.
    Selection(i64),
}

/// An exported file: the name to save it as, and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub file_name: String,
    pub content: String,
}

impl Store {
    /// The Fixdat of Source Set `name` from its latest Audit, of `scope`: by default, the Games its Coverage covers
    /// when it has one, else the whole DAT. Its header names the Coverage or Selection it is limited to.
    pub fn fixdat(&self, name: &str, scope: Option<FixdatScope>) -> Result<Export> {
        self.lacking(name, scope, |lacking| Export {
            file_name: lacking.fixdat_file_name(),
            content: lacking.fixdat(),
        })
    }

    /// The have/miss CSV of Source Set `name` from its latest Audit, of the whole DAT or of the winners of Selection
    /// `selection`. It reports every Game, covered or not, so it ignores Coverage.
    pub fn have_miss(&self, name: &str, selection: Option<i64>) -> Result<Export> {
        let scope = Some(selection.map_or(FixdatScope::Dat, FixdatScope::Selection));
        self.lacking(name, scope, |lacking| Export {
            file_name: lacking.have_miss_file_name(),
            content: lacking.have_miss(),
        })
    }

    fn lacking(
        &self,
        name: &str,
        scope: Option<FixdatScope>,
        export: impl FnOnce(&Lacking) -> Export,
    ) -> Result<Export> {
        let set = self.read(|c| set_by_name(c, name))?;
        let scope = scope.unwrap_or(match set.record.coverage {
            Some(_) => FixdatScope::Coverage,
            None => FixdatScope::Dat,
        });
        if scope == FixdatScope::Coverage && set.record.coverage.is_none() {
            return Err(Error::Refused(format!(
                "source set {name} has no coverage: it covers every game, so export the whole DAT instead"
            )));
        }
        let audit = self
            .source_set_history_by_id(set.id)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                Error::Refused(format!(
                    "source set {name} has no audit yet; audit it to export what it lacks"
                ))
            })?;
        let report = self.report(audit.id)?;
        let dat = Dat::read(&self.blob_path(&report.dat_revision.sha256))
            .map_err(|e| Error::Io(e.to_string()))?;
        let games = match scope {
            FixdatScope::Dat => HashSet::new(),
            FixdatScope::Selection(id) => self.winners(name, set.id, &audit, id)?,
            FixdatScope::Coverage => {
                let covering = self.covering(&set.record)?;
                dat.games
                    .iter()
                    .filter(|game| covering.covers(game))
                    .cloned()
                    .collect()
            }
        };
        let scope = match scope {
            FixdatScope::Dat => None,
            FixdatScope::Selection(selection) => Some(Scope::Selection {
                selection,
                games: &games,
            }),
            FixdatScope::Coverage => Some(Scope::Coverage {
                regions: set.record.coverage.as_deref().expect("refused without one"),
                games: &games,
            }),
        };
        Ok(export(&Lacking {
            dat: &dat,
            report: &report,
            audit: audit.id,
            scope,
        }))
    }

    /// The winners of Selection `id`, refused unless it selected from the registration `set_id` of Source Set `name`
    /// against the DAT Revision of its latest Audit, `audit`. The registration, not the name, decides: an
    /// Unregistered Source Set's name can be registered again.
    fn winners(
        &self,
        name: &str,
        set_id: i64,
        audit: &AuditSummary,
        id: i64,
    ) -> Result<HashSet<String>> {
        let record = self.selection_record(id)?;
        let selected_from: i64 = self.read(|c| {
            Ok(c.query_row(
                "SELECT source_set_id FROM selections WHERE id = ?1",
                [id],
                |r| r.get(0),
            )?)
        })?;
        if selected_from != set_id {
            let whose = if record.source_set == name {
                format!("an earlier registration of {name}, since unregistered")
            } else {
                format!("source set {}", record.source_set)
            };
            return Err(Error::Refused(format!(
                "selection {id} is of {whose}, not of {name} as registered now; choose one of {name}"
            )));
        }
        if record.dat_revision_sha256 != audit.dat_revision_sha256 {
            return Err(Error::Refused(format!(
                "selection {id} is against DAT revision {}, but the latest audit of {name} is against DAT revision \
                 {}; choose a selection against that revision, or audit against the selection's",
                record.dat_revision_version, audit.dat_revision_version
            )));
        }
        Ok(self
            .selection_rows(id)?
            .into_iter()
            .filter(|row| row.role == Role::Winner)
            .map(|row| row.title)
            .collect())
    }
}
