//! Single Copy (ADR 0023): Builds that place a Source Set's Games in the Library Folder holding them, by a rename, a
//! copy or a conversion, and recovery from a stop at each step, from synthetic DATs, Source Sets and files.

use stage_select_api::roots::RootSpec;
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::common::*;
use stage_select_api::build::{
    AddLibraryFolder, ItemKind, ItemState, LibraryFolder, PlaceHow, Plan, PlanItem,
};
use stage_select_api::selection::{
    CoverageDrift, KeptGame, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary,
};
use stage_select_api::storage::{StorageFormat, placed_path};
use stage_select_api::{AuditEvidence, EvidenceLevel, Finding, JobKind, JobState, Report};
use stage_select_store::{Error, Fault, Store, Workers};

pub(crate) const SET: &str = "Alpha";
pub(crate) const PROFILE: &str = "Mine";
pub(crate) const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

pub(crate) fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]
}

/// A store with the Source Set Alpha in the Root `sets`, which lists it among its `writable_paths`, and the Library
/// Folder `atari` bound to it in the writable Root `play`: the only one bound to it, so the one holding its Placed
/// Games.
pub(crate) struct Env {
    pub(crate) tmp: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) folder: LibraryFolder,
    pub(crate) sha256: String,
}

impl Env {
    pub(crate) fn new() -> Env {
        Env::with(&games())
    }

    /// As [`Env::new`], with a DAT of `games`.
    pub(crate) fn with(games: &[(&'static str, Vec<RomSpec>)]) -> Env {
        Env::setting(games, None, None)
    }

    /// As [`Env::with`], with a DAT whose header names the Header Rule `rule`.
    pub(crate) fn with_header(rule: &str, games: &[(&'static str, Vec<RomSpec>)]) -> Env {
        Env::setting(games, None, Some(rule))
    }

    /// As [`Env::new`], with `sets` listing no writable paths until Alpha is registered with Stage Select let change
    /// it as `may_change` says.
    pub(crate) fn registering(may_change: bool) -> Env {
        Env::setting(&games(), Some(may_change), None)
    }

    fn setting(
        games: &[(&'static str, Vec<RomSpec>)],
        may_change: Option<bool>,
        header_rule: Option<&str>,
    ) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/Alpha", "roots/play", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", tmp.path().join("roots/sets")),
                RootSpec::new("play", tmp.path().join("roots/play")).library(),
            ],
        );
        let dat = write_dat_as(
            &tmp.path().join("dats"),
            &Dat {
                file: "alpha.dat",
                version: "1",
                provider: "No-Intro",
                header_rule,
                ..Dat::default()
            },
            games,
        );
        let sha256 = store.import_dat(&dat, None).unwrap().0.sha256;
        store
            .register_source_set(
                "sets",
                SET,
                "No-Intro/Test - System",
                None,
                may_change.or(Some(true)),
                false,
                "Operator",
            )
            .unwrap();
        store
            .save_profile(
                PROFILE,
                &SelectionProfile::default(),
                None,
                "Operator",
                &[],
                WORKERS,
            )
            .unwrap();
        let folder = add_folder(&store, "atari");
        Env {
            tmp,
            store,
            folder,
            sha256,
        }
    }

    pub(crate) fn set(&self) -> PathBuf {
        self.tmp.path().join("roots/sets/Alpha")
    }

    pub(crate) fn library(&self) -> PathBuf {
        self.tmp.path().join("roots/play/atari")
    }

    pub(crate) fn audit(&self, evidence: AuditEvidence) -> Report {
        self.store
            .audit_source_set(SET, None, evidence)
            .unwrap()
            .report
    }

    /// Turns Single Copy on for play libraries, storing each game as `format`.
    pub(crate) fn single_copy(&self, format: StorageFormat) {
        let mut policy = self.store.storage_policy().unwrap();
        policy.play_libraries.format = format;
        policy.play_libraries.single_copy = true;
        self.store.set_storage_policy(&policy, "Operator").unwrap();
    }

    /// Selects `titles`, each the winner of its own group.
    pub(crate) fn select(&self, titles: &[&str]) -> i64 {
        self.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        assert_eq!(job.kind, JobKind::Selection);
        let selection = Selection {
            pins: SelectionPins::default(),
            dat_sha256: self.sha256.clone(),
            system: SET.into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups: titles
                .iter()
                .map(|title| SelectionGroup {
                    dependencies: Vec::new(),
                    group: title.to_lowercase(),
                    unresolved: false,
                    kept: vec![KeptGame {
                        title: (*title).into(),
                        rule: Rule {
                            stage: "Parent selection".into(),
                            reference: Some("REF0001".into()),
                            message: "m".into(),
                        },
                        unresolved: false,
                    }],
                    clones: vec![],
                    left_out: vec![],
                    choice: None,
                    drift: false,
                })
                .collect(),
            stale_choices: BTreeMap::new(),
        };
        self.store
            .record_selection(job.id, &selection)
            .unwrap()
            .selection
            .unwrap()
    }

    pub(crate) fn plan(&self, folder: &LibraryFolder, selection: i64) -> Plan {
        self.store.plan_build(folder.id, selection).unwrap()
    }

    /// Approves `plan`, then claims and runs its Build Job.
    pub(crate) fn build(&self, plan: &Plan) -> stage_select_api::Job {
        let job = self
            .store
            .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        self.store.run_build_job(job.id, None).unwrap()
    }

    /// Approves `plan` and runs its Build Job until `stop` says to stop it, as a kill would.
    pub(crate) fn build_until(
        &self,
        plan: &Plan,
        stop: impl Fn(Fault) -> bool + Send + Sync + 'static,
    ) {
        let job = self
            .store
            .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if stop(fault) {
                    panic!("killed at {fault:?}");
                }
            })));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.store.run_build_job(job.id, None)
        }));
        self.store.hook_faults(None);
        assert!(ran.is_err(), "{ran:?}");
    }

    /// Reopens the store, as the service does when it starts, and recovers what a stop left. Returns the folders
    /// recovery left blocked, with why.
    pub(crate) fn restart(self) -> (Env, Vec<(i64, String)>) {
        let Env {
            tmp,
            store,
            folder,
            sha256,
        } = self;
        let path = store.dir().to_path_buf();
        drop(store);
        let store = Store::open(&path).unwrap();
        store.recover_jobs(WORKERS).unwrap();
        let blocked = store.recover_builds().unwrap();
        store.recover_kept_deletions().unwrap();
        (
            Env {
                tmp,
                store,
                folder,
                sha256,
            },
            blocked,
        )
    }

    /// The Build of `job`'s item for `game`.
    pub(crate) fn item(&self, job: i64, game: &str) -> stage_select_api::build::BuildItem {
        let build = self.store.build_of_job(job).unwrap().unwrap();
        self.store
            .build(build.id)
            .unwrap()
            .items
            .into_iter()
            .find(|i| i.game == game)
            .unwrap()
    }

    /// Runs a stat walk of the Source Set: its counts of changed, added and removed files, if it found any.
    pub(crate) fn walk(&self) -> Option<(u64, u64, u64)> {
        let job = self.store.enqueue_stat_walk(SET, "Operator").unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        let ended = self.store.run_stat_walk_job(job.id).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        let set = self.store.source_set(SET).unwrap();
        set.stale.into_iter().find_map(|s| match s {
            stage_select_api::SourceSetStale::FilesChanged {
                changed,
                added,
                removed,
                ..
            } => Some((changed, added, removed)),
            _ => None,
        })
    }

    /// The file names in the Source Set's folder and in the Library Folder, leaving out what Stage Select keeps for
    /// itself.
    pub(crate) fn files(&self) -> (Vec<String>, Vec<String>) {
        (names(&self.set()), names(&self.library()))
    }
}

