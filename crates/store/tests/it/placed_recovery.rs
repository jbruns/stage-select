//! Placed Games recover from a change of metadata alone and from a move home by hand (ADR 0031): a rename keeps the
//! Audit's Evidence as its proof, a Placed file with none is proven against its DAT ROM, an Audit refreshes the
//! Placements it verifies and records a Placed Game found back in its Source Set's folder there, and a check points
//! the way home.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::common::{
    RomSpec, optional_rom, rom, rom_size_only, rom_without_sha, write_dat_from, write_torrentzip,
    write_torrentzip_deflated_at,
};
use crate::single_copy::*;
use stage_select_api::build::{
    CheckedFile, FileOutcome, FolderCheck, ItemKind, Plan, RemoveAction, RemoveGames,
};
use stage_select_api::storage::StorageFormat;
use stage_select_api::{AuditEvidence, JobState};
use stage_select_store::Fault;

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

    /// Runs an Audit of the Source Set as a Job, calling `last` once it has read every file, before it finishes.
    fn audit_then(&self, evidence: AuditEvidence, last: impl Fn() + Sync) {
        let job = self
            .store
            .enqueue_audit(SET, None, "Operator", WORKERS, evidence)
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        let progress = |p: stage_select_engine::Progress| {
            if p.files_total > 0 && p.files_done == p.files_total {
                last();
            }
        };
        let done = self.store.run_audit_job(job.id, Some(&progress)).unwrap();
        assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
    }

    /// Plans Pong home as `how` says: deselected, sent back, or with its folder unregistered.
    fn home_plan(&self, how: Home) -> Plan {
        match how {
            Home::Deselected => {
                let selection = self.select(&["Combat (USA)"]);
                self.plan(&self.folder, selection)
            }
            Home::SentBack => self
                .store
                .plan_game_removal(
                    self.folder.id,
                    &RemoveGames {
                        games: vec!["Pong (USA)".into()],
                        action: RemoveAction::SendBack,
                    },
                )
                .unwrap(),
            Home::Unregistered => self.store.plan_library_removal(self.folder.id).unwrap(),
        }
    }
}

/// How a placed game goes home.
#[derive(Debug, Clone, Copy)]
enum Home {
    Deselected,
    SentBack,
    Unregistered,
}

const HOMES: [Home; 3] = [Home::Deselected, Home::SentBack, Home::Unregistered];

/// A DAT of Pong, as `pong` lists it, and Combat.
fn games_with(pong: Vec<RomSpec>) -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", pong),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
    ]
}

fn loose_pong(env: &Env) {
    std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
}

fn pong(env: &Env) {
    pong_zip(env);
}

/// Changes `path`'s mode alone, as a `chmod` does: no byte, size or modification time changes, only its change time.
pub(crate) fn chmod(path: &Path) {
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
                .contains("isn't exactly the ROMs Pong (USA) was placed with"),
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
        Some(
            "a file of the same name is in your source set; audit it to see whether it holds this game"
        )
    );
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    let blocked = item(&plan, "Pong (USA)");
    assert!(
        blocked
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("a file of the same name is in your source set; audit it to see whether it holds this game"),
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

#[test]
fn an_audit_that_reads_a_loose_placed_file_records_the_sha256_it_verified() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::Loose, |env| {
        std::fs::write(env.set().join("Pong (USA).a26"), b"pong").unwrap();
    });
    chmod(&at);

    env.audit(AuditEvidence::Incremental);

    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(b"pong").as_str()));
    assert_eq!(placed.ctime_ns, ctime_ns(&at));
    let check = env.check();
    let file = checked(&check, "Pong (USA).a26");
    assert_eq!((file.outcome, file.hashed), (FileOutcome::Unchanged, false));
}

#[test]
fn a_store_whose_placed_file_its_audit_found_changed_opens_with_no_sha256_for_it() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::Loose, loose_pong);
    // Other bytes under the same size and modification time: the audit reads it again, and finds it changed.
    overwrite_keeping_stamp(&at);
    env.audit(AuditEvidence::Incremental);
    let db =
        rusqlite::Connection::open(env.store.dir().join(stage_select_store::DATABASE)).unwrap();
    db.execute_batch("UPDATE placed_files SET sha256 = NULL; PRAGMA user_version = 8;")
        .unwrap();
    drop(db);

    let (env, _) = env.restart();

    assert_eq!(env.store.placed_files(SET).unwrap()[0].sha256, None);
    let check = env.check();
    let file = checked(&check, "Pong (USA).a26");
    assert_eq!(file.outcome, FileOutcome::Conflict, "{file:?}");
}

#[test]
fn a_placed_zip_with_no_sha256_holding_its_roms_in_another_deflate_stream_is_refreshed() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    // TorrentZip's structure and the same ROMs, deflated otherwise: the ROMs it was placed with, which prove it.
    write_torrentzip_deflated_at(&at, &[("Pong (USA).a26", b"pong")], 0);
    let bytes = std::fs::read(&at).unwrap();

    let check = env.check();

    // Its bytes changed, and it holds the ROMs it was built with.
    let file = checked(&check, "Pong (USA).zip");
    assert_eq!(file.outcome, FileOutcome::Repacked, "{file:?}");
    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(&bytes).as_str()));
}

