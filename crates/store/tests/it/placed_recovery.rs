//! Placed Games recover from a change of metadata alone and from a move home by hand (ADR 0031): a rename keeps the
//! Audit's Evidence as its proof, a Placed file with none is proven against its DAT ROM, an Audit refreshes the
//! Placements it verifies and records a Placed Game found back in its Source Set's folder there, and a check points
//! the way home.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::single_copy::*;
use stage_select_api::build::{
    CheckedFile, FileOutcome, FolderCheck, ItemKind, RemoveAction, RemoveGames,
};
use stage_select_api::storage::StorageFormat;
use stage_select_api::{AuditEvidence, JobState};

impl Env {
    /// Checks the Library Folder and returns what it found.
    fn check(&self) -> FolderCheck {
        let job = self
            .store
            .enqueue_check(self.folder.id, "Operator")
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        let done = self.store.run_check_job(job.id, None).unwrap();
        assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
        self.store.latest_check(self.folder.id).unwrap().unwrap()
    }

    /// Places Pong by a rename, as the folder stores it as `format`, from the file `write` puts in the Source Set's
    /// folder, after a full Audit. Returns the placed file's path.
    fn place_pong(&self, format: StorageFormat, write: impl Fn(&Env)) -> std::path::PathBuf {
        write(self);
        self.audit(AuditEvidence::Full);
        self.single_copy(format);
        let selection = self.select(&["Pong (USA)"]);
        let plan = self.plan(&self.folder, selection);
        let pong = item(&plan, "Pong (USA)");
        assert_eq!(pong.kind, ItemKind::Place, "{pong:?}");
        assert_eq!(
            pong.place.as_ref().unwrap().how,
            stage_select_api::build::PlaceHow::Rename
        );
        assert_eq!(self.build(&plan).state, JobState::Succeeded);
        let placed = self.store.placed_files(SET).unwrap();
        assert_eq!(placed.len(), 1, "{placed:?}");
        self.library().join(&placed[0].path)
    }
}

fn pong(env: &Env) {
    pong_zip(env);
}

/// Changes `path`'s mode alone, as a `chmod` does: no byte, size or modification time changes, only its change time.
fn chmod(path: &Path) {
    let before = std::fs::metadata(path).unwrap();
    let mode = before.mode() & 0o7777;
    let mut next = mode ^ 0o040;
    loop {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(next)).unwrap();
        let after = std::fs::metadata(path).unwrap();
        if (after.ctime(), after.ctime_nsec()) != (before.ctime(), before.ctime_nsec()) {
            assert_eq!(
                (after.size(), after.mtime(), after.mtime_nsec(), after.ino()),
                (
                    before.size(),
                    before.mtime(),
                    before.mtime_nsec(),
                    before.ino()
                )
            );
            return;
        }
        // Its change time hasn't ticked yet: change it back and again.
        next ^= 0o040;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn ctime_ns(path: &Path) -> i128 {
    let st = std::fs::metadata(path).unwrap();
    st.ctime() as i128 * 1_000_000_000 + st.ctime_nsec() as i128
}

fn checked<'a>(check: &'a FolderCheck, target: &str) -> &'a CheckedFile {
    check
        .files
        .iter()
        .find(|f| f.target == target)
        .unwrap_or_else(|| panic!("no checked file {target}: {:?}", check.files))
}

#[test]
fn a_game_placed_by_a_rename_whose_mode_changed_is_refreshed_with_its_sha256_recorded() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let bytes = std::fs::read(&at).unwrap();
    chmod(&at);

    let check = env.check();

    let file = checked(&check, "Pong (USA).zip");
    assert_eq!(file.outcome, FileOutcome::Refreshed, "{file:?}");
    assert_eq!(check.summary.conflict, 0, "{:?}", check.files);
    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(&bytes).as_str()));
    assert_eq!(placed.ctime_ns, ctime_ns(&at));
    // Recorded, it is trusted unread again.
    let again = env.check();
    let file = checked(&again, "Pong (USA).zip");
    assert_eq!((file.outcome, file.hashed), (FileOutcome::Unchanged, false));
}

#[test]
fn a_placed_file_with_no_sha256_whose_bytes_differ_from_its_dat_roms_is_a_conflict() {
    let other_roms =
        |at: &Path| crate::common::write_torrentzip(at, &[("Pong (USA).a26", b"pang")]);
    // The same ROMs, in other bytes: no longer the TorrentZip they make.
    let other_bytes = |at: &Path| overwrite_keeping_stamp(at);
    for (label, change) in [
        ("other ROMs", &other_roms as &dyn Fn(&Path)),
        ("other bytes", &other_bytes),
    ] {
        let env = Env::new();
        let at = env.place_pong(StorageFormat::TorrentZip, pong);
        assert_eq!(env.store.placed_files(SET).unwrap()[0].sha256, None);
        change(&at);

        let check = env.check();

        let file = checked(&check, "Pong (USA).zip");
        assert_eq!(file.outcome, FileOutcome::Conflict, "{label}: {file:?}");
        assert!(
            file.detail
                .as_deref()
                .unwrap_or_default()
                .contains("isn't exactly the DAT's ROMs for Pong (USA)"),
            "{label}: {file:?}"
        );
        assert_eq!(env.store.placed_files(SET).unwrap()[0].sha256, None);
    }
}