pub(crate) fn add_folder(store: &Store, path: &str) -> LibraryFolder {
    store
        .add_library_folder(&AddLibraryFolder {
            root: "play".into(),
            path: Some(path.into()),
            template: None,
            source_set: SET.into(),
            profile: PROFILE.into(),
        })
        .unwrap()
}

pub(crate) fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".stage-select")
        .collect();
    found.sort();
    found
}

/// Every file under the folder Stage Select keeps for itself in `dir`, relative to it.
pub(crate) fn private_files(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut walk = vec![dir.join(".stage-select")];
    while let Some(at) = walk.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            match path.is_dir() {
                true => walk.push(path),
                false => found.push(path.strip_prefix(dir).unwrap().display().to_string()),
            }
        }
    }
    found.retain(|f| !f.ends_with(".stage-select/marker.json") && !f.ends_with("marker"));
    found.sort();
    found
}

/// Flips the last byte of `path` in place, then sets its modification time back: the same size and modification
/// time, other bytes.
pub(crate) fn overwrite_keeping_stamp(path: &Path) {
    let st = std::fs::metadata(path).unwrap();
    let mut changed = std::fs::read(path).unwrap();
    let last = changed.len() - 1;
    changed[last] ^= 0xff;
    std::fs::write(path, &changed).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(st.modified().unwrap())
        .unwrap();
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    <sha2::Sha256 as sha2::Digest>::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn pong_zip(env: &Env) -> Vec<u8> {
    write_torrentzip(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    std::fs::read(env.set().join("Pong (USA).zip")).unwrap()
}

pub(crate) fn item<'a>(plan: &'a Plan, game: &str) -> &'a PlanItem {
    plan.content
        .items
        .iter()
        .find(|i| i.game == game)
        .unwrap_or_else(|| panic!("no item for {game}: {:?}", plan.content.items))
}

#[test]
fn single_copy_is_off_by_default_and_a_choice_for_play_libraries_only() {
    let env = Env::new();
    let policy = env.store.storage_policy().unwrap();
    assert!(!policy.play_libraries.single_copy);

    env.single_copy(StorageFormat::TorrentZip);
    let mut policy = env.store.storage_policy().unwrap();
    assert!(policy.play_libraries.single_copy);
    policy
        .play_libraries
        .systems
        .entry("No-Intro/Test - System".into())
        .or_default()
        .single_copy = Some(false);
    let saved = env.store.set_storage_policy(&policy, "Operator").unwrap();
    assert_eq!(
        saved.play_libraries.systems["No-Intro/Test - System"].single_copy,
        Some(false)
    );

    let mut refused = saved.clone();
    refused.source_sets.single_copy = true;
    let Err(Error::Refused(why)) = env.store.set_storage_policy(&refused, "Operator") else {
        panic!("a source set's single copy was saved");
    };
    assert!(why.contains("play libraries"), "{why}");
}

#[test]
fn saving_single_copy_changes_no_file_and_a_plan_without_it_copies() {
    let env = Env::new();
    let bytes = pong_zip(&env);
    env.audit(AuditEvidence::Full);
    let selection = env.select(&["Pong (USA)"]);
    env.single_copy(StorageFormat::TorrentZip);
    assert_eq!(env.files().0, ["Pong (USA).zip"]);
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        bytes
    );

    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.single_copy = false;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Copy);
    assert!(!item(&plan, "Pong (USA)").changes_source_set);
}

#[test]
fn a_source_set_registered_for_stage_select_to_change_has_its_games_placed() {
    let env = Env::registering(true);
    std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).a26".into()])
    );
}

#[test]
fn a_source_set_stage_select_may_not_change_has_each_game_copied_and_says_where_to_let_it() {
    let env = Env::registering(false);
    std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    assert_ne!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("nothing is placed")
                && n.contains("\"Stage Select may change this source set\"")),
        "{:?}",
        plan.content.notes
    );
}

#[test]
fn on_one_filesystem_a_game_is_placed_by_a_rename_its_evidence_carried_over_and_not_read_again() {
    let env = Env::new();
    let bytes = pong_zip(&env);
    let first = env.audit(AuditEvidence::Full);
    let read_at = first
        .entries
        .iter()
        .find(|e| e.path == "Pong (USA).zip")
        .unwrap()
        .evidence
        .as_ref()
        .unwrap()
        .read_at;
    let before = std::fs::metadata(env.set().join("Pong (USA).zip")).unwrap();
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(pong.kind, ItemKind::Place);
    assert!(pong.changes_source_set);
    let place = pong.place.as_ref().unwrap();
    assert_eq!(place.how, PlaceHow::Rename);
    assert_eq!((place.freed, place.written), (bytes.len() as u64, 0));
    assert_eq!((plan.summary.place, plan.summary.place_renamed), (1, 1));
    assert!(
        plan.content.notes.iter().any(
            |n| n.contains("single copy places 1 file here") && n.contains("moved by a rename")
        ),
        "{:?}",
        plan.content.notes
    );
    // What it frees and writes are estimates, and say so.
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("Both are estimates")),
        "{:?}",
        plan.content.notes
    );

    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(env.item(job.id, "Pong (USA)").state, ItemState::Applied);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).zip".into()])
    );
    // The same file, renamed: never copied.
    let after = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(
        std::fs::read(env.library().join("Pong (USA).zip")).unwrap(),
        bytes
    );
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1);
    assert_eq!(
        (placed[0].game.as_str(), placed[0].path.as_str()),
        ("Pong (USA)", "Pong (USA).zip")
    );
    assert_eq!(placed[0].sha256, None, "a rename reads nothing");
    assert_eq!(
        placed[0].ctime_ns,
        after.ctime() as i128 * 1_000_000_000 + after.ctime_nsec() as i128
    );

    // The next Audit trusts the read the first made, where the game is placed now.
    let next = env.audit(AuditEvidence::Incremental);
    let entry = next
        .entries
        .iter()
        .find(|e| e.path == placed_path(env.folder.id, "Pong (USA).zip"))
        .unwrap_or_else(|| panic!("{:?}", next.entries));
    assert_eq!(entry.finding, Finding::Matched);
    let evidence = entry.evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Payload);
    assert_eq!(evidence.read_at, read_at);
    assert!(evidence.reused.is_some(), "{evidence:?}");
}

