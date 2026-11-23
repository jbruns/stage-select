//! The kept-files view's pure logic: one row model for the kept files of a Library Folder, an Ingest Folder, a Source
//! Set and an Additions Set, what set each aside and where that links, the count and total size the view leads with,
//! and a deletion of kept files, planned and carried out, as one plan whatever folder it is of.

use stage_select_api::build::{
    KeptDeletion, KeptDeletionOutcome, KeptFile, PlanKeptDeletion, RETENTION,
};
use stage_select_api::ingest::IngestKeptFile;
use stage_select_api::repair::{KeptFileReviewed, SourceKeptDeletion, SourceKeptFile};

use crate::build_model::keep_reason_label;
use crate::ingest_model::size;
use crate::parts::status::{ItemOutcome, KeptDeletionKind};
use crate::parts::tabs::tab_href;
use crate::parts::url_state::encode;

/// Where a kept file is kept: each folder Stage Select keeps files aside in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeptPlace {
    LibraryFolder {
        id: i64,
        path: String,
    },
    IngestFolder(String),
    SourceSet(String),
    /// By its system, `<provider>/<name>`.
    AdditionsSet(String),
}

impl KeptPlace {
    fn kind(&self) -> &'static str {
        match self {
            KeptPlace::LibraryFolder { .. } => "library-folder",
            KeptPlace::IngestFolder(_) => "ingest-folder",
            KeptPlace::SourceSet(_) => "source-set",
            KeptPlace::AdditionsSet(_) => "additions-set",
        }
    }

    /// The folder's name.
    pub fn name(&self) -> String {
        match self {
            KeptPlace::LibraryFolder { path, .. } => path.clone(),
            KeptPlace::IngestFolder(root) => root.clone(),
            KeptPlace::SourceSet(name) => name.clone(),
            KeptPlace::AdditionsSet(system) => system.clone(),
        }
    }

    /// The page where you review and delete its kept files.
    pub fn href(&self) -> String {
        let kept = Some("kept");
        match self {
            KeptPlace::LibraryFolder { id, .. } => tab_href(&format!("/libraries/{id}"), [], kept),
            KeptPlace::IngestFolder(root) => {
                tab_href(&format!("/ingest/{}", encode(root)), [], kept)
            }
            KeptPlace::SourceSet(name) => {
                tab_href(&format!("/source-sets/{}", encode(name)), [], kept)
            }
            KeptPlace::AdditionsSet(_) => "/additions".to_string(),
        }
    }

    /// Where its kept files are listed, planned for deletion and their deletions approved, under `/api/v1`.
    pub fn files_path(&self) -> String {
        self.under("kept-files")
    }

    pub fn plan_path(&self) -> String {
        self.under("kept-deletions")
    }

    pub fn approval_path(&self, deletion: i64) -> String {
        format!("{}/approval", self.deletion_path(deletion))
    }

    pub fn deletion_path(&self, deletion: i64) -> String {
        let deletions = match self {
            KeptPlace::LibraryFolder { .. } => "kept-deletions",
            KeptPlace::IngestFolder(_) => "ingest-kept-deletions",
            KeptPlace::SourceSet(_) => "source-kept-deletions",
            KeptPlace::AdditionsSet(_) => "additions-kept-deletions",
        };
        format!("/{deletions}/{deletion}")
    }

    fn under(&self, what: &str) -> String {
        match self {
            KeptPlace::LibraryFolder { id, .. } => format!("/libraries/{id}/{what}"),
            KeptPlace::IngestFolder(root) => format!("/ingest-folders/{}/{what}", encode(root)),
            KeptPlace::SourceSet(name) => format!("/source-sets/{}/{what}", encode(name)),
            KeptPlace::AdditionsSet(system) => {
                format!("/additions-set/{what}?system={}", encode(system))
            }
        }
    }
}

/// What set a kept file aside: the Build, the repair, the retired addition or the ingest plan, each with its own page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetAsideBy {
    Build(i64),
    Repair {
        source_set: String,
        plan: i64,
    },
    /// The addition retired, its file kept.
    Retired(i64),
    IngestPlan {
        root: String,
        plan: i64,
    },
}