/// Pong's ZIP as another TorrentZip tool may write it: TorrentZip's structure, with another deflate stream.
fn pong_deflated_otherwise(env: &Env) {
    write_torrentzip_deflated_at(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
        0,
    );
}

#[test]
fn a_zip_another_tool_wrote_placed_by_a_rename_recovers_from_a_mode_change_on_its_first_check_or_audit()
 {
    for audit in [false, true] {
        let env = Env::new();
        let at = env.place_pong(StorageFormat::TorrentZip, pong_deflated_otherwise);
        let bytes = std::fs::read(&at).unwrap();
        assert_eq!(env.store.placed_files(SET).unwrap()[0].sha256, None);
        chmod(&at);

        if audit {
            env.audit(AuditEvidence::Incremental);
        } else {
            let check = env.check();
            let file = checked(&check, "Pong (USA).zip");
            assert_eq!(file.outcome, FileOutcome::Refreshed, "{file:?}");
        }

        let placed = &env.store.placed_files(SET).unwrap()[0];
        assert_eq!(
            placed.sha256.as_deref(),
            Some(sha256_hex(&bytes).as_str()),
            "audit: {audit}"
        );
        assert_eq!(placed.ctime_ns, ctime_ns(&at), "audit: {audit}");
        let check = env.check();
        let file = checked(&check, "Pong (USA).zip");
        assert_eq!(
            (file.outcome, file.hashed),
            (FileOutcome::Unchanged, false),
            "audit: {audit}"
        );
    }
}

/// An iNES header: what the Header Rule `No-Intro_NES.xml` finds and the DAT's checksums leave out.
fn ines() -> Vec<u8> {
    let mut h = b"NES\x1a".to_vec();
    h.resize(16, 0x01);
    h
}

fn headered_env() -> Env {
    Env::with_header(
        "No-Intro_NES.xml",
        &games_with(vec![rom("Pong (USA).a26", b"pong")]),
    )
}

#[test]
fn a_headered_zip_placed_by_a_rename_whose_mode_changed_is_proven_by_its_payload() {
    let env = headered_env();
    let at = env.place_pong(StorageFormat::TorrentZip, |env| {
        write_torrentzip(
            &env.set().join("Pong (USA).zip"),
            &[("Pong (USA).a26", &[ines(), b"pong".to_vec()].concat())],
        )
    });
    let bytes = std::fs::read(&at).unwrap();
    chmod(&at);

    let check = env.check();

    let file = checked(&check, "Pong (USA).zip");
    assert_eq!(file.outcome, FileOutcome::Refreshed, "{file:?}");
    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(&bytes).as_str()));
}

/// The Operator's choice for a Placed ZIP with no SHA-256: a member's Header is accepted as an Audit accepts it in a
/// Source Set, unread by the proof, so a Header edited by hand, such as its mapper bits, isn't detected.
#[test]
fn a_placed_zip_with_no_sha256_whose_header_was_edited_is_still_proven_by_its_payload() {
    let env = headered_env();
    let at = env.place_pong(StorageFormat::TorrentZip, |env| {
        write_torrentzip(
            &env.set().join("Pong (USA).zip"),
            &[("Pong (USA).a26", &[ines(), b"pong".to_vec()].concat())],
        )
    });
    let mut edited = ines();
    edited[6] ^= 0xf0;
    write_torrentzip(
        &at,
        &[("Pong (USA).a26", &[edited, b"pong".to_vec()].concat())],
    );

    let check = env.check();

    let file = checked(&check, "Pong (USA).zip");
    assert_eq!(file.outcome, FileOutcome::Repacked, "{file:?}");
    assert!(env.store.placed_files(SET).unwrap()[0].sha256.is_some());
}

/// A headered loose file isn't the ROM whole, so a Build converts it rather than renaming it, and records its
/// SHA-256: a changed stamp is proven by that, never by its payload.
#[test]
fn a_headered_loose_game_is_converted_as_it_is_placed_and_keeps_its_sha256() {
    let env = headered_env();
    let headered = [ines(), b"pong".to_vec()].concat();
    std::fs::write(env.set().join("Pong (USA).a26"), &headered).unwrap();
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    let pong = item(&plan, "Pong (USA)");
    assert_eq!(
        pong.place.as_ref().unwrap().how,
        stage_select_api::build::PlaceHow::Convert
    );
    assert_eq!(env.build(&plan).state, JobState::Succeeded);

    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(
        placed.sha256.as_deref(),
        Some(sha256_hex(&headered).as_str())
    );
}

#[test]
fn a_placed_file_holding_a_newer_dat_revisions_rom_is_a_conflict_after_an_audit_against_it() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let newer = write_dat_from(
        &env.tmp.path().join("dats"),
        "alpha2.dat",
        "2",
        "No-Intro",
        &games_with(vec![rom("Pong (USA).a26", b"pang")]),
    );
    let newer = env.store.import_dat(&newer, None).unwrap().0.sha256;
    // The newer revision's Pong, where the folder's build placed the older's.
    write_torrentzip(&at, &[("Pong (USA).a26", b"pang")]);

    env.store
        .audit_source_set(SET, Some(&newer), AuditEvidence::Incremental)
        .unwrap();

    assert_eq!(env.store.placed_files(SET).unwrap()[0].sha256, None);
    let check = env.check();
    let file = checked(&check, "Pong (USA).zip");
    assert_eq!(file.outcome, FileOutcome::Conflict, "{file:?}");
}