#[test]
fn a_loose_game_stored_loose_is_placed_by_a_rename() {
    let env = Env::new();
    std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Rename
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).a26".into()])
    );
}

#[test]
fn across_filesystems_a_game_is_copied_read_back_and_its_original_deleted() {
    let env = Env::new();
    let bytes = pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    let place = item(&plan, "Pong (USA)").place.clone().unwrap();
    assert_eq!(place.how, PlaceHow::Copy);
    assert_eq!(
        (place.freed, place.written),
        (bytes.len() as u64, bytes.len() as u64)
    );

    let before = std::fs::metadata(env.set().join("Pong (USA).zip")).unwrap();
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(env.item(job.id, "Pong (USA)").state, ItemState::Applied);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).zip".into()])
    );
    let after = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();
    assert_ne!(after.ino(), before.ino(), "a copy");
    assert_eq!(
        std::fs::read(env.library().join("Pong (USA).zip")).unwrap(),
        bytes
    );
    assert_eq!(private_files(&env.set()), Vec::<String>::new());
    assert_eq!(private_files(&env.library()), Vec::<String>::new());
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(
        placed[0].sha256.as_deref(),
        Some(sha256_hex(&bytes).as_str())
    );

    let next = env.audit(AuditEvidence::Incremental);
    let entry = next
        .entries
        .iter()
        .find(|e| e.path == placed_path(env.folder.id, "Pong (USA).zip"))
        .unwrap();
    assert_eq!(entry.finding, Finding::Matched);
}

#[test]
fn a_copy_whose_original_changes_before_it_is_deleted_keeps_the_original_in_the_source_sets_kept_files()
 {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);

    // Written over in place once its copy is recorded placed, its size and modification time set back: only its
    // change time, and reading it again, show it is not the file copied.
    let original = env.set().join("Pong (USA).zip");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceRecorded(_)) {
                overwrite_keeping_stamp(&original);
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let pong = env.item(job.id, "Pong (USA)");
    assert_eq!(pong.state, ItemState::Applied);
    let error = pong.error.unwrap();
    assert!(
        error.contains("kept in source set Alpha's kept files as Pong (USA).zip.kept"),
        "{error}"
    );
    // Never deleted, and never left in the source set as a second copy: kept, by the build.
    assert_eq!(env.files(), (vec![], vec!["Pong (USA).zip".into()]));
    assert_kept_by_build(&env, job.id, None);
}

/// A Source Set file a Build moved into the Source Set's Kept Files is recorded kept only where the record says it is:
/// one whose retention area was moved meanwhile, as anyone in the folder's group may, is not recorded, and the item
/// says where to look.
#[test]
fn a_kept_files_folder_moved_while_a_build_keeps_an_original_is_never_recorded_as_its_place() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    let original = env.set().join("Pong (USA).zip");
    let area = env.set().join(".stage-select/retained");
    let moved = env.set().join(".stage-select/retained-moved");
    let (from, to) = (area, moved);
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceRecorded(_)) {
                overwrite_keeping_stamp(&original);
            }
            if matches!(fault, Fault::PlaceKeptAside(_)) && from.exists() && !to.exists() {
                std::fs::rename(&from, &to).unwrap();
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);

    assert!(
        private_files(&env.set())
            .iter()
            .any(|f| f.starts_with(".stage-select/retained-moved/")
                && f.ends_with("Pong (USA).zip.kept")),
        "{:?}",
        private_files(&env.set())
    );
    assert!(
        env.store.source_kept_files(SET).unwrap().is_empty(),
        "recorded where it is not"
    );
    let said = format!("{:?} {:?}", job.error, env.item(job.id, "Pong (USA)").error);
    assert!(
        said.contains("was moved before it was recorded, so it is not at"),
        "{said}"
    );
}

/// Asserts that the source set's one kept file is its Pong (USA).zip, kept by the build of job `job`, and holds
/// `bytes` if given; nothing else is left in its private folders.
fn assert_kept_by_build(env: &Env, job: i64, bytes: Option<&[u8]>) {
    let build = env.store.build_of_job(job).unwrap().unwrap().id;
    let kept = env.store.source_kept_files(SET).unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].build, Some(build));
    assert_eq!(kept[0].plan, None);
    assert_eq!(kept[0].name, "Pong (USA).zip");
    assert_eq!(kept[0].kept_name, "Pong (USA).zip.kept");
    let path = format!(
        ".stage-select/retained/{}/Pong (USA).zip.kept",
        kept[0].generation
    );
    assert_eq!(private_files(&env.set()), vec![path.clone()]);
    if let Some(bytes) = bytes {
        assert_eq!(std::fs::read(env.set().join(path)).unwrap(), bytes);
    }
}

#[test]
fn a_copy_changed_before_the_last_check_keeps_the_original_in_the_source_sets_kept_files() {
    let env = Env::new();
    let bytes = pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);

    // Written over in place, its size and modification time set back, just before it is checked a last time.
    let placed = env.library().join("Pong (USA).zip");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceChecking(_)) {
                overwrite_keeping_stamp(&placed);
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let error = env.item(job.id, "Pong (USA)").error.unwrap();
    assert!(
        error.contains("kept in source set Alpha's kept files as Pong (USA).zip.kept"),
        "{error}"
    );
    assert!(!env.set().join("Pong (USA).zip").exists());
    assert_kept_by_build(&env, job.id, Some(&bytes));
}

#[test]
fn a_torrentzip_stored_loose_is_converted_on_the_way_and_its_original_deleted() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    let place = item(&plan, "Pong (USA)").place.clone().unwrap();
    assert_eq!(place.how, PlaceHow::Convert);
    assert_eq!(place.written, 4);

    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).a26".into()])
    );
    assert_eq!(
        std::fs::read(env.library().join("Pong (USA).a26")).unwrap(),
        b"pong"
    );
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed[0].path, "Pong (USA).a26");
    assert_eq!(
        placed[0].sha256.as_deref(),
        Some(sha256_hex(b"pong").as_str())
    );

    let next = env.audit(AuditEvidence::Incremental);
    let entry = next
        .entries
        .iter()
        .find(|e| e.path == placed_path(env.folder.id, "Pong (USA).a26"))
        .unwrap_or_else(|| panic!("{:?}", next.entries));
    assert_eq!(entry.finding, Finding::Matched);
}