impl SetAsideBy {
    /// Terse enough for a cell: "Build #12".
    pub fn label(&self) -> String {
        match self {
            SetAsideBy::Build(id) => format!("Build #{id}"),
            SetAsideBy::Repair { plan, .. } => format!("Repair #{plan}"),
            SetAsideBy::Retired(id) => format!("Addition #{id}"),
            SetAsideBy::IngestPlan { plan, .. } => format!("Plan #{plan}"),
        }
    }

    pub fn href(&self) -> String {
        match self {
            SetAsideBy::Build(id) => format!("/builds/{id}"),
            SetAsideBy::Repair { source_set, plan } => format!(
                "{}&plan={plan}",
                tab_href(
                    &format!("/source-sets/{}", encode(source_set)),
                    [],
                    Some("repair")
                )
            ),
            SetAsideBy::Retired(id) => format!("/additions?row={id}"),
            SetAsideBy::IngestPlan { root, plan } => format!(
                "{}&plan={plan}",
                tab_href(&format!("/ingest/{}", encode(root)), [], Some("plan"))
            ),
        }
    }
}

/// One kept file of any folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptRow {
    pub place: KeptPlace,
    /// Its ID among its kind of folder's kept files.
    pub id: i64,
    pub name: String,
    /// Its name in the retention area, in `<generation>/`.
    pub kept_name: String,
    pub generation: u64,
    pub reason: String,
    pub size: u64,
    pub kept_at: String,
    pub sha256: Option<String>,
    /// The game a Build wrote it for.
    pub game: Option<String>,
    pub set_aside_by: Option<SetAsideBy>,
    /// A directory kept aside whole.
    pub directory: bool,
}

impl KeptRow {
    /// Unique across every folder: kept files are numbered per kind of folder.
    pub fn key(&self) -> String {
        format!("{}-{}", self.place.kind(), self.id)
    }

    /// Where it is in its folder.
    pub fn kept_path(&self) -> String {
        format!("{RETENTION}/{}/{}", self.generation, self.kept_name)
    }

    pub fn library(id: i64, path: &str, f: KeptFile) -> KeptRow {
        KeptRow {
            place: KeptPlace::LibraryFolder {
                id,
                path: path.to_string(),
            },
            id: f.id,
            name: f.name,
            kept_name: f.kept_name,
            generation: f.generation,
            reason: keep_reason_label(f.reason).to_string(),
            size: f.file.size,
            kept_at: f.kept_at,
            sha256: f.sha256,
            game: f.game,
            set_aside_by: Some(SetAsideBy::Build(f.build)),
            directory: f.directory,
        }
    }

    pub fn ingest(root: &str, f: IngestKeptFile) -> KeptRow {
        KeptRow {
            place: KeptPlace::IngestFolder(root.to_string()),
            id: f.id,
            name: f.name,
            kept_name: f.kept_name,
            generation: f.generation,
            reason: f.reason,
            size: f.file.size,
            kept_at: f.kept_at,
            sha256: f.sha256,
            game: None,
            set_aside_by: f.plan.map(|plan| SetAsideBy::IngestPlan {
                root: root.to_string(),
                plan,
            }),
            directory: false,
        }
    }

    pub fn source(name: &str, f: SourceKeptFile) -> KeptRow {
        let by = match (f.plan, f.build) {
            (_, Some(build)) => Some(SetAsideBy::Build(build)),
            (Some(plan), None) => Some(SetAsideBy::Repair {
                source_set: name.to_string(),
                plan,
            }),
            (None, None) => None,
        };
        KeptRow::of_source(KeptPlace::SourceSet(name.to_string()), by, f)
    }

    pub fn additions(system: &str, f: SourceKeptFile) -> KeptRow {
        let by = match (f.plan, f.build) {
            (_, Some(build)) => Some(SetAsideBy::Build(build)),
            (plan, None) => plan.map(SetAsideBy::Retired),
        };
        KeptRow::of_source(KeptPlace::AdditionsSet(system.to_string()), by, f)
    }

    fn of_source(place: KeptPlace, set_aside_by: Option<SetAsideBy>, f: SourceKeptFile) -> KeptRow {
        KeptRow {
            place,
            id: f.id,
            name: f.name,
            kept_name: f.kept_name,
            generation: f.generation,
            reason: f.reason,
            size: f.file.size,
            kept_at: f.kept_at,
            sha256: f.sha256,
            game: None,
            set_aside_by,
            directory: false,
        }
    }
}

