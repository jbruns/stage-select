//! The store (ADR 0003): one SQLite database in a store directory, with DAT blobs under `dats/<sha256>`.

mod additions;
mod audits;
mod beneath;
mod blobs;
mod builds;
pub mod calibration;
mod carry;
pub mod ceilings;
mod checks;
mod coverage;
mod cue_packs;
mod dat_change;
mod dat_deletion;
mod dats;
mod exports;
pub mod fetch;
mod folder_deletion;
mod folder_dir;
mod folder_moves;
mod folders;
mod ingest;
mod jobs;
mod kept;
mod lifecycle;
mod made_folders;
mod migrations;
pub mod observe;
mod permissions;
mod placements;
pub mod plan_meter;
mod plans;
mod profiles;
mod removed;
mod repairs;
pub mod romm;
mod romm_additions;
pub mod roots;
mod sbi_packs;
mod selections;
mod sets;
pub mod settings;
mod stale;
mod storage;
mod support_files;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

pub use audits::StoredAudit;
pub use coverage::RegionReader;
pub use cue_packs::{CUE_PACK_LIMIT, CUE_PACKS};
pub use exports::{Export, FixdatScope};
pub use ingest::OwedAudits;
pub use jobs::Interruption;
pub use plan_meter::PlanMeter;
pub use sbi_packs::SBI_PACKS;
pub use selections::SelectionInput;
pub use stage_select_api::{
    AuditSummary, DatRevisionRecord, DeletedDatRevision, DeletionBlocker, DeletionBlockerKind,
    ImportOutcome, Job, JobState, LastVerified, SourceSetRecord, Workers,
};
pub use support_files::{SUPPORT_FILE_LIMIT, SUPPORT_FILES};