/// Places Pong, written in the Source Set by `write`, in a folder storing each game as `format`, after an Audit with
/// `evidence`, copying `across` filesystems, and asserts it was placed `how` as `placed` and that a stat walk then
/// finds nothing changed. Returns the path of its file in the Library Folder.
fn place_then_walk(
    write: impl Fn(&Env),
    evidence: AuditEvidence,
    format: StorageFormat,
    across: bool,
    (how, placed): (PlaceHow, &str),
) -> (Env, PathBuf) {
    let env = Env::new();
    write(&env);
    env.audit(evidence);
    assert_eq!(env.walk(), None, "nothing changed since the audit");
    env.single_copy(format);
    env.store.copy_across_filesystems(across);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").place.as_ref().unwrap().how, how);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(env.files().1, [placed]);
    // What the build moved, copied or wrote, it recorded as the audit's: nothing looks changed, added or removed.
    assert_eq!(env.walk(), None, "placed by a {how:?}");
    let at = env.library().join(placed);
    (env, at)
}

fn loose_pong(env: &Env) {
    std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
}

#[test]
fn a_game_placed_by_a_rename_leaves_its_source_set_unchanged_until_its_file_is_edited() {
    let pong = |env: &Env| {
        pong_zip(env);
    };
    let (env, at) = place_then_walk(
        pong,
        AuditEvidence::Full,
        StorageFormat::TorrentZip,
        false,
        (PlaceHow::Rename, "Pong (USA).zip"),
    );
    overwrite_keeping_stamp(&at);
    assert_eq!(env.walk(), Some((1, 0, 0)));
}

#[test]
fn a_game_placed_by_a_copy_leaves_its_source_set_unchanged() {
    let (env, at) = place_then_walk(
        |env| {
            pong_zip(env);
        },
        AuditEvidence::Full,
        StorageFormat::TorrentZip,
        true,
        (PlaceHow::Copy, "Pong (USA).zip"),
    );
    overwrite_keeping_stamp(&at);
    assert_eq!(env.walk(), Some((1, 0, 0)));
}

#[test]
fn a_game_converted_as_it_is_placed_leaves_its_source_set_unchanged() {
    let (env, at) = place_then_walk(
        |env| {
            pong_zip(env);
        },
        AuditEvidence::Full,
        StorageFormat::Loose,
        false,
        (PlaceHow::Convert, "Pong (USA).a26"),
    );
    overwrite_keeping_stamp(&at);
    assert_eq!(env.walk(), Some((1, 0, 0)));
    let (env, at) = place_then_walk(
        loose_pong,
        AuditEvidence::Full,
        StorageFormat::TorrentZip,
        false,
        (PlaceHow::Convert, "Pong (USA).zip"),
    );
    overwrite_keeping_stamp(&at);
    assert_eq!(env.walk(), Some((1, 0, 0)));
}

/// A converted file edited before it is checked keeps its Source Set's files, and isn't recorded as audited.
#[test]
fn a_converted_file_edited_before_it_is_checked_is_found_changed() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Convert
    );
    let at = env.library().join("Pong (USA).a26");
    let once = std::sync::Once::new();
    let edited = at.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::AfterPublish(_)) {
                once.call_once(|| overwrite_keeping_stamp(&edited));
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_ne!(std::fs::read(&at).unwrap(), b"pong");
    let (changed, _, _) = env.walk().expect("the edited file is found changed");
    assert!(changed >= 1, "{changed}");
}

/// A file edited in place just before its rename, keeping its size and modification time, stays in its source set,
/// found changed.
#[test]
fn a_file_edited_just_before_its_rename_stays_in_the_source_set_found_changed() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Rename
    );
    let source = env.set().join("Pong (USA).zip");
    let edited = source.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceRenaming(_)) {
                overwrite_keeping_stamp(&edited);
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let pong = env.item(job.id, "Pong (USA)");
    assert_eq!(pong.state, ItemState::Failed, "{pong:?}");
    assert_eq!(
        env.files(),
        (vec!["Pong (USA).zip".into()], Vec::<String>::new())
    );
    assert_eq!(env.walk(), Some((1, 0, 0)));
}

/// A file edited in place as it is renamed into place, keeping its size and modification time, is found changed.
#[test]
fn a_file_edited_as_it_is_renamed_into_place_is_found_changed() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Rename
    );
    let at = env.library().join("Pong (USA).zip");
    let edited = at.clone();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceMoved(_)) {
                overwrite_keeping_stamp(&edited);
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(env.files().1, ["Pong (USA).zip"]);
    assert_eq!(env.walk(), Some((1, 0, 0)));
}

/// A file renamed into place, then edited while the service is stopped, is found changed once recovery records it.
#[test]
fn a_file_edited_after_its_rename_while_the_service_was_stopped_is_found_changed() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Rename
    );
    env.build_until(&plan, |f| matches!(f, Fault::PlaceMoved(_)));
    let at = env.library().join("Pong (USA).zip");
    overwrite_keeping_stamp(&at);

    let (env, blocked) = env.restart();
    assert_eq!(blocked, Vec::<(i64, String)>::new());
    assert_eq!(env.files().1, ["Pong (USA).zip"]);
    assert_eq!(env.walk(), Some((1, 0, 0)));
}

#[test]
fn a_plan_is_refused_once_the_storage_policy_changed_since_it_was_made() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);

    let mut policy = env.store.storage_policy().unwrap();
    policy.play_libraries.single_copy = false;
    env.store.set_storage_policy(&policy, "Operator").unwrap();
    let refused = env
        .store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS);
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("changed")),
        "{refused:?}"
    );
    assert_eq!(env.files().0, ["Pong (USA).zip"]);
}

#[test]
fn with_several_folders_bound_and_none_chosen_to_hold_its_games_a_build_places_nothing_and_says_why()
 {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let other = add_folder(&env.store, "atari-too");
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Copy);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("you haven't chosen the one that holds its games")),
        "{:?}",
        plan.content.notes
    );

    // Once one is chosen, it places the game, and the other copies it from where it is placed.
    env.store
        .set_holding_folder(SET, Some(env.folder.id))
        .unwrap();
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    let plan = env.plan(&other, selection);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(pong.kind, ItemKind::Copy);
    assert!(!pong.changes_source_set);
    assert_eq!(
        pong.sources[0].path,
        placed_path(env.folder.id, "Pong (USA).zip")
    );
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("another play library folder holds")),
        "{:?}",
        plan.content.notes
    );
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded);
    let copy = env.item(job.id, "Pong (USA)");
    assert_eq!(copy.state, ItemState::Applied, "{copy:?}");
    let copied = env.tmp.path().join("roots/play/atari-too/Pong (USA).zip");
    assert_eq!(
        std::fs::read(copied).unwrap(),
        std::fs::read(env.library().join("Pong (USA).zip")).unwrap()
    );
}