/// The kept files, the most recently kept first.
pub fn newest_first(mut rows: Vec<KeptRow>) -> Vec<KeptRow> {
    rows.sort_by(|a, b| b.kept_at.cmp(&a.kept_at));
    rows
}

/// What the view leads with: how many kept files, and how much they hold. "No kept files" when there are none.
pub fn summary(rows: &[KeptRow]) -> String {
    if rows.is_empty() {
        return "No kept files".into();
    }
    let total = rows.iter().map(|r| r.size).sum();
    format!("{}, {}", files(rows.len()), size(total))
}

fn files(n: usize) -> String {
    if n == 1 {
        "1 kept file".into()
    } else {
        format!("{n} kept files")
    }
}

/// Deleting the kept files you checked.
pub fn deleting(rows: &[KeptRow]) -> PlanKeptDeletion {
    PlanKeptDeletion {
        ids: rows.iter().map(|r| r.id).collect(),
        all: false,
    }
}

/// Deleting every kept file of the folder that is still the file kept.
pub fn deleting_all() -> PlanKeptDeletion {
    PlanKeptDeletion {
        ids: Vec::new(),
        all: true,
    }
}

/// One kept file a deletion covers: as it was kept, as it was read when the deletion was planned, and what became
/// of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionRow {
    pub id: i64,
    pub name: String,
    pub kept_name: String,
    pub generation: u64,
    pub size: u64,
    /// What it read as when the deletion was planned: it is deleted only while it still does.
    pub reviewed: Option<String>,
    pub kind: KeptDeletionKind,
    /// Why it was left, or how it went.
    pub detail: Option<String>,
}

impl DeletionRow {
    pub fn kept_path(&self) -> String {
        format!("{RETENTION}/{}/{}", self.generation, self.kept_name)
    }

    /// What became of the file once the deletion was carried out: none while it is still to delete.
    pub fn outcome(&self) -> Option<ItemOutcome> {
        match self.kind {
            KeptDeletionKind::Delete => None,
            KeptDeletionKind::Deleted => Some(ItemOutcome::Done),
            KeptDeletionKind::Left => Some(ItemOutcome::NotDone),
        }
    }
}

/// A deletion of kept files of any folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    pub id: i64,
    pub digest: String,
    pub approved: bool,
    pub finished: bool,
    /// Why it isn't finished yet, when the service could not finish it.
    pub recovery_error: Option<String>,
    pub rows: Vec<DeletionRow>,
    pub owner: DeletionOwner,
}

/// Whose kept files a deletion deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionOwner {
    LibraryFolder(i64),
    /// A source set, additions set or ingest folder by its id, and by the name it is registered under while it is.
    Named {
        id: i64,
        name: Option<String>,
    },
}

impl Deletion {
    /// The one line the review leads with, in numbers.
    pub fn summary(&self) -> String {
        let n = self.rows.len();
        if !self.approved {
            let total = self.rows.iter().map(|r| r.size).sum();
            return format!("Deletes {}, {}.", files(n), size(total));
        }
        if !self.finished {
            return format!("Deleting {}.", files(n));
        }
        let deleted = self
            .rows
            .iter()
            .filter(|r| r.kind == KeptDeletionKind::Deleted)
            .count();
        match n - deleted {
            0 => format!("Deleted {}.", files(n)),
            1 => format!("Deleted {deleted} of {}. 1 was left kept.", files(n)),
            left => format!("Deleted {deleted} of {}. {left} were left kept.", files(n)),
        }
    }

    /// Whether it deletes kept files of `place`: only there is approving it approving what you see.
    pub fn belongs_to(&self, place: &KeptPlace) -> bool {
        match (&self.owner, place) {
            (DeletionOwner::LibraryFolder(id), KeptPlace::LibraryFolder { id: here, .. }) => {
                id == here
            }
            (
                DeletionOwner::Named {
                    name: Some(name), ..
                },
                KeptPlace::IngestFolder(here)
                | KeptPlace::SourceSet(here)
                | KeptPlace::AdditionsSet(here),
            ) => name == here,
            _ => false,
        }
    }

    /// The id of the folder or set whose kept files it deletes, which approving it names.
    pub fn owner_id(&self) -> i64 {
        match self.owner {
            DeletionOwner::LibraryFolder(id) | DeletionOwner::Named { id, .. } => id,
        }
    }