#[test]
fn a_placed_file_whose_dat_rom_has_no_sha1_or_sha256_is_a_conflict_once_its_mode_changed() {
    for (label, pong_rom) in [
        ("CRC-32", rom_without_sha("Pong (USA).a26", b"pong")),
        ("size", rom_size_only("Pong (USA).a26", b"pong")),
    ] {
        let env = Env::with(&games_with(vec![pong_rom]));
        let at = env.place_pong(StorageFormat::TorrentZip, pong);
        chmod(&at);

        let check = env.check();

        let file = checked(&check, "Pong (USA).zip");
        assert_eq!(file.outcome, FileOutcome::Conflict, "{label}: {file:?}");
    }
}

#[test]
fn a_rename_recovered_after_a_stop_records_no_sha256_it_cant_vouch_for() {
    let env = Env::new();
    loose_pong(&env);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::Loose);
    let selection = env.select(&["Pong (USA)"]);
    let plan = env.plan(&env.folder, selection);
    assert_eq!(item(&plan, "Pong (USA)").kind, ItemKind::Place);
    env.build_until(&plan, |f| matches!(f, Fault::PlaceMoved(_)));

    let (env, _) = env.restart();

    let placed = env.store.placed_files(SET).unwrap();
    assert_eq!(placed.len(), 1, "{placed:?}");
    assert_eq!(placed[0].sha256, None);
}

#[test]
fn a_placed_game_whose_optional_rom_isnt_home_stays_placed() {
    let env = Env::with(&games_with(vec![
        rom("Pong (USA).a26", b"pong"),
        optional_rom("Pong (USA).txt", b"manual"),
    ]));
    let at = env.place_pong(StorageFormat::TorrentZip, |env| {
        write_torrentzip(
            &env.set().join("Pong (USA).zip"),
            &[("Pong (USA).a26", b"pong"), ("Pong (USA).txt", b"manual")],
        );
    });
    // Its own ROM is home, so the game is complete there, but not the optional one its placed file held.
    std::fs::remove_file(&at).unwrap();
    write_torrentzip(
        &env.set().join("Pong (USA).zip"),
        &[("Pong (USA).a26", b"pong")],
    );

    env.audit(AuditEvidence::Incremental);

    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
}

#[test]
fn a_placed_game_lacking_an_optional_rom_whose_mode_changed_goes_home_every_way() {
    for how in HOMES {
        let env = Env::with(&games_with(vec![
            rom("Pong (USA).a26", b"pong"),
            optional_rom("Pong (USA).txt", b"manual"),
        ]));
        let at = env.place_pong(StorageFormat::TorrentZip, pong);
        let bytes = std::fs::read(&at).unwrap();
        chmod(&at);

        let plan = env.home_plan(how);

        assert_eq!(plan.content.returns.len(), 1, "{how:?}: {:?}", plan.content);
        assert_eq!(env.build(&plan).state, JobState::Succeeded, "{how:?}");
        assert!(env.store.placed_files(SET).unwrap().is_empty(), "{how:?}");
        assert_eq!(
            std::fs::read(env.set().join("Pong (USA).zip")).unwrap(),
            bytes,
            "{how:?}"
        );
    }
}

#[test]
fn a_placed_game_put_back_in_its_folder_while_its_audit_ran_stays_placed() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let home = env.set().join("Pong (USA).zip");
    std::fs::rename(&at, &home).unwrap();

    env.audit_then(AuditEvidence::Incremental, || {
        std::fs::copy(&home, &at).unwrap();
    });

    assert_eq!(env.store.placed_files(SET).unwrap().len(), 1);
}

#[test]
fn an_audit_records_no_placement_for_a_loose_file_changed_since_it_read_it() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::Loose, loose_pong);
    chmod(&at);
    let before = env.store.placed_files(SET).unwrap();

    env.audit_then(AuditEvidence::Incremental, || overwrite_keeping_stamp(&at));

    assert_eq!(env.store.placed_files(SET).unwrap(), before);
}

#[test]
fn a_full_audit_of_an_unchanged_placed_zip_records_its_sha256() {
    let env = Env::new();
    let at = env.place_pong(StorageFormat::TorrentZip, pong);
    let bytes = std::fs::read(&at).unwrap();
    let before = env.store.placed_files(SET).unwrap();
    assert_eq!(before[0].sha256, None);

    env.audit(AuditEvidence::Full);

    let placed = &env.store.placed_files(SET).unwrap()[0];
    assert_eq!(placed.sha256.as_deref(), Some(sha256_hex(&bytes).as_str()));
    assert_eq!(
        (placed.dev, placed.ino, placed.ctime_ns),
        (before[0].dev, before[0].ino, before[0].ctime_ns)
    );
}