#[test]
fn a_placed_game_stays_placed_when_the_folder_is_built_again() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    assert_eq!(
        env.build(&env.plan(&env.folder, selection)).state,
        JobState::Succeeded
    );

    env.audit(AuditEvidence::Incremental);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Unchanged);
    assert!(
        plan.content.removals.is_empty(),
        "{:?}",
        plan.content.removals
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).zip".into()])
    );
}

#[test]
fn a_game_already_built_in_the_holding_folder_is_placed_by_deleting_its_source_set_copy() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    let selection = env.select(&["Pong (USA)"]);
    assert_eq!(
        env.build(&env.plan(&env.folder, selection)).state,
        JobState::Succeeded
    );
    let built = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();

    env.single_copy(StorageFormat::TorrentZip);
    let plan = env.plan(&env.folder, selection);
    let place = item(&plan, "Pong (USA)").place.clone().unwrap();
    assert_eq!(place.how, PlaceHow::Built);
    assert_eq!(place.written, 0);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(
        env.files(),
        (Vec::<String>::new(), vec!["Pong (USA).zip".into()])
    );
    let now = std::fs::metadata(env.library().join("Pong (USA).zip")).unwrap();
    assert_eq!(now.ino(), built.ino());
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
}

#[test]
fn a_source_set_is_locked_while_a_build_placing_its_games_is_queued() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    env.store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    assert!(matches!(
        env.store
            .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental),
        Err(Error::Conflict(_))
    ));
    let repair = env.store.plan_repair(SET, &Default::default()).unwrap();
    assert!(matches!(
        env.store
            .approve_repair(repair.id, &repair.digest, "Operator", WORKERS),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn a_game_that_shares_its_source_set_file_with_another_is_copied_not_placed() {
    let env = Env::new();
    // One zip holding both games' ROMs: deleting it would lose Combat.
    write_torrentzip(
        &env.set().join("Pong (USA).zip"),
        &[("Combat (USA).a26", b"combat"), ("Pong (USA).a26", b"pong")],
    );
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);

    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Repack);
    assert!(
        plan.content
            .notes
            .iter()
            .any(|n| n.contains("copied rather than placed")
                && n.contains("hold more than its games")),
        "{:?}",
        plan.content.notes
    );
}

/// After a stop at any step of a placement, and recovery, the game has exactly one copy, and it is recorded: in the
/// Source Set, unplaced, or placed in the Library Folder with nothing left in the Source Set.
fn one_recorded_copy(env: &Env, ext: &str, bytes: &[u8]) -> bool {
    let (set, library) = env.files();
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(private_files(&env.set()), Vec::<String>::new(), "set");
    let staged: Vec<String> = private_files(&env.library())
        .into_iter()
        .filter(|f| !f.contains("retained"))
        .collect();
    assert_eq!(staged, Vec::<String>::new(), "library");
    match (set.as_slice(), library.as_slice()) {
        ([s], []) => {
            assert_eq!(s, "Pong (USA).zip");
            assert!(placed.is_empty(), "{placed:?}");
            false
        }
        ([], [l]) => {
            assert_eq!(*l, format!("Pong (USA).{ext}"));
            assert_eq!(std::fs::read(env.library().join(l)).unwrap(), bytes);
            assert_eq!(placed.len(), 1, "{placed:?}");
            assert_eq!(&placed[0].path, l);
            true
        }
        _ => panic!("{set:?} {library:?} {placed:?}"),
    }
}