/// The database's file name in the store directory.
pub const DATABASE: &str = "stage-select.sqlite";
/// The directory, in the store directory, that holds DAT blobs.
pub const DATS: &str = "dats";
/// The directory, in the store directory, that holds the DAT-exact cue sheets of CDs placed as CHD, which a CHD doesn't
/// hold, as content-addressed blobs `cues/<sha256>` (ADR 0023).
pub const CUES: &str = "cues";
/// The directory, in the store directory, that holds the backups taken before migrations.
pub const BACKUPS: &str = "backups";
/// The directory, in the store directory, that holds per-run scratch folders, such as a Selection's Retool run.
/// Each run deletes its own folder.
pub const TMP: &str = "tmp";
/// The file, in the store directory, that an open store holds an exclusive lock on.
pub const LOCK: &str = "lock";

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle reader connections kept for reuse.
const READER_POOL: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Io(String),
    /// The request cannot be carried out as asked.
    #[error("{0}")]
    Refused(String),
    /// What the request names does not exist.
    #[error("{0}")]
    NotFound(String),
    /// The request was made against a state that has since changed, such as an edit of an older Profile version.
    #[error("{0}")]
    Conflict(String),
    /// A Source Set already has a Job of this kind Queued or Running.
    #[error("source set {source_set} already has an active {}: job {job}", .kind.as_str())]
    JobActive {
        source_set: String,
        job: i64,
        kind: stage_select_api::JobKind,
    },
    /// A DAT Revision can't be deleted while something still depends on it: `message` says what, for you to read.
    #[error("{message}")]
    RevisionInUse {
        message: String,
        blockers: Vec<stage_select_api::DeletionBlocker>,
    },
    #[error(transparent)]
    Audit(#[from] stage_select_engine::Error),
    /// Games' regions can't be read: no reader was set by [`Store::set_regions`], as when the Selection engine's
    /// clone lists aren't available. What needs a Coverage, or regions, can't be worked out until they are.
    #[error("{0}")]
    RegionsUnavailable(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

pub(crate) fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// An open store: one writer connection that serialises writes, and a pool of readers. Only one process at a
/// time may open a store, so the writer is the only one.
pub struct Store {
    dir: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    /// Holds the exclusive lock on `lock` until the store is dropped.
    _lock: Lock,
    fault: Mutex<Option<FaultHook>>,
    /// Held while Additions are published, so that only one caller settles them at a time.
    publishing: Mutex<()>,
    /// Held while an Additions Set's folder is created.
    set_dirs: Mutex<()>,
    /// Held while a plan retries the cleanup of a Library Folder's stopped Builds, so that no two retry at once.
    recovering: Mutex<()>,
    /// Held while a registration is made or Unregistered, its marker written or removed, or a Library Folder's move
    /// reserves its destination, so that none of them interleave. Taken before the writer, never inside a write.
    lifecycle: Mutex<()>,
    /// What receives each occurrence as its write commits.
    observer: std::sync::OnceLock<observe::Observer>,
    /// The Ceilings in force, and the pools and gates every Job shares to keep to them: worked out from the
    /// Roots and settings on first use, and again as they or a Calibration change them.
    ceilings: ceilings::Live,
    /// The pace in the settings, which running Jobs read at each file.
    pace: std::sync::RwLock<stage_select_api::settings::Pace>,
    /// The largest 7z dictionary in the settings, in MiB, which each Job reads as it starts.
    max_dictionary_mib: std::sync::atomic::AtomicU32,
    /// Every registered Root, replaced whole as one changes.
    roots: std::sync::RwLock<std::sync::Arc<roots::Roots>>,
    /// The chdman the Service was told to run, if any; else `STAGE_SELECT_CHDMAN`, else `chdman` on the `PATH`.
    chdman: std::sync::RwLock<Option<PathBuf>>,
    /// Held across each settings update's commit and publication, so they happen in the same order.
    settings_update: std::sync::Mutex<()>,
    /// Reads a Game's regions from its name, for Coverage; set by [`Store::set_regions`].
    regions: std::sync::OnceLock<coverage::RegionReader>,
    /// Counts the bytes and files every Job reads and writes, for each Job, each Root and in total.
    meter: stage_select_engine::meter::Meter,
    /// Whether a repair carries a Placed Game's file between folders by a copy, as across filesystems, though one
    /// rename would do: set by [`Store::copy_across_filesystems`].
    copy_across: std::sync::atomic::AtomicBool,
    /// The bytes free a test says every Root has, in place of what its filesystem says: set by
    /// [`Store::pretend_free_space`]. `u64::MAX` says none.
    free_space: std::sync::atomic::AtomicU64,
    /// How many of this store's syncs of the RomM Additions collection wait their turn, for tests.
    romm_syncs_waiting: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// What the latest check of each folder found of its permissions, since the service started.
    permissions: permissions::Checked,
}

// The seams let a caller make an entry read as another user's, so no optimized build, which every shipped binary is,
// may have them: `cargo build --release --all-features`, or a dependent that turns the feature on, fails here. The
// store's tests build with debug assertions, as `cargo test` and `cargo nextest run` do by default.
#[cfg(all(feature = "test-seams", not(debug_assertions)))]
compile_error!(
    "the store's test-seams feature is for its own tests only, and never builds with --release; build without it"
);

/// Makes the entry at `path` read, wherever Stage Select checks who owns one of its own entries, as owned by the user
/// and group `owner`, or, with `None`, by whoever its filesystem says: an owner other than the test's own user, or a
/// group it isn't in, which an unprivileged test can't otherwise arrange. It holds for every store in the process, by
/// the entry's identity. Only with the `test-seams` feature, which only the store's own tests enable.
#[cfg(feature = "test-seams")]
#[doc(hidden)]
pub fn pretend_owner(path: &Path, owner: Option<(u32, u32)>) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let st = std::fs::symlink_metadata(path)?;
    folder_dir::pretend_owner(
        folder_dir::Identity {
            dev: st.dev(),
            ino: st.ino(),
        },
        owner,
    );
    Ok(())
}

/// Makes the folder at `path` a mount of its own while `on`: a rename into it from outside it, or out of it, fails
/// with `EXDEV`, as one between two bind mounts of a filesystem does though both report its device. Only with the
/// `test-seams` feature.
#[cfg(feature = "test-seams")]
#[doc(hidden)]
pub fn pretend_mount(path: &Path, on: bool) {
    folder_dir::pretend_mount(path, on);
}

/// What a test runs at each [`Fault`] point.
#[doc(hidden)]
pub type FaultHook = std::sync::Arc<dyn Fn(Fault) + Send + Sync>;

/// A point at which a Build stops as if the service were killed there, for tests of its recovery.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// After the file of the item with this sequence number is written and verified in the staging folder, before the
    /// next file of its unit is.
    Staged(i64),
    /// After the publish intent of the item with this sequence number is committed, before its file is renamed.
    BeforePublish(i64),
    /// After the file of the item with this sequence number is renamed into place, before either folder is synced.
    PublishedUnsynced(i64),
    /// After the file of the item with this sequence number is renamed into place and the folder it went in is
    /// synced, a disc layout folder included: by the Build, or by its recovery.
    ParentSynced(i64),
    /// After the file of the item with this sequence number is renamed into place and both folders are synced, before
    /// the store records it.
    AfterPublish(i64),
    /// After the intent to keep aside the file of the item with this sequence number is committed, before it is
    /// moved.
    BeforeKeep(i64),
    /// Before the file the item with this sequence number keeps aside is read whole, to be hashed: not reached when
    /// the hash its Build read while repacking it is trusted.
    KeepRead(i64),
    /// After the file of the item with this sequence number is moved into the retention area, before the store
    /// records it kept.
    AfterKeep(i64),
    /// After the file of the item with this sequence number is moved into the retention area, before either folder is
    /// synced.
    KeptUnsynced(i64),
    /// After the file of the removal with this sequence number is kept aside to clear the way for a file of its
    /// release's other disc layout, before the disc layout folder it emptied, if any, is removed.
    Cleared(i64),
    /// After the intent to move the entry of a cleanup's item with this sequence number is committed, before it is
    /// renamed.
    BeforeMove(i64),
    /// After the entry of a cleanup's item with this sequence number is renamed, before either folder is synced.
    AfterMove(i64),
    /// After the entry of a cleanup's item with this sequence number is renamed and both folders are synced, before
    /// the store records it moved.
    MoveSynced(i64),
    /// After a folder Stage Select is creating in a Library Folder is made under its temporary name, before it is
    /// opened and checked.
    MadeDir,
    /// After a folder Stage Select is creating in a Library Folder is made and recorded under its temporary name,
    /// before it is renamed into place.
    BeforePublishDir,
    /// As [`Fault::MadeDir`], for a Build's staging directory.
    MadeStaging,
    /// After a folder Stage Select is creating in a Library Folder is renamed into place, before its parent is
    /// synced and its identity recorded under its name.
    PublishedDir,
    /// As [`Fault::BeforePublishDir`], for a Build's staging directory.
    BeforePublishStaging,
    /// After the intent to delete the kept file with this id is committed and it is found to be the kept file, before
    /// it is moved into the private folder it is deleted from.
    BeforeDeleteMove(i64),
    /// After the kept file with this id is renamed into the private folder it is deleted from, before the ctime read
    /// through the descriptor held across that rename is recorded.
    DeleteMoveUnrecorded(i64),
    /// After the kept file with this id is moved into the private folder it is deleted from, before it is checked.
    AfterDeleteMove(i64),
    /// When recovery finds something where the kept file with this id was moved to be deleted, before it checks it.
    DeletingFound(i64),
    /// After what was moved aside to delete the kept file with this id, not being that file, is recorded, before it is
    /// moved back.
    Quarantined(i64),
    /// After what was held aside for the kept file with this id is moved back, before either folder is synced.
    MovedBack(i64),
    /// After what was held aside for the kept file with this id is moved back and both folders are synced, before the
    /// store records it kept again.
    MoveBackSynced(i64),
    /// After the kept file with this id is deleted, before the store records it deleted.
    AfterUnlink(i64),
    /// After a cleanup is planned from what its folder holds, before the plan and the folder's check are stored.
    Scanned,
    /// After a placing item with this sequence number commits its intent to rename its Source Set file into the
    /// Library Folder, before the rename.
    PlaceRenaming(i64),
    /// After the Source Set file of a placing item with this sequence number is renamed into the Library Folder,
    /// before either folder is synced or anything is recorded.
    PlaceMoved(i64),
    /// After the Source Set file of a placing item with this sequence number is copied into the Library Folder and
    /// read back, before its Game is recorded placed.
    PlaceCopied(i64),
    /// After the Game of a placing item with this sequence number is recorded placed, before its Source Set files are
    /// deleted.
    PlaceRecorded(i64),
    /// After the cue sheet of a placing item with this sequence number that places a CD as CHD is kept in the store,
    /// written and synced, before the item records it.
    PlaceCueKept(i64),
    /// After a Source Set file of a placing item with this sequence number is set aside to delete and read back,
    /// before the placed file is checked a last time and it is deleted.
    PlaceChecking(i64),
    /// After a Source Set file of a placing item with this sequence number is deleted, or kept, before the store
    /// records which.
    PlaceDisposed(i64),
    /// After a Source Set file of a placing item with this sequence number is moved into the Source Set's kept
    /// files, before either folder is synced or anything is recorded.
    PlaceKeptAside(i64),
    /// After a returning item with this sequence number commits its intent to rename its placed files home, before
    /// each rename.
    ReturnRenaming(i64),
    /// After a placed file of a returning item with this sequence number is renamed home and its change time read
    /// and recorded, before either folder is synced.
    ReturnMoved(i64),
    /// After a placed file of a returning item with this sequence number, renamed home, is put back where it was
    /// placed, before it is verified and its new stamp recorded.
    ReturnPutBack(i64),
    /// After a placed CHD of a returning item with this sequence number is copied into the store's scratch directory
    /// and read whole, before chdman extracts it.
    ReturnChdCopied(i64),
    /// After a file a returning item with this sequence number writes home is staged and verified, before it is put in
    /// place.
    ReturnStaged(i64),
    /// After a file a returning item with this sequence number writes home is put in place, before its folder is
    /// synced.
    ReturnPublished(i64),
    /// After a placed file of a returning item with this sequence number is set aside to delete, before the next is.
    ReturnSetAside(i64),
    /// After every placed file of a returning item with this sequence number that goes is set aside to delete, before
    /// the last check of the files put home and of those set aside.
    ReturnChecking(i64),
    /// After the release of a returning item with this sequence number is recorded home, before its placed files are
    /// deleted.
    ReturnRecorded(i64),
    /// After a placed file of a returning item with this sequence number is deleted, or put back, before the store
    /// records which.
    ReturnDisposed(i64),
    /// After the store commits the folder's next generation, before the marker advances to it.
    AfterFinalize,
    /// After the marker advances, before the store records the Build finished.
    AfterMarker,
    /// After a Build that failed is recorded failed, with its Job's error, before the recovery of its folder it runs as
    /// it fails.
    BuildFailed,
    /// Before a scan reads an item of an Ingest Folder to hash it: not reached when the hashes it read before are
    /// reused.
    IngestRead,
    /// After an entry of an Ingest Folder is checked to be the file a plan moves, just before it is renamed there.
    IngestRenaming,
    /// After an entry of an Ingest Folder is renamed to where a plan moves it, before either folder is synced.
    IngestMoved,
    /// After the verified copy or repack of an entry of an Ingest Folder is written, before it is renamed into place.
    IngestStaged,
    /// After the staged copy or repack of an entry of an Ingest Folder is renamed into place and its folder synced,
    /// before the published file's identity is checked.
    IngestRenamed,
    /// After the copy or repack of an entry of an Ingest Folder is renamed into place, before its original is
    /// disposed of.
    IngestPublished,
    /// After the original of a published copy is moved into the private `.deleting` area, before either folder is
    /// synced.
    IngestIsolated,
    /// After a rename puts back what was moved in place of an Ingest Folder's entry, before either folder is synced.
    IngestRestored,
    /// After an entry is renamed aside into an Ingest Folder's kept files, before either folder is synced.
    IngestSetAside,
    /// After a move records that what it set aside to delete is kept instead, before it is moved into the kept files.
    IngestKeeping,
    /// Before a move reads the entry it copies or repacks whole, hashing it as it goes.
    IngestReadWhole,
    /// After the incremental Audits a DAT import from an Ingest Folder owes are recorded, before the DAT is imported.
    IngestDatOwed,
    /// After a repair plan is made, before it is recorded.
    RepairPlanned,
    /// After a repair's step with this sequence number writes and checks its staged file, before the store records it.
    RepairStaged(u32),
    /// After a repair's step with this sequence number moves its file, before either folder is synced.
    RepairMoved(u32),
    /// After a repair's step with this sequence number is done on disk and synced, before the store records it done.
    RepairStepDone(u32),
    /// After the original a repair's step with this sequence number carried across filesystems is moved into its
    /// folder's private `.deleting` area to be deleted, before its change time there is recorded.
    RepairCarryMoved(u32),
    /// After the original a repair's step with this sequence number carried across filesystems is set aside to delete
    /// and read back, before its published copy is checked a last time and the original deleted.
    RepairCarryChecking(u32),
    /// After a repair's outcome, the Audit it owes and its Job's end are recorded, before that Audit is queued.
    RepairFinished,
    /// After a Source Set's first marker is written and synced under its recorded temporary name, before it is
    /// renamed into place.
    /// After a Source Set's first marker's temporary file is opened and checked, before it is written.
    RepairMarkerChecked,
    RepairMarkerWritten,
    /// After a Source Set's first marker is renamed into place, before its folder is synced and the marker recorded.
    RepairMarkerPublished,
    /// After a repair's cancellation and every step it skips are recorded, before the files its skipped Builds
    /// staged are deleted.
    RepairCancelled,
    /// After a file the repair step with this sequence number staged is moved into the private folder it is deleted
    /// from, before either folder is synced.
    RepairDiscarding(u32),
    /// After a repair plan's approval is checked against the Source Set, before the transaction that reserves it.
    RepairApproving,
    /// After an approved repair is found unable to start, before the transaction that fails its steps, finishes it
    /// and ends its Job Failed.
    RepairAbandoning,
    /// After an Ingest Folder's registration is read or recorded, before its marker is written.
    IngestMarking,
    /// After a Library Folder being registered is created or found empty, before the transaction that records it.
    FolderRegistering,
    /// Before a change to what is registered waits for another under way to finish.
    LifecycleWaiting,
    /// After a Build that removed games to delete succeeds, before the files it kept aside of them are deleted.
    RemovedKept,
    /// After a Build that deletes its Library Folder succeeds, before the folder's kept files are deleted.
    FolderDeleting,
    /// After deleting a Library Folder records it is removing the folder, before anything of it is removed.
    FolderRemoving,
    /// After an edit of a Root takes the lifecycle lock, before it checks and records the edit.
    RootEditing,
    /// Before a Root or Source Set folder registered on request is created, once its path is checked.
    CreatingFolder,
    /// After a Root or Source Set folder registered on request is created, before the registration is checked
    /// further and recorded.
    FolderCreated,
    /// After the kept file of a Source Set with this id is moved into the private folder it is deleted from, before
    /// either folder is synced.
    SourceKeptMoved(i64),
    /// After both folders a deletion of the kept file of a Source Set with this id touches are synced, before it is
    /// checked, deleted or recorded.
    SourceKeptSynced(i64),
    /// After both folders are synced when a stopped publish step with this sequence number, whose file was to be
    /// kept aside, finds it in neither place, before anything found there is recorded or the step ends.
    RepairFoundSynced(u32),
    /// After recovery syncs both folders a stopped deletion of the kept file with this id touches, before it is
    /// checked, deleted or recorded.
    KeptDeletionSynced(i64),
    /// After the converted file of a repair's Delete step with this sequence number is verified, before the deletion
    /// of its original is recorded.
    ConversionVerified(u32),
    /// After a Build keeps aside the original of the conversion of the item with this sequence number, before it
    /// verifies the converted file to delete the original.
    ConversionKept(i64),
    /// Before a Build reads a file of its Source Set to write the ROMs it holds for the disc of the item with this
    /// sequence number, which it makes a CHD of.
    ChdSourceRead(i64),
    /// A CHD that was checked has been copied into staging, before the copy is hashed again.
    ChdCopied(i64),
    /// After an Additions Set's folder is made and recorded under its temporary name, before its marker is written.
    AdditionSetMade,
    /// After an Additions Set's folder is renamed into place, before the Root is synced and the folder recorded.
    AdditionSetPublished,
    /// After an Addition's file is written and checked under its staged name, before it is renamed into place.
    AdditionStaged,
    /// After an Addition's file is renamed into place, before its folder is synced and it is recorded active.
    AdditionRenamed,
    /// After a retiring Addition's file is renamed into its Additions Set's kept files, before either folder
    /// is synced or anything is recorded.
    AdditionRetireMoved,
    /// After a restoring Addition's file is renamed back from its Additions Set's kept files, before either
    /// folder is synced or anything is recorded.
    AdditionRestoreMoved,
    /// After approving an ingest plan records the intent to add one of its items as an Addition, before the
    /// rest of its items, what became of them and its Additions Job are recorded.
    IngestAdditionRecorded,
    /// After a restoring Addition's file, found replaced once renamed back into its Additions Set, has what
    /// replaced it renamed back into the kept files, before either folder is synced or anything is recorded.
    AdditionRestoreReturned,
}