    /// Where its own kept files are, when it was found under `place` of the same kind: none once its owner is
    /// unregistered.
    pub fn owner_href(&self, place: &KeptPlace) -> Option<String> {
        let owner = match (&self.owner, place) {
            (DeletionOwner::LibraryFolder(id), KeptPlace::LibraryFolder { .. }) => {
                KeptPlace::LibraryFolder {
                    id: *id,
                    path: String::new(),
                }
            }
            (
                DeletionOwner::Named {
                    name: Some(name), ..
                },
                KeptPlace::IngestFolder(_),
            ) => KeptPlace::IngestFolder(name.clone()),
            (
                DeletionOwner::Named {
                    name: Some(name), ..
                },
                KeptPlace::SourceSet(_),
            ) => KeptPlace::SourceSet(name.clone()),
            (
                DeletionOwner::Named {
                    name: Some(name), ..
                },
                KeptPlace::AdditionsSet(_),
            ) => KeptPlace::AdditionsSet(name.clone()),
            _ => return None,
        };
        Some(owner.href())
    }

    /// The approve button's label: what approving does.
    pub fn approve_label(&self) -> String {
        format!("Approve and delete {}", files(self.rows.len()))
    }
}

/// The rows of a deletion of `files`, each `(id, name, kept_name, generation, size)`.
fn deletion_rows(
    files: impl IntoIterator<Item = (i64, String, String, u64, u64)>,
    finished: bool,
    reviewed: &[KeptFileReviewed],
    outcomes: &[KeptDeletionOutcome],
) -> Vec<DeletionRow> {
    files
        .into_iter()
        .map(|(id, name, kept_name, generation, size)| {
            let ended = outcomes.iter().find(|o| o.id == id);
            let kind = match ended {
                Some(o) if o.deleted => KeptDeletionKind::Deleted,
                Some(_) => KeptDeletionKind::Left,
                None if finished => KeptDeletionKind::Left,
                None => KeptDeletionKind::Delete,
            };
            DeletionRow {
                id,
                name,
                kept_name,
                generation,
                size,
                reviewed: reviewed
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.sha256.clone()),
                kind,
                detail: ended.and_then(|o| o.detail.clone()),
            }
        })
        .collect()
}

impl From<KeptDeletion> for Deletion {
    fn from(d: KeptDeletion) -> Self {
        let files = d
            .content
            .files
            .into_iter()
            .map(|f| (f.id, f.name, f.kept_name, f.generation, f.file.size));
        let finished = d.finished_at.is_some();
        Deletion {
            id: d.id,
            digest: d.digest,
            approved: d.approval.is_some(),
            finished,
            recovery_error: d.recovery_error,
            rows: deletion_rows(files, finished, &d.content.reviewed, &d.outcomes),
            owner: DeletionOwner::LibraryFolder(d.content.folder),
        }
    }
}