/// Where a test stops a placement, the fault it stops at, and whether the game is placed once recovery settles it.
type Stop = (&'static str, fn(Fault) -> bool, bool);

fn stopped_placement(
    format: StorageFormat,
    across: bool,
    stops: &[Stop],
    ext: &str,
    placed_bytes: fn(&[u8]) -> Vec<u8>,
) {
    for (name, stop, placed) in stops {
        let env = Env::new();
        let zip = pong_zip(&env);
        env.audit(AuditEvidence::Full);
        env.single_copy(format);
        env.store.copy_across_filesystems(across);
        let selection = env.select(&["Pong (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place, "{name}");
        env.build_until(&plan, *stop);

        let (env, blocked) = env.restart();
        assert_eq!(blocked, Vec::<(i64, String)>::new(), "{name}");
        assert_eq!(
            one_recorded_copy(&env, ext, &placed_bytes(&zip)),
            *placed,
            "{name}"
        );
        // The folder is free again, and the next build finishes what the stop left.
        env.store.copy_across_filesystems(across);
        env.audit(AuditEvidence::Incremental);
        let job = env.build(&env.plan(&env.folder, selection));
        assert_eq!(job.state, JobState::Succeeded, "{name}: {job:?}");
        assert!(one_recorded_copy(&env, ext, &placed_bytes(&zip)), "{name}");
    }
}

#[test]
fn a_rename_the_service_stopped_in_leaves_exactly_one_recorded_copy() {
    stopped_placement(
        StorageFormat::TorrentZip,
        false,
        &[
            ("renaming", |f| matches!(f, Fault::PlaceRenaming(_)), false),
            ("moved", |f| matches!(f, Fault::PlaceMoved(_)), true),
        ],
        "zip",
        |zip| zip.to_vec(),
    );
}

#[test]
fn a_copy_the_service_stopped_in_leaves_exactly_one_recorded_copy() {
    stopped_placement(
        StorageFormat::TorrentZip,
        true,
        &[
            ("copied", |f| matches!(f, Fault::PlaceCopied(_)), true),
            ("recorded", |f| matches!(f, Fault::PlaceRecorded(_)), true),
            ("checking", |f| matches!(f, Fault::PlaceChecking(_)), true),
            ("disposed", |f| matches!(f, Fault::PlaceDisposed(_)), true),
        ],
        "zip",
        |zip| zip.to_vec(),
    );
}

#[test]
fn a_conversion_the_service_stopped_in_leaves_exactly_one_recorded_copy() {
    stopped_placement(
        StorageFormat::Loose,
        false,
        &[
            ("staged", |f| matches!(f, Fault::Staged(_)), false),
            // Committed to go in place once every file of it is written: recovery finishes it.
            (
                "before publish",
                |f| matches!(f, Fault::BeforePublish(_)),
                true,
            ),
            (
                "published",
                |f| matches!(f, Fault::PublishedUnsynced(_)),
                true,
            ),
            (
                "after publish",
                |f| matches!(f, Fault::AfterPublish(_)),
                true,
            ),
            ("recorded", |f| matches!(f, Fault::PlaceRecorded(_)), true),
            ("checking", |f| matches!(f, Fault::PlaceChecking(_)), true),
            ("disposed", |f| matches!(f, Fault::PlaceDisposed(_)), true),
        ],
        "a26",
        |_| b"pong".to_vec(),
    );
}

#[test]
fn a_game_whose_file_changed_since_the_plan_stays_in_the_source_set_unplaced() {
    let cases: [(&str, StorageFormat, bool); 3] = [
        ("rename", StorageFormat::TorrentZip, false),
        ("copy", StorageFormat::TorrentZip, true),
        ("convert", StorageFormat::Loose, false),
    ];
    for (name, format, across) in cases {
        let env = Env::new();
        let bytes = pong_zip(&env);
        write_torrentzip(
            &env.set().join("Combat (USA).zip"),
            &[("Combat (USA).a26", b"combat")],
        );
        env.audit(AuditEvidence::Full);
        env.single_copy(format);
        env.store.copy_across_filesystems(across);
        let selection = env.select(&["Combat (USA)", "Pong (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place, "{name}");
        assert_eq!(plan.content.items[0].game, "Combat (USA)", "{name}");

        // While Combat is placed, Pong's file is put back as another file with the same bytes.
        let path = env.set().join("Pong (USA).zip");
        let once = std::sync::Once::new();
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if matches!(
                    fault,
                    Fault::PlaceRenaming(0) | Fault::PlaceCopied(0) | Fault::Staged(0)
                ) {
                    once.call_once(|| {
                        std::fs::remove_file(&path).unwrap();
                        std::fs::write(&path, &bytes).unwrap();
                    });
                }
            })));
        let job = env.build(&plan);
        env.store.hook_faults(None);
        assert_eq!(job.state, JobState::Succeeded, "{name}: {job:?}");
        let pong = env.item(job.id, "Pong (USA)");
        assert_eq!(pong.state, ItemState::Failed, "{name}: {pong:?}");
        assert!(
            pong.error
                .as_deref()
                .unwrap()
                .contains("stays in source set Alpha"),
            "{name}: {pong:?}"
        );
        let combat = if format == StorageFormat::Loose {
            "a26"
        } else {
            "zip"
        };
        assert_eq!(
            env.files(),
            (
                vec!["Pong (USA).zip".into()],
                vec![format!("Combat (USA).{combat}")]
            ),
            "{name}"
        );
        let placed = env.store.placed_files(SET).unwrap();
        assert_eq!(placed.len(), 1, "{name}");
        assert_eq!(placed[0].game, "Combat (USA)", "{name}");
        assert_eq!(
            private_files(&env.library()),
            Vec::<String>::new(),
            "{name}"
        );
    }
}

/// A rename that finds another file under the original's name as it renames it puts that file back.
#[test]
fn a_file_put_in_place_of_the_original_just_before_its_rename_goes_back() {
    let env = Env::new();
    let bytes = pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);

    let path = env.set().join("Pong (USA).zip");
    let aside = env.tmp.path().join("pong-aside.zip");
    let (p, a) = (path.clone(), aside.clone());
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceRenaming(_)) {
                std::fs::rename(&p, &a).unwrap();
                std::fs::write(&p, b"another").unwrap();
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let pong = env.item(job.id, "Pong (USA)");
    assert_eq!(pong.state, ItemState::Failed, "{pong:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"another");
    assert_eq!(std::fs::read(&aside).unwrap(), bytes);
    assert_eq!(env.files().1, Vec::<String>::new());
    assert!(env.store.placed_files(SET).unwrap().is_empty());
}

#[test]
fn a_repair_planned_before_a_build_placed_its_games_is_refused() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let repair = env.store.plan_repair(SET, &Default::default()).unwrap();
    assert_eq!(
        env.build(&env.plan(&env.folder, selection)).state,
        JobState::Succeeded
    );

    let refused = env
        .store
        .approve_repair(repair.id, &repair.digest, "Operator", WORKERS);
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
}

#[test]
fn a_built_file_changed_since_it_was_built_never_stands_in_for_its_source_set_files() {
    let env = Env::new();
    pong_zip(&env);
    write_torrentzip(
        &env.set().join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    env.audit(AuditEvidence::Full);
    let selection = env.select(&["Combat (USA)", "Pong (USA)"]);
    assert_eq!(
        env.build(&env.plan(&env.folder, selection)).state,
        JobState::Succeeded
    );
    env.single_copy(StorageFormat::TorrentZip);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(
        item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Built
    );

    // Once Combat is placed, Pong's built file is written over, its size and modification time set back.
    let built = env.library().join("Pong (USA).zip");
    let once = std::sync::Once::new();
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceDisposed(0)) {
                once.call_once(|| overwrite_keeping_stamp(&built));
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let pong = env.item(job.id, "Pong (USA)");
    assert_eq!(pong.state, ItemState::Failed, "{pong:?}");
    assert_eq!(
        env.files(),
        (
            vec!["Pong (USA).zip".into()],
            vec!["Combat (USA).zip".into(), "Pong (USA).zip".into()]
        )
    );
    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].game, "Combat (USA)");
}

#[test]
fn a_deletion_of_the_source_sets_kept_files_waits_for_a_build_placing_its_games() {
    let env = Env::new();
    pong_zip(&env);
    std::fs::write(env.set().join("readme.txt"), b"hello").unwrap();
    env.audit(AuditEvidence::Full);
    // A repair keeps the file no game needs aside, then audits the source set again.
    let repair = env.store.plan_repair(SET, &Default::default()).unwrap();
    let approved = env
        .store
        .approve_repair(repair.id, &repair.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    let ended = env.store.run_repair_job(job, None).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    env.store.fulfil_repair_audits().unwrap();
    let audit = env.store.repair_plan(repair.id).unwrap().audit_job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, audit);
    env.store.run_audit_job(audit, None).unwrap();
    let kept = env.store.source_kept_files(SET).unwrap();
    assert_eq!(kept.len(), 1);

    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    env.store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let deletion = env
        .store
        .plan_source_kept_deletion(
            SET,
            &stage_select_api::build::PlanKeptDeletion {
                ids: vec![kept[0].id],
                all: false,
            },
        )
        .unwrap();
    let refused = env.store.approve_source_kept_deletion(
        deletion.id,
        deletion.content.source_set,
        &deletion.digest,
        "Operator",
    );
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("places the source set's games")),
        "{refused:?}"
    );
}