/// The store directory's exclusive lock, released when dropped. A process forked meanwhile shares the open lock file
/// until it runs its program, and closing the store's copy alone would leave the lock held by the child's.
struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Takes the store directory's exclusive lock, failing at once if another process holds it.
fn lock(dir: &Path) -> Result<Lock> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io_error(&path, e))?;
    match file.try_lock() {
        Ok(()) => Ok(Lock(file)),
        Err(TryLockError::WouldBlock) => Err(Error::Refused(format!(
            "{}: the store is in use by another Stage Select process",
            dir.display()
        ))),
        Err(TryLockError::Error(e)) => Err(io_error(&path, e)),
    }
}

/// Settings every connection gets (ADR 0003).
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // SQLite's temporary files stay in memory, so a read-only root filesystem without a writable /tmp still
    // starts: recovering an interrupted Job's Audit needs them (#66).
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

impl Store {
    /// Runs `program` as chdman from now on, or, when `None`, the program `STAGE_SELECT_CHDMAN` names, else `chdman`
    /// on the `PATH`.
    pub fn use_chdman(&self, program: Option<PathBuf>) {
        *self.chdman.write().unwrap_or_else(|e| e.into_inner()) = program;
    }

    /// The chdman to run, found as [`Store::use_chdman`] says.
    pub(crate) fn chdman(&self) -> std::result::Result<PathBuf, String> {
        stage_select_engine::chd::locate(
            self.chdman
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref(),
        )
    }