impl From<SourceKeptDeletion> for Deletion {
    fn from(d: SourceKeptDeletion) -> Self {
        let files = d
            .content
            .files
            .into_iter()
            .map(|f| (f.id, f.name, f.kept_name, f.generation, f.file.size));
        let finished = d.finished_at.is_some();
        Deletion {
            id: d.id,
            digest: d.digest,
            approved: d.approval.is_some(),
            finished,
            recovery_error: d.recovery_error,
            rows: deletion_rows(files, finished, &d.content.reviewed, &d.outcomes),
            owner: DeletionOwner::Named {
                id: d.content.source_set,
                name: d.owner,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use stage_select_api::build::{
        Approval, FileIdentity, FileStat, KeepReason, KeptDeletionContent,
    };
    use stage_select_api::repair::SourceKeptDeletionContent;

    use crate::parts::status::Status;

    use super::*;

    fn stat(size: u64) -> FileStat {
        FileStat { size, mtime_ns: 1 }
    }

    fn library_file(id: i64) -> KeptFile {
        KeptFile {
            id,
            folder: 3,
            generation: 2,
            name: "Pong (USA).zip".into(),
            kept_name: "Pong (USA).zip".into(),
            reason: KeepReason::Replacement,
            build: 12,
            built_by: Some(9),
            game: Some("Pong (USA)".into()),
            sha256: Some("ab".into()),
            file: stat(1500),
            identity: FileIdentity { dev: 1, ino: 2 },
            kept_at: "2026-01-02T00:00:00Z".into(),
            directory: false,
        }
    }

    fn source_file(id: i64, plan: Option<i64>, build: Option<i64>) -> SourceKeptFile {
        SourceKeptFile {
            id,
            source_set: 4,
            plan,
            build,
            generation: 1,
            name: "Combat.a26".into(),
            kept_name: "Combat.a26.kept".into(),
            reason: "it matched no entry".into(),
            sha256: Some("cd".into()),
            file: stat(4),
            identity: FileIdentity { dev: 1, ino: 3 },
            kept_at: "2026-01-03T00:00:00Z".into(),
        }
    }

    fn ingest_file(id: i64, plan: Option<i64>) -> IngestKeptFile {
        IngestKeptFile {
            id,
            generation: 5,
            name: "mystery.a26".into(),
            kept_name: "mystery.a26.kept".into(),
            reason: "you discarded it".into(),
            plan,
            sha256: None,
            file: stat(2_000_000),
            kept_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn each_folders_kept_file_is_one_row_linking_to_what_set_it_aside() {
        let library = KeptRow::library(3, "roms/Atari", library_file(1));
        assert_eq!(library.set_aside_by, Some(SetAsideBy::Build(12)));
        assert_eq!(library.reason, "replaced");
        assert_eq!(library.game.as_deref(), Some("Pong (USA)"));
        assert_eq!(
            library.kept_path(),
            ".stage-select/retained/2/Pong (USA).zip"
        );

        let repaired = KeptRow::source("Atari 2600", source_file(1, Some(7), None));
        let built = KeptRow::source("Atari 2600", source_file(2, Some(7), Some(30)));
        let retired = KeptRow::additions("No-Intro/Atari 2600", source_file(3, Some(8), None));
        let discarded = KeptRow::ingest("My Drop", ingest_file(4, Some(6)));
        let found = KeptRow::ingest("My Drop", ingest_file(5, None));
        let by: Vec<Option<(String, String)>> = [&repaired, &built, &retired, &discarded, &found]
            .iter()
            .map(|r| r.set_aside_by.as_ref().map(|b| (b.label(), b.href())))
            .collect();
        assert_eq!(
            by,
            [
                Some((
                    "Repair #7".into(),
                    "/source-sets/Atari%202600?tab=repair&plan=7".into()
                )),
                Some(("Build #30".into(), "/builds/30".into())),
                Some(("Addition #8".into(), "/additions?row=8".into())),
                Some(("Plan #6".into(), "/ingest/My%20Drop?tab=plan&plan=6".into())),
                None,
            ]
        );
        assert_eq!(discarded.reason, "you discarded it");
    }

    #[test]
    fn kept_files_of_every_folder_are_keyed_apart_and_link_to_their_folders_kept_files() {
        let rows = newest_first(vec![
            KeptRow::library(3, "roms/Atari", library_file(1)),
            KeptRow::source("Atari 2600", source_file(1, None, None)),
            KeptRow::ingest("My Drop", ingest_file(1, None)),
            KeptRow::additions("No-Intro/Atari - 2600", source_file(1, Some(2), None)),
        ]);
        let shown: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| (r.key(), r.place.name(), r.place.href()))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "source-set-1".into(),
                    "Atari 2600".into(),
                    "/source-sets/Atari%202600?tab=kept".into()
                ),
                (
                    "additions-set-1".into(),
                    "No-Intro/Atari - 2600".into(),
                    "/additions".into()
                ),
                (
                    "library-folder-1".into(),
                    "roms/Atari".into(),
                    "/libraries/3?tab=kept".into()
                ),
                (
                    "ingest-folder-1".into(),
                    "My Drop".into(),
                    "/ingest/My%20Drop?tab=kept".into()
                ),
            ]
        );
    }

    #[test]
    fn each_folder_lists_plans_and_approves_its_own_deletions() {
        let paths = |p: KeptPlace| {
            assert_eq!(
                format!("{}/approval", p.deletion_path(9)),
                p.approval_path(9)
            );
            (p.files_path(), p.plan_path(), p.approval_path(9))
        };
        assert_eq!(
            paths(KeptPlace::LibraryFolder {
                id: 3,
                path: "roms".into()
            }),
            (
                "/libraries/3/kept-files".into(),
                "/libraries/3/kept-deletions".into(),
                "/kept-deletions/9/approval".into()
            )
        );
        assert_eq!(
            paths(KeptPlace::IngestFolder("My Drop".into())),
            (
                "/ingest-folders/My%20Drop/kept-files".into(),
                "/ingest-folders/My%20Drop/kept-deletions".into(),
                "/ingest-kept-deletions/9/approval".into()
            )
        );
        assert_eq!(
            paths(KeptPlace::SourceSet("Atari 2600".into())),
            (
                "/source-sets/Atari%202600/kept-files".into(),
                "/source-sets/Atari%202600/kept-deletions".into(),
                "/source-kept-deletions/9/approval".into()
            )
        );
        assert_eq!(
            paths(KeptPlace::AdditionsSet("No-Intro/Atari 2600".into())),
            (
                "/additions-set/kept-files?system=No-Intro%2FAtari%202600".into(),
                "/additions-set/kept-deletions?system=No-Intro%2FAtari%202600".into(),
                "/additions-kept-deletions/9/approval".into()
            )
        );
    }

    #[test]
    fn the_view_leads_with_how_many_kept_files_and_their_total_size() {
        let one = KeptRow::source("A", source_file(1, None, None));
        let big = KeptRow::ingest("B", ingest_file(2, None));
        assert_eq!(summary(&[]), "No kept files");
        assert_eq!(summary(std::slice::from_ref(&one)), "1 kept file, 4 bytes");
        assert_eq!(summary(&[one, big]), "2 kept files, 2.0 MB");
    }

    #[test]
    fn a_deletion_names_the_checked_files_or_every_file() {
        let rows = [
            KeptRow::source("A", source_file(4, None, None)),
            KeptRow::source("A", source_file(2, None, None)),
        ];
        assert_eq!(
            deleting(&rows),
            PlanKeptDeletion {
                ids: vec![4, 2],
                all: false
            }
        );
        assert_eq!(
            deleting_all(),
            PlanKeptDeletion {
                ids: vec![],
                all: true
            }
        );
    }

    fn source_deletion(approved: bool, outcomes: Vec<KeptDeletionOutcome>) -> SourceKeptDeletion {
        SourceKeptDeletion {
            id: 5,
            owner: Some("Atari 2600".into()),
            digest: "d1".into(),
            created_at: "2026-01-05T00:00:00Z".into(),
            content: SourceKeptDeletionContent {
                source_set: 4,
                files: vec![source_file(1, None, None), source_file(2, None, None)],
                reviewed: vec![KeptFileReviewed {
                    id: 1,
                    ctime_ns: 3,
                    sha256: "cd".into(),
                }],
            },
            approval: approved.then(|| Approval {
                digest: "d1".into(),
                actor: "you".into(),
                at: "2026-01-05T00:00:01Z".into(),
            }),
            finished_at: (!outcomes.is_empty()).then(|| "2026-01-05T00:00:02Z".into()),
            outcomes,
            recovery_error: None,
        }
    }

    fn outcome(id: i64, deleted: bool, detail: Option<&str>) -> KeptDeletionOutcome {
        KeptDeletionOutcome {
            id,
            kept_name: "Combat.a26.kept".into(),
            generation: 1,
            deleted,
            detail: detail.map(Into::into),
        }
    }

    #[test]
    fn a_deletion_says_what_it_will_delete_then_what_it_did_failures_first() {
        let planned = Deletion::from(source_deletion(false, vec![]));
        assert_eq!(planned.summary(), "Deletes 2 kept files, 8 bytes.");
        assert_eq!(planned.approve_label(), "Approve and delete 2 kept files");
        assert!(
            planned
                .rows
                .iter()
                .all(|r| r.kind == KeptDeletionKind::Delete)
        );
        assert_eq!(planned.rows[0].reviewed.as_deref(), Some("cd"));
        assert_eq!(planned.rows[1].reviewed, None);

        let done = Deletion::from(source_deletion(
            true,
            vec![
                outcome(1, true, None),
                outcome(2, false, Some("it changed since you reviewed its deletion")),
            ],
        ));
        assert!(done.approved && done.finished);
        assert_eq!(
            done.summary(),
            "Deleted 1 of 2 kept files. 1 was left kept."
        );
        let kinds: Vec<_> = done.rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [KeptDeletionKind::Deleted, KeptDeletionKind::Left]);
        assert_eq!(
            done.rows[1].detail.as_deref(),
            Some("it changed since you reviewed its deletion")
        );
        assert_eq!(
            KeptDeletionKind::ALL[0],
            KeptDeletionKind::Left,
            "a file left kept leads"
        );

        let every = Deletion::from(source_deletion(
            true,
            vec![outcome(1, true, None), outcome(2, true, None)],
        ));
        assert_eq!(every.summary(), "Deleted 2 kept files.");
    }