#[test]
fn a_loose_game_placed_by_a_rename_keeps_the_sha256_its_audit_verified() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::Loose, |env| {
        std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
    });
    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(b"pong").as_str()));

    // Changed in bytes, it is a conflict however it was placed.
    overwrite_keeping_stamp(&at);
    let check = env.check();
    let file = checked(&check, "Pong (USA).a26");
    assert_eq!(file.outcome, FileOutcome::Conflict, "{file:?}");
}

#[test]
fn an_audit_that_reads_a_placed_file_records_its_sha256_and_stamp_in_its_placement() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let bytes = std::fs::read(&at).unwrap();
    chmod(&at);

    env.audit(AuditEvidence::Incremental);

    let placed = &env.store.placed_files(SET).unwrap()[0];
    let st = std::fs::metadata(&at).unwrap();
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(&bytes).as_str()));
    assert_eq!(
        (placed.dev, placed.ino, placed.file.size, placed.ctime_ns),
        (st.dev(), st.ino(), st.size(), ctime_ns(&at))
    );
    // The check trusts what the audit read, unread.
    let check = env.check();
    let file = checked(&check, "Pong (USA).zip");
    assert_eq!((file.outcome, file.hashed), (FileOutcome::Unchanged, false));
}

#[test]
fn an_audit_that_finds_a_placed_file_changed_leaves_its_placement_as_it_was() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let before = env.store.placed_files(SET).unwrap();
    crate::common::write_torrentzip(&at, &[("Pong (USA).a26", b"pang")]);

    env.audit(AuditEvidence::Incremental);

    assert_eq!(env.store.placed_files(SET).unwrap(), before);
}

#[test]
fn a_placed_game_moved_home_by_hand_is_found_there_recorded_home_by_an_audit_and_placed_again() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let home = env.set().join("Pong (USA).zip");
    std::fs::rename(&at, &home).unwrap();

    // The check points the way home rather than reporting the only copy missing.
    let check = env.check();
    let file = checked(&check, "Pong (USA).zip");
    assert_eq!(file.outcome, FileOutcome::Missing, "{file:?}");
    assert_eq!(
        file.detail.as_deref(),
        Some("found in its source set; audit it to bring it home")
    );
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    let blocked = item(&plan, "Pong (USA)");
    assert!(
        blocked
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("found in its source set; audit it to bring it home"),
        "{blocked:?}"
    );

    // The audit records it home: the store changes, not the disk.
    env.audit(AuditEvidence::Incremental);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(env.files(), (vec!["Pong (USA).zip".into()], Vec::new()));

    // The folder sees it as not yet placed, and its next build places it again.
    let plan = env.plan(&env.folder, selection);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(pong.kind, ItemKind::Place, "{pong:?}");
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(env.files(), (Vec::new(), vec!["Pong (USA).zip".into()]));
    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
}

#[test]
fn a_placed_game_still_in_its_folder_stays_placed_though_a_copy_of_it_is_in_its_source_set() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    std::fs::copy(&at, env.set().join("Pong (USA).zip")).unwrap();

    env.audit(AuditEvidence::Incremental);

    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
}

#[test]
fn a_placed_game_whose_mode_changed_is_returned_home_once_deselected() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let bytes = std::fs::read(&at).unwrap();
    chmod(&at);
    let selection = env.select(&["Combat (USA)"]);

    let plan = env.plan(&env.folder, selection);

    assert_eq!(plan.content.returns.len(), 1, "{:?}", plan.content);
    assert_eq!(plan.content.returns[0].games, ["Pong (USA)"]);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        bytes
    );
}

#[test]
fn a_placed_game_whose_mode_changed_can_be_sent_back() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let bytes = std::fs::read(&at).unwrap();
    chmod(&at);

    let plan = env
        .store
        .plan_game_removal(
            env.folder.id,
            &RemoveGames {
                games: vec!["Pong (USA)".into()],
                action: RemoveAction::SendBack,
            },
        )
        .unwrap();

    assert_eq!(plan.content.returns.len(), 1, "{:?}", plan.content);
    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert!(env.store.placed_files(SET).unwrap().is_empty());
    assert_eq!(
        std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
        bytes
    );
}

#[test]
fn a_store_whose_placed_files_have_no_sha256_opens_with_them_filled_in_from_their_audits_evidence()
{
    let env = Env::new();
    let at = env.place_pong(StorageFormat::Loose, |env| {
        std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
    });
    // As a store made by 0.13.3 has them: placed by a rename, with no SHA-256, and their mode changed since.
    let db =
        rusqlite::Connection::open(env.store.dir().join(stage_select_store::DATABASE)).unwrap();
    db.execute_batch("UPDATE placed_files SET sha256 = NULL; PRAGMA user_version = 8;")
        .unwrap();
    drop(db);
    chmod(&at);

    let (env, _) = env.restart();

    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(b"pong").as_str()));
    let check = env.check();
    let file = checked(&check, "Pong (USA).a26");
    assert_eq!(file.outcome, FileOutcome::Refreshed, "{file:?}");
}