/// Replaces `dir`, the play library folder `library` or a folder above it, with a new folder, as if another were
/// mounted where it was, leaving it at `<dir>.moved`: the library folder's path then leads to a new folder holding a
/// copy of its marker.
pub(crate) fn replace_with_marked_copy(dir: &Path, library: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let moved = PathBuf::from(format!("{}.moved", dir.display()));
    let was = moved.join(library.strip_prefix(dir).unwrap());
    std::fs::rename(dir, &moved).unwrap();
    std::fs::create_dir_all(library.join(".stage-select")).unwrap();
    std::fs::set_permissions(
        library.join(".stage-select"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    for entry in std::fs::read_dir(was.join(".stage-select")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(
                entry.path(),
                library.join(".stage-select").join(entry.file_name()),
            )
            .unwrap();
        }
    }
}

/// The folder a test replaces while a game is placed: the play library folder itself, or the root above it.
fn replaced(env: &Env, parent: bool) -> (PathBuf, PathBuf) {
    let dir = match parent {
        false => env.library(),
        true => env.tmp.path().join("roots/play"),
    };
    (dir, env.library())
}

#[test]
fn a_folder_replaced_before_its_rename_is_checked_puts_the_original_back() {
    for parent in [false, true] {
        let env = Env::new();
        let bytes = pong_zip(&env);
        env.audit(AuditEvidence::Full);
        env.single_copy(StorageFormat::TorrentZip);
        let selection = env.select(&["Pong (USA)"]);
        let plan = env.plan(&env.folder, selection);
        assert_eq!(
            item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
            PlaceHow::Rename
        );

        // Another folder, marked as the play library folder, appears at its path just before the rename.
        let (dir, library) = replaced(&env, parent);
        let once = std::sync::Once::new();
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if matches!(fault, Fault::PlaceRenaming(_)) {
                    once.call_once(|| replace_with_marked_copy(&dir, &library));
                }
            })));
        let job = env.build(&plan);
        env.store.hook_faults(None);
        let pong = env.item(job.id, "Pong (USA)");
        assert_eq!(pong.state, ItemState::Failed, "{parent}: {pong:?}");
        let error = pong.error.unwrap_or_default();
        assert!(
            error.contains("no longer the one at the play library folder's path")
                && error.contains("put back"),
            "{parent}: {error}"
        );
        assert_eq!(
            std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
            bytes,
            "{parent}"
        );
        assert!(env.store.placed_files(SET).unwrap().is_empty(), "{parent}");
    }
}

#[test]
fn a_folder_replaced_before_the_last_check_keeps_the_copied_original() {
    for parent in [false, true] {
        let env = Env::new();
        let bytes = pong_zip(&env);
        env.audit(AuditEvidence::Full);
        env.single_copy(StorageFormat::TorrentZip);
        env.store.copy_across_filesystems(true);
        let selection = env.select(&["Pong (USA)"]);
        let plan = env.plan(&env.folder, selection);

        // Its copy recorded placed, and its original set aside to delete, the folder is replaced before the last
        // check: the copy is no longer at the folder's path, so the original is never deleted.
        let (dir, library) = replaced(&env, parent);
        let once = std::sync::Once::new();
        env.store
            .hook_faults(Some(std::sync::Arc::new(move |fault| {
                if matches!(fault, Fault::PlaceChecking(_)) {
                    once.call_once(|| replace_with_marked_copy(&dir, &library));
                }
            })));
        let job = env.build(&plan);
        env.store.hook_faults(None);
        let error = env.item(job.id, "Pong (USA)").error.unwrap_or_default();
        assert!(
            error.contains("no longer the one at the play library folder's path")
                && error.contains("kept in source set Alpha's kept files"),
            "{parent}: {error}"
        );
        assert_kept_by_build(&env, job.id, Some(&bytes));
    }
}

#[test]
fn after_a_stop_a_folder_replaced_leaves_its_placement_outstanding() {
    // Stopped once the original is moved into the folder, and once it is set aside to delete.
    type Across = (bool, fn(Fault) -> bool);
    let stops: [Across; 2] = [
        (false, |f| matches!(f, Fault::PlaceMoved(_))),
        (true, |f| matches!(f, Fault::PlaceChecking(_))),
    ];
    for (across, stop) in stops {
        let env = Env::new();
        let bytes = pong_zip(&env);
        env.audit(AuditEvidence::Full);
        env.single_copy(StorageFormat::TorrentZip);
        env.store.copy_across_filesystems(across);
        let selection = env.select(&["Pong (USA)"]);
        let plan = env.plan(&env.folder, selection);
        let library = env.library();
        env.build_until(&plan, move |fault| {
            if stop(fault) {
                replace_with_marked_copy(&library, &library);
                return true;
            }
            false
        });

        let (env, blocked) = env.restart();
        assert_eq!(blocked.len(), 1, "{across}: {blocked:?}");
        assert!(
            blocked[0]
                .1
                .contains("no longer the one at the play library folder's path"),
            "{across}: {blocked:?}"
        );
        // Neither done nor failed: the original is where the stop left it, the build unfinished.
        let moved = PathBuf::from(format!("{}.moved", env.library().display()));
        match across {
            false => assert_eq!(std::fs::read(moved.join("Pong (USA).zip")).unwrap(), bytes),
            true => {
                let aside = private_files(&env.set());
                assert_eq!(aside.len(), 1, "{aside:?}");
                assert!(aside[0].contains(".deleting/"), "{aside:?}");
                assert_eq!(std::fs::read(env.set().join(&aside[0])).unwrap(), bytes);
            }
        }
        assert!(env.store.source_kept_files(SET).unwrap().is_empty());
        assert!(matches!(
            env.store
                .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental),
            Err(Error::Conflict(_))
        ));
    }
}