    /// The bounds a 7z decodes within: the store's one gate, sized by the pace's share of the archive workers Ceiling,
    /// and the dictionary limit in the settings; its bytes read are counted to `read`.
    pub(crate) fn decoding<'a>(
        &'a self,
        read: &'a stage_select_engine::meter::Tally,
    ) -> stage_select_engine::sevenz::Decoding<'a> {
        stage_select_engine::sevenz::Decoding {
            gate: self.archive_gate(),
            max_dictionary_mib: self
                .max_dictionary_mib
                .load(std::sync::atomic::Ordering::Relaxed),
            read,
        }
    }

    /// The meter that counts what every Job reads and writes.
    pub fn meter(&self) -> &stage_select_engine::meter::Meter {
        &self.meter
    }

    /// Opens the store in `dir`, creating it if needed, migrating it, and collecting blobs that have no row.
    pub fn open(dir: &Path) -> Result<Store> {
        Store::open_with(dir, migrations::MIGRATIONS)
    }

    fn open_with(dir: &Path, migrations: &[&str]) -> Result<Store> {
        let dats = dir.join(DATS);
        std::fs::create_dir_all(&dats).map_err(|e| io_error(&dats, e))?;
        let lock = lock(dir)?;
        let mut writer = Connection::open(dir.join(DATABASE))?;
        // A database with a schema that this release didn't stamp is refused before any migration runs.
        let version: i64 = writer.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let id: i32 = writer.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if let Err(refused) = migrations::check_stamp(version, id) {
            writer.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
            return Err(refused);
        }
        configure(&writer)?;
        let mode: String = writer.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Io(format!(
                "{}: cannot use WAL mode (got {mode})",
                dir.display()
            )));
        }
        migrations::migrate(&mut writer, dir, migrations)?;
        let store = Store {
            dir: dir.to_path_buf(),
            writer: Mutex::new(writer),
            readers: Mutex::new(Vec::new()),
            _lock: lock,
            fault: Mutex::new(None),
            publishing: Mutex::new(()),
            set_dirs: Mutex::new(()),
            recovering: Mutex::new(()),
            lifecycle: Mutex::new(()),
            observer: std::sync::OnceLock::new(),
            ceilings: ceilings::Live::default(),
            pace: std::sync::RwLock::default(),
            max_dictionary_mib: std::sync::atomic::AtomicU32::new(
                stage_select_api::settings::DEFAULT_MAX_DICTIONARY_MIB,
            ),
            roots: std::sync::RwLock::default(),
            chdman: std::sync::RwLock::default(),
            settings_update: std::sync::Mutex::default(),
            permissions: permissions::Checked::default(),
            regions: std::sync::OnceLock::new(),
            meter: stage_select_engine::meter::Meter::default(),
            copy_across: std::sync::atomic::AtomicBool::new(false),
            free_space: std::sync::atomic::AtomicU64::new(u64::MAX),
            romm_syncs_waiting: std::sync::Arc::default(),
        };
        store.meter.share_limits(store.ceilings.readers.clone());
        let settings = store.settings()?;
        *store.pace.write().unwrap_or_else(|e| e.into_inner()) = settings.pace;
        store.max_dictionary_mib.store(
            settings.archive.max_dictionary_mib,
            std::sync::atomic::Ordering::Relaxed,
        );
        store.reload_roots()?;
        // Retention is not run here: it runs after an import or an Audit, so a blob restored by one command is
        // still there for the Audit the next command runs.
        store.discard_unfinished_audits()?;
        store.collect_garbage()?;
        Ok(store)
    }

    /// Makes a repair carry a Placed Game's file between its Library Folder and its Source Set's folder by a copy,
    /// as it does across filesystems, though they share one: what a test can't otherwise arrange.
    #[doc(hidden)]
    pub fn copy_across_filesystems(&self, on: bool) {
        self.copy_across
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Makes the store read every Root as having `bytes` free, or, with `None`, what its filesystem says: what a
    /// test can't otherwise arrange.
    #[doc(hidden)]
    pub fn pretend_free_space(&self, bytes: Option<u64>) {
        self.free_space.store(
            bytes.unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    /// The bytes free to Stage Select in the filesystem holding the folder `path` of Root `root`.
    fn free_space(&self, root: &str, path: &str) -> Result<u64> {
        if let Some(pretended) = self.pretended_free() {
            return Ok(pretended);
        }
        let resolved = sets::resolve(&self.roots(), root, path)?;
        let st = rustix::fs::statvfs(&resolved.path)
            .map_err(|e| Error::Io(format!("{}: {e}", resolved.path.display())))?;
        Ok(st.f_bavail.saturating_mul(st.f_frsize))
    }

    /// The bytes free to Stage Select in the filesystem holding the open folder `dir`, or what a test says.
    fn free_in(&self, dir: impl std::os::fd::AsFd) -> Result<u64> {
        if let Some(pretended) = self.pretended_free() {
            return Ok(pretended);
        }
        let st = rustix::fs::fstatvfs(dir).map_err(|e| Error::Io(e.to_string()))?;
        Ok(st.f_bavail.saturating_mul(st.f_frsize))
    }

    /// The free space a test says every filesystem has, if it has said.
    fn pretended_free(&self) -> Option<u64> {
        let pretended = self.free_space.load(std::sync::atomic::Ordering::SeqCst);
        (pretended != u64::MAX).then_some(pretended)
    }

    /// Whether `e`, from a rename between folders, says they are on different filesystems; or a test has said to
    /// treat them as if they were.
    fn across_filesystems(&self, e: Option<&std::io::Error>) -> bool {
        self.copy_across.load(std::sync::atomic::Ordering::SeqCst)
            // By its kind: a rename's error names the path, which keeps the kind and drops the code.
            || e.is_some_and(|e| e.kind() == std::io::ErrorKind::CrossesDevices)
    }

    #[doc(hidden)]
    pub fn inject_fault(&self, fault: Option<Fault>) {
        self.hook_faults(fault.map(|fault| {
            std::sync::Arc::new(move |at: Fault| {
                if at == fault {
                    panic!("injected fault: {fault:?}");
                }
            }) as FaultHook
        }));
    }

    /// Runs `hook` at every [`Fault`] point.
    #[doc(hidden)]
    pub fn hook_faults(&self, hook: Option<FaultHook>) {
        *self.fault.lock().unwrap() = hook;
    }

    /// Serializes changes to what is registered: see [`Store::lifecycle`].
    pub(crate) fn lifecycle_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.lifecycle.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                self.fault_point(Fault::LifecycleWaiting);
                self.lifecycle.lock().unwrap_or_else(|e| e.into_inner())
            }
        }
    }

    /// Runs the hook a test set, which may stop the thread here as a kill would.
    fn fault_point(&self, fault: Fault) {
        let hook = self.fault.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(fault);
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the blob of the DAT Revision with this SHA-256 is kept.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(DATS).join(sha256)
    }

    /// Where the cue sheet blob whose SHA-256 is `sha256` is kept.
    pub fn cue_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(CUES).join(sha256)
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = self.readers.lock().unwrap().pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => {
                let conn = Connection::open_with_flags(
                    self.dir.join(DATABASE),
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                configure(&conn)?;
                conn
            }
        };
        let out = f(&conn);
        let mut readers = self.readers.lock().unwrap();
        if readers.len() < READER_POOL {
            readers.push(conn);
        }
        out
    }

    /// Runs `f` in one write transaction on the writer connection.
    fn write<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
    }

    /// Runs `f` while holding the writer, so that blob files and the rows that hold them change together.
    /// What `f` notes is handed to the observer once it returns successfully and the writer is released.
    fn with_writer<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        // A Job holding a reader permit may wait for the writer, so nothing holding it waits for one.
        let _no_waits = stage_select_engine::meter::NoReaderWaits::new();
        let mut conn = self.writer.lock().unwrap();
        observe::begin(self.observer.get().is_some());
        let out = f(&mut conn);
        let noted = observe::end();
        drop(conn);
        if out.is_ok()
            && let Some(observer) = self.observer.get()
        {
            for o in noted {
                observer(o);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pragma(conn: &Connection, name: &str) -> String {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| {
            r.get::<_, rusqlite::types::Value>(0)
        })
        .map(|v| format!("{v:?}"))
        .unwrap()
    }

    #[test]
    fn every_connection_uses_wal_full_sync_foreign_keys_a_busy_timeout_and_memory_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let writer = store.writer.lock().unwrap();
        assert_eq!(pragma(&writer, "journal_mode"), r#"Text("wal")"#);
        let reader = store.read(|c| Ok(pragma(c, "journal_mode"))).unwrap();
        assert_eq!(reader, r#"Text("wal")"#);
        let settings = |c: &Connection| {
            (
                pragma(c, "synchronous"),
                pragma(c, "foreign_keys"),
                pragma(c, "busy_timeout"),
                pragma(c, "temp_store"),
            )
        };
        let expected = (
            "Integer(2)".to_string(),
            "Integer(1)".to_string(),
            "Integer(30000)".to_string(),
            "Integer(2)".to_string(),
        );
        assert_eq!(settings(&writer), expected);
        assert_eq!(store.read(|c| Ok(settings(c))).unwrap(), expected);
    }

    #[test]
    fn a_migration_runs_after_backing_up_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let current = migrations::MIGRATIONS;
        let store = Store::open_with(dir.path(), current).unwrap();
        let blob = store.dir().join("dat.dat");
        std::fs::write(&blob, DAT).unwrap();
        store.import_dat(&blob, None).unwrap();
        drop(store);
        assert!(!dir.path().join(BACKUPS).exists());

        let second = "CREATE TABLE later (x INTEGER);";
        let store = Store::open_with(dir.path(), &[current, &[second]].concat()).unwrap();

        let backups: Vec<_> = std::fs::read_dir(dir.path().join(BACKUPS))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        let before = format!("Integer({})", current.len());
        assert_eq!(pragma(&backup, "user_version"), before);
        let revisions: i64 = backup
            .query_row("SELECT count(*) FROM dat_revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(revisions, 1);
        let version = store.read(|c| Ok(pragma(c, "user_version"))).unwrap();
        assert_eq!(version, format!("Integer({})", current.len() + 1));
    }

    #[test]
    fn migration_7_makes_a_general_root_sources_if_read_only_and_library_if_writable() {
        let dir = tempfile::tempdir().unwrap();
        let before = &migrations::MIGRATIONS[..6];
        let store = Store::open_with(dir.path(), before).unwrap();
        store
            .write(|tx| {
                for (name, kind, writable, paths) in [
                    ("sets", "general", false, r#"["Alpha"]"#),
                    ("play", "general", true, "[]"),
                    ("in", "ingest", true, "[]"),
                ] {
                    tx.execute(
                        "INSERT INTO roots (name, path, kind, writable, writable_paths, updated_by, updated_at)
                         VALUES (?1, '/' || ?1, ?2, ?3, ?4, 'Operator', 'then')",
                        rusqlite::params![name, kind, writable, paths],
                    )?;
                    let after = format!(
                        r#"{{"name":"{name}","path":"/{name}","kind":"{kind}","writable":{writable},"updated_by":"Operator","updated_at":"then"}}"#
                    );
                    tx.execute(
                        "INSERT INTO root_changes (root, action, root_after, changed_by, changed_at)
                         VALUES (?1, 'registered', ?2, 'Operator', 'then')",
                        rusqlite::params![name, after],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        drop(store);

        let store = Store::open(dir.path()).unwrap();

        let roots = store.roots();
        let kind = |name: &str| {
            let root = roots.root(name).unwrap();
            (root.kind, root.writable, root.writable_paths.clone())
        };
        assert_eq!(
            kind("sets"),
            (roots::RootKind::Sources, false, vec!["Alpha".to_owned()])
        );
        assert_eq!(kind("play"), (roots::RootKind::Library, true, Vec::new()));
        assert_eq!(kind("in"), (roots::RootKind::Ingest, true, Vec::new()));
        for (name, kind) in [
            ("sets", roots::RootKind::Sources),
            ("play", roots::RootKind::Library),
            ("in", roots::RootKind::Ingest),
        ] {
            let changes = store.root_changes(name).unwrap();
            assert_eq!(changes[0].after.as_ref().unwrap().kind, kind, "{name}");
        }
    }

    /// A process forked while the store is open shares its lock's open file until it runs its program; dropping the
    /// store releases the lock even so, as a store reopened in the same process needs.
    #[test]
    fn a_dropped_store_is_unlocked_though_a_copy_of_its_lock_file_is_still_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let copy = store._lock.0.try_clone().unwrap();

        drop(store);

        Store::open(dir.path()).unwrap();
        drop(copy);
    }

    #[test]
    fn a_second_open_of_the_same_store_fails_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();

        let second = Store::open(dir.path());

        let Err(Error::Refused(message)) = second else {
            panic!(
                "expected a refusal, got {second:?}",
                second = second.map(|_| ())
            );
        };
        assert!(message.contains("in use by another Stage Select process"));
        drop(first);
        Store::open(dir.path()).unwrap();
    }

    const DAT: &str = "<datafile><header><name>Test</name><version>1</version></header></datafile>";

    fn stamp(dir: &Path) -> (String, String) {
        let conn = Connection::open(dir.join(DATABASE)).unwrap();
        (
            pragma(&conn, "user_version"),
            pragma(&conn, "application_id"),
        )
    }

    fn baseline() -> (String, String) {
        (
            format!("Integer({})", migrations::MIGRATIONS.len()),
            format!("Integer({})", migrations::APPLICATION_ID),
        )
    }

    #[test]
    fn a_missing_store_starts_fresh_at_the_baseline_and_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store");

        drop(Store::open(&path).unwrap());

        assert_eq!(stamp(&path), baseline());
    }

    #[test]
    fn an_empty_database_file_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DATABASE), b"").unwrap();

        drop(Store::open(dir.path()).unwrap());

        assert_eq!(stamp(dir.path()), baseline());
    }

    /// The `application_id` 0.12.0 and every release before the rename stamped a store with: "TSel" in ASCII.
    const BEFORE_THE_RENAME: i32 = 0x5453_656C;

    #[test]
    fn a_store_with_a_schema_and_another_stamp_is_refused() {
        // Unstamped, as before the baseline, or stamped before the rename.
        for id in [None, Some(BEFORE_THE_RENAME)] {
            let dir = tempfile::tempdir().unwrap();
            let conn = Connection::open(dir.path().join(DATABASE)).unwrap();
            let stamp = format!("PRAGMA application_id = {};", migrations::APPLICATION_ID);
            let other = id.map_or(String::new(), |id| format!("PRAGMA application_id = {id};"));
            let baseline = migrations::MIGRATIONS[0];
            assert_eq!(baseline.matches(&stamp).count(), 1);
            conn.execute_batch(&baseline.replace(&stamp, &other))
                .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            drop(conn);

            let opened = Store::open(dir.path());

            let Err(Error::Refused(message)) = opened else {
                panic!("expected a refusal, got {:?}", opened.map(|_| ()));
            };
            assert_eq!(
                message,
                "This store was made by an earlier release and can't be opened. Start a new store."
            );
        }
    }

    #[test]
    fn a_store_from_a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = migrations::MIGRATIONS[0];
        drop(Store::open_with(dir.path(), &[first, "SELECT 1;"]).unwrap());

        assert!(matches!(
            Store::open_with(dir.path(), &[first]),
            Err(Error::Refused(_))
        ));
    }
}