    #[test]
    fn a_deleted_file_is_done_and_one_left_kept_was_not_done_while_one_still_to_delete_has_no_outcome()
     {
        let planned = Deletion::from(source_deletion(true, vec![]));
        assert_eq!(planned.rows[0].outcome(), None);
        let done = Deletion::from(source_deletion(
            true,
            vec![
                outcome(1, true, None),
                outcome(2, false, Some("it changed since you reviewed its deletion")),
            ],
        ));
        let outcomes: Vec<_> = done.rows.iter().map(DeletionRow::outcome).collect();
        assert_eq!(
            outcomes,
            [Some(ItemOutcome::Done), Some(ItemOutcome::NotDone)]
        );
    }

    #[test]
    fn a_library_folders_deletion_is_the_same_plan() {
        let deletion = KeptDeletion {
            id: 2,
            digest: "d2".into(),
            created_at: "2026-01-05T00:00:00Z".into(),
            content: KeptDeletionContent {
                folder: 3,
                files: vec![library_file(1)],
                reviewed: vec![],
            },
            approval: None,
            finished_at: None,
            outcomes: vec![],
            recovery_error: Some("its folder is missing".into()),
        };
        let shown = Deletion::from(deletion);
        assert_eq!(shown.summary(), "Deletes 1 kept file, 1.5 KB.");
        assert_eq!(shown.approve_label(), "Approve and delete 1 kept file");
        assert_eq!(
            shown.recovery_error.as_deref(),
            Some("its folder is missing")
        );
        assert_eq!(
            shown.rows[0].kept_path(),
            ".stage-select/retained/2/Pong (USA).zip"
        );
    }