#[test]
fn an_original_set_aside_whose_folder_is_gone_after_a_stop_is_never_stranded() {
    let env = Env::new();
    std::fs::create_dir(env.set().join("nested")).unwrap();
    write_torrentzip(
        &env.set().join("nested/Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);

    // Stopped with the original set aside to delete; its folder, empty now, is removed before the service starts.
    env.build_until(&plan, |f| matches!(f, Fault::PlaceChecking(_)));
    std::fs::remove_dir(env.set().join("nested")).unwrap();
    let (env, blocked) = env.restart();
    assert_eq!(blocked, Vec::<(i64, String)>::new());

    // Proven unchanged, through its folder made again, it is deleted: nothing is left set aside, and the game is
    // placed.
    assert_eq!(private_files(&env.set()), Vec::<String>::new());
    assert_eq!(env.files().1, vec!["Pong (USA).zip".to_owned()]);
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
    assert!(!env.set().join("nested/Pong (USA).zip").exists());
}

#[test]
fn a_placed_file_missing_or_changed_is_reported_and_never_written_over() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    assert_eq!(
        env.build(&env.plan(&env.folder, selection)).state,
        JobState::Succeeded
    );
    let placed = env.library().join("Pong (USA).zip");

    // Changed: a conflict, left as it is.
    std::fs::write(&placed, b"something else").unwrap();
    let plan = env.plan(&env.folder, selection);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(pong.kind, ItemKind::Conflict, "{pong:?}");
    assert!(
        pong.reason
            .as_deref()
            .unwrap_or_default()
            .contains("only copy"),
        "{pong:?}"
    );
    let job = env.build(&plan);
    assert_eq!(std::fs::read(&placed).unwrap(), b"something else");
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let folder = env.store.library_folder(env.folder.id).unwrap();
    let incomplete = folder.incomplete.unwrap();
    assert_eq!(incomplete.conflict, 1, "{incomplete:?}");

    // Missing: blocked, nothing written in its place.
    std::fs::remove_file(&placed).unwrap();
    let plan = env.plan(&env.folder, selection);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(pong.kind, ItemKind::Blocked, "{pong:?}");
    assert!(
        pong.reason
            .as_deref()
            .unwrap_or_default()
            .contains("missing"),
        "{pong:?}"
    );
    let job = env.build(&plan);
    assert!(!placed.exists());
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let folder = env.store.library_folder(env.folder.id).unwrap();
    let incomplete = folder.incomplete.unwrap();
    assert_eq!(incomplete.blocked, 1, "{incomplete:?}");
}

#[test]
fn a_source_set_stays_locked_while_a_placing_build_is_queued_whatever_folder_holds_its_games() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    env.store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();

    // A second folder leaves none chosen to hold the games, and choosing it is refused while the build is queued.
    let second = add_folder(&env.store, "atari2");
    let refused = env.store.set_holding_folder(SET, Some(second.id));
    assert!(
        matches!(&refused, Err(e) if e.to_string().contains("places the source set's games")),
        "{refused:?}"
    );
    // The source set is still locked, though no folder is chosen to hold its games now.
    let audit = env
        .store
        .enqueue_audit(SET, None, "Operator", WORKERS, AuditEvidence::Incremental);
    assert!(
        matches!(&audit, Err(Error::Conflict(why)) if why.contains("places the source set's games")),
        "{audit:?}"
    );
}

#[test]
fn an_original_rewritten_before_it_is_set_aside_goes_to_the_source_sets_kept_files() {
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);

    // Written over with other bytes once its copy is recorded placed: it is never set aside to delete.
    let original = env.set().join("Pong (USA).zip");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceRecorded(_)) {
                std::fs::write(&original, b"rewritten").unwrap();
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let error = env.item(job.id, "Pong (USA)").error.unwrap();
    assert!(
        error.contains("changed after it was copied")
            && error.contains("kept in source set Alpha's kept files as Pong (USA).zip.kept"),
        "{error}"
    );
    assert_eq!(env.files(), (vec![], vec!["Pong (USA).zip".into()]));
    assert_kept_by_build(&env, job.id, Some(b"rewritten"));
}

/// Approves `repair`, then runs it and the audit it queues.
fn run_repair(env: &Env, repair: &stage_select_api::repair::RepairPlan) {
    let approved = env
        .store
        .approve_repair(repair.id, &repair.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    let ended = env.store.run_repair_job(job, None).unwrap();
    assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
    env.store.fulfil_repair_audits().unwrap();
    let audit = env.store.repair_plan(repair.id).unwrap().audit_job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, audit);
    env.store.run_audit_job(audit, None).unwrap();
}

#[test]
fn a_file_a_build_kept_outlives_its_library_folder_and_goes_with_its_source_set() {
    use stage_select_api::lifecycle::RegistrationKind;
    let env = Env::new();
    pong_zip(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    env.store.copy_across_filesystems(true);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    // The original is written over once its copy is placed, so the build keeps it.
    let original = env.set().join("Pong (USA).zip");
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::PlaceRecorded(_)) {
                overwrite_keeping_stamp(&original);
            }
        })));
    let job = env.build(&plan);
    env.store.hook_faults(None);
    let build = env.store.build_of_job(job.id).unwrap().unwrap().id;

    // The placed file is written over too, and a repair keeps it aside: nothing is placed, so the folder can be
    // unregistered, and the source set has a file each kept.
    std::fs::remove_file(env.library().join("Pong (USA).zip")).unwrap();
    write_torrentzip(
        &env.library().join("Pong (USA).zip"),
        &[("junk.bin", b"junk")],
    );
    env.audit(AuditEvidence::Full);
    let repair = env.store.plan_repair(SET, &Default::default()).unwrap();
    run_repair(&env, &repair);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    let kept = env.store.source_kept_files(SET).unwrap();
    let owners: Vec<(Option<i64>, Option<i64>)> = kept.iter().map(|k| (k.plan, k.build)).collect();
    assert_eq!(owners.len(), 2, "{kept:?}");
    assert!(owners.contains(&(None, Some(build))), "{kept:?}");
    assert!(owners.contains(&(Some(repair.id), None)), "{kept:?}");

    // Forgetting the folder forgets its builds; the file its build kept is the source set's, and stays as it was.
    env.store.unregister_library_folder(env.folder.id).unwrap();
    env.store
        .forget(RegistrationKind::LibraryFolder, env.folder.id)
        .unwrap();
    assert_eq!(env.store.source_kept_files(SET).unwrap(), kept);

    // Both are deleted when you approve it.
    let deletion = env
        .store
        .plan_source_kept_deletion(
            SET,
            &stage_select_api::build::PlanKeptDeletion {
                ids: vec![],
                all: true,
            },
        )
        .unwrap();
    assert_eq!(deletion.content.files.len(), 2, "{deletion:?}");
    env.store
        .approve_source_kept_deletion(
            deletion.id,
            deletion.content.source_set,
            &deletion.digest,
            "Operator",
        )
        .unwrap();
    assert!(env.store.source_kept_files(SET).unwrap().is_empty());
    assert!(
        private_files(&env.set())
            .iter()
            .all(|f| !f.contains("retained/")),
        "{:?}",
        private_files(&env.set())
    );

    // Then the source set is forgotten, its kept files' rows with it.
    let set = kept[0].source_set;
    env.store.unregister_source_set(SET).unwrap();
    env.store.forget(RegistrationKind::SourceSet, set).unwrap();
}