    #[test]
    fn a_deletion_is_approved_only_under_the_folder_whose_kept_files_it_deletes() {
        let library = Deletion::from(KeptDeletion {
            id: 2,
            digest: "d2".into(),
            created_at: "2026-01-05T00:00:00Z".into(),
            content: KeptDeletionContent {
                folder: 3,
                files: vec![library_file(1)],
                reviewed: vec![],
            },
            approval: None,
            finished_at: None,
            outcomes: vec![],
            recovery_error: None,
        });
        let folder = |id| KeptPlace::LibraryFolder {
            id,
            path: "play/atari-2600".into(),
        };
        assert!(library.belongs_to(&folder(3)));
        assert_eq!(
            library.owner_id(),
            3,
            "approval names the folder it deletes from"
        );
        assert!(
            !library.belongs_to(&folder(9)),
            "another folder's deletion, even of the same path"
        );
        assert_eq!(
            library.owner_href(&folder(9)).as_deref(),
            Some("/libraries/3?tab=kept")
        );

        let set = Deletion::from(source_deletion(false, vec![]));
        let named = |name: &str| KeptPlace::SourceSet(name.into());
        assert!(set.belongs_to(&named("Atari 2600")));
        assert_eq!(set.owner_id(), 4);
        assert!(!set.belongs_to(&named("Atari 7800")));
        assert_eq!(
            set.owner_href(&named("Atari 7800")).as_deref(),
            Some("/source-sets/Atari%202600?tab=kept")
        );

        let mut forgotten = source_deletion(false, vec![]);
        forgotten.owner = None;
        let forgotten = Deletion::from(forgotten);
        assert!(
            !forgotten.belongs_to(&named("Atari 2600")),
            "a deletion of an unregistered set is no registered set's"
        );
        assert_eq!(forgotten.owner_href(&named("Atari 2600")), None);
    }

    #[test]
    fn an_approved_deletion_still_under_way_says_so() {
        let deletion = Deletion::from(source_deletion(true, vec![]));
        assert_eq!(deletion.summary(), "Deleting 2 kept files.");
        assert!(
            deletion
                .rows
                .iter()
                .all(|r| r.kind == KeptDeletionKind::Delete)
        );
    }
}
