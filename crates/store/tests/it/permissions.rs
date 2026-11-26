//! The Permission Policy (ADR 0031): what a Repair and an ingest move write is given it, a stat walk reports what
//! differs from it apart from Stale, a plan restores what Stage Select owns, and what it can't restore is listed with
//! the command to run on the Host. A Build's own case is in `builds`.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::common::*;
use stage_select_api::permissions::{
    Mode, PermissionCause, PermissionPolicy, PermissionSettings, PermissionsView,
};
use stage_select_api::repair::{PlanRepair, RepairKind};
use stage_select_api::roots::{RootKind, RootSpec};
use stage_select_api::settings::SettingsUpdate;
use stage_select_api::{AuditEvidence, JobState};
use stage_select_store::{Error, Store, Workers};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// The policy the tests declare: files `0660`, folders `2770`, and the test's own primary group, which Stage Select,
/// running as the test's user, is in.
pub(crate) fn policy() -> PermissionPolicy {
    PermissionPolicy {
        file_mode: Mode(0o660),
        folder_mode: Mode(0o2770),
        group: Some(rustix::process::getegid().as_raw()),
    }
}

/// Declares [`policy`] for every Root.
pub(crate) fn declare(store: &Store) {
    store
        .update_settings(
            &SettingsUpdate {
                permissions: Some(PermissionSettings {
                    global: Some(policy()),
                    roots: Default::default(),
                }),
                ..Default::default()
            },
            "Operator",
        )
        .unwrap();
}

/// The mode, owner's group, and whether a folder, of `path`.
fn stat(path: &Path) -> (u32, u32, bool) {
    use std::os::unix::fs::MetadataExt;
    let st = std::fs::symlink_metadata(path).unwrap();
    (st.mode() & 0o7777, st.gid(), st.is_dir())
}

/// Asserts each of `paths` is as [`policy`] declares.
pub(crate) fn assert_declared(paths: &[PathBuf]) {
    let policy = policy();
    for path in paths {
        let (mode, gid, folder) = stat(path);
        assert_eq!(
            (mode, Some(gid)),
            (policy.mode(folder), policy.group),
            "{} is {mode:o}",
            path.display()
        );
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]),
    ]
}

/// A store with the Source Set Alpha in the Root `sets`, which lets Stage Select change it, and Gamma, of the same
/// DAT, in the Root `ro`, which doesn't. `in` is an Ingest Folder whose items settle at once.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/Alpha", "roots/ro/Gamma", "in", "dats"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", tmp.path().join("roots/sets")),
                RootSpec::new("ro", tmp.path().join("roots/ro")),
                RootSpec::new("in", tmp.path().join("in")).of_kind(RootKind::Ingest),
            ],
        );
        store
            .update_settings(
                &SettingsUpdate {
                    ingest_folders: Some(
                        [(
                            "in".to_owned(),
                            stage_select_api::settings::IngestCadence {
                                settle_secs: 0,
                                scan_every_minutes: None,
                            },
                        )]
                        .into(),
                    ),
                    ..Default::default()
                },
                "Operator",
            )
            .unwrap();
        let dat = write_dat_from(
            &tmp.path().join("dats"),
            "alpha.dat",
            "1",
            "No-Intro",
            &games(),
        );
        store.import_dat(&dat, None).unwrap();
        store
            .add_source_set("sets", "Alpha", "No-Intro/Test - System")
            .unwrap();
        crate::let_change(&store, "Alpha");
        store
            .add_source_set("ro", "Gamma", "No-Intro/Test - System")
            .unwrap();
        store.register_ingest_folder("in").unwrap();
        Env { tmp, store }
    }

    fn set(&self, name: &str) -> PathBuf {
        match name {
            "Alpha" => self.tmp.path().join("roots/sets/Alpha"),
            _ => self.tmp.path().join("roots/ro/Gamma"),
        }
    }

    fn audit(&self, name: &str) {
        self.store
            .audit_source_set(name, None, AuditEvidence::Full)
            .unwrap();
    }

    /// Queues a stat walk of `name`, then claims and runs it.
    fn walk(&self, name: &str) -> PermissionsView {
        let job = self.store.enqueue_stat_walk(name, "Operator").unwrap();
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        let ended = self.store.run_stat_walk_job(job.id).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.store.source_set_permissions(name).unwrap()
    }

    /// Puts `name` in the Ingest Folder and scans it twice, so it settles and is assessed.
    fn ingest(&self, name: &str, bytes: &[u8]) {
        write_file(&self.tmp.path().join("in").join(name), bytes);
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self.store.run_ingest_scan_job(job.id).unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
    }
}

/// The paths, in `view`'s latest report, of the differences of `cause`.
fn of_cause(view: &PermissionsView, cause: PermissionCause) -> Vec<String> {
    let report = view.report.as_ref().expect("a report");
    report
        .differences
        .iter()
        .filter(|d| d.cause == cause)
        .map(|d| d.path.clone())
        .collect()
}

#[test]
fn a_repairs_renamed_game_and_an_ingest_moves_filled_game_come_out_as_declared() {
    let env = Env::new();
    declare(&env.store);
    write_file(&env.set("Alpha").join("Pong (Anthology).a26"), b"pong");
    env.audit("Alpha");
    env.ingest("combat.a26", b"combat");

    let plan = env
        .store
        .plan_repair("Alpha", &PlanRepair::default())
        .unwrap();
    let kinds: Vec<_> = plan
        .content
        .items
        .iter()
        .map(|i| (i.key.as_str(), i.kind))
        .collect();
    assert!(
        kinds.contains(&("game:Pong (USA)", RepairKind::Rename)),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&("game:Combat (USA)", RepairKind::Fill)),
        "{kinds:?}"
    );
    let approved = env
        .store
        .approve_repair(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    let job = approved.job.unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job);
    assert_eq!(
        env.store.run_repair_job(job, None).unwrap().state,
        JobState::Succeeded
    );

    let set = env.set("Alpha");
    assert_declared(&[set.join("Pong (USA).zip"), set.join("Combat (USA).zip")]);
}

#[test]
fn a_stat_walk_reports_what_differs_a_plan_restores_what_stage_select_owns_and_the_rest_comes_with_the_fix()
 {
    let env = Env::new();
    let set = env.set("Alpha");
    write_file(&set.join("Pong (USA).a26"), b"pong");
    write_file(&set.join("Combat (USA).a26"), b"combat");
    std::fs::create_dir(set.join("extras")).unwrap();
    write_file(&set.join("extras/readme.txt"), b"hi");
    set_mode(&set, 0o2770);
    for file in ["Pong (USA).a26", "Combat (USA).a26", "extras/readme.txt"] {
        set_mode(&set.join(file), 0o660);
    }
    set_mode(&set.join("extras"), 0o755);
    set_mode(&set.join("Pong (USA).a26"), 0o644);
    // Another user's: Stage Select can't change its mode.
    set_mode(&set.join("Combat (USA).a26"), 0o604);
    let (uid, gid) = {
        use std::os::unix::fs::MetadataExt;
        let st = std::fs::metadata(&set).unwrap();
        (st.uid(), st.gid())
    };
    stage_select_store::pretend_owner(&set.join("Combat (USA).a26"), Some((uid + 1, gid))).unwrap();
    env.audit("Alpha");
    declare(&env.store);

    let view = env.walk("Alpha");

    assert_eq!(view.policy, Some(policy()));
    let report = view.report.clone().unwrap();
    assert!(report.current);
    assert_eq!(
        of_cause(&view, PermissionCause::Restorable),
        ["Pong (USA).a26", "extras"]
    );
    assert_eq!(
        of_cause(&view, PermissionCause::OtherOwner),
        ["Combat (USA).a26"]
    );
    assert_eq!(report.fixes.len(), 1);
    let fix = &report.fixes[0];
    assert_eq!(fix.cause, PermissionCause::OtherOwner);
    let quoted = format!("'{}'", set.display());
    assert_eq!(
        fix.command,
        format!("chown -R {} {quoted}", rustix::process::geteuid().as_raw())
    );
    assert!(
        fix.explanation.contains("run this on the host as root")
            && fix
                .explanation
                .contains("the path is as Stage Select sees it"),
        "{}",
        fix.explanation
    );

    // A stale digest restores nothing.
    let refused = env
        .store
        .restore_source_set_permissions("Alpha", "not the plan", "Operator");
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    assert_eq!(stat(&set.join("Pong (USA).a26")).0, 0o644);

    let restored = env
        .store
        .restore_source_set_permissions("Alpha", report.plan_digest.as_deref().unwrap(), "Operator")
        .unwrap();

    assert_eq!(restored.restored, 2, "{restored:?}");
    assert!(restored.failed.is_empty(), "{restored:?}");
    assert_declared(&[set.join("Pong (USA).a26"), set.join("extras")]);
    assert_eq!(
        stat(&set.join("Combat (USA).a26")).0,
        0o604,
        "another user's, left as it is"
    );
    let after = env.store.source_set_permissions("Alpha").unwrap();
    assert_eq!(
        of_cause(&after, PermissionCause::Restorable),
        Vec::<String>::new()
    );

    // The Operator runs the command on the Host; the next check finds it Stage Select's to restore, and then none.
    let ran = std::process::Command::new("sh")
        .arg("-c")
        .arg(&fix.command)
        .status()
        .unwrap();
    assert!(ran.success());
    stage_select_store::pretend_owner(&set.join("Combat (USA).a26"), None).unwrap();
    let view = env.walk("Alpha");
    assert!(view.report.as_ref().unwrap().fixes.is_empty());
    // A `chown` may clear the folders' setgid bits too, as macOS's does: those are Stage Select's to restore as well.
    let restorable = of_cause(&view, PermissionCause::Restorable);
    assert!(
        restorable.contains(&"Combat (USA).a26".to_owned())
            && restorable
                .iter()
                .all(|p| ["", "extras", "Combat (USA).a26"].contains(&p.as_str())),
        "{restorable:?}"
    );
    env.store
        .restore_source_set_permissions(
            "Alpha",
            view.report.unwrap().plan_digest.as_deref().unwrap(),
            "Operator",
        )
        .unwrap();
    // The restore moved change times, which the stat walk compares until #374; audit to count them as unchanged.
    env.audit("Alpha");
    let view = env.walk("Alpha");
    assert_eq!(view.differing(), 0);
    assert!(view.report.unwrap().differences.is_empty());
}

#[test]
fn a_read_only_source_sets_differences_are_reported_and_never_planned() {
    let env = Env::new();
    let set = env.set("Gamma");
    write_file(&set.join("Pong (USA).a26"), b"pong");
    set_mode(&set.join("Pong (USA).a26"), 0o644);
    env.audit("Gamma");
    declare(&env.store);

    let view = env.walk("Gamma");

    let report = view.report.clone().unwrap();
    assert!(of_cause(&view, PermissionCause::ReadOnly).contains(&"Pong (USA).a26".to_owned()));
    assert!(of_cause(&view, PermissionCause::Restorable).is_empty());
    assert_eq!(report.plan_digest, None, "never in a plan");
    let fix = &report.fixes[0];
    assert_eq!(fix.cause, PermissionCause::ReadOnly);
    assert!(
        fix.command.contains("-type f -exec chmod 0660"),
        "{}",
        fix.command
    );
    assert!(
        fix.explanation.contains("may not change this source set"),
        "{}",
        fix.explanation
    );
    let refused = env
        .store
        .restore_source_set_permissions("Gamma", "anything", "Operator");
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    assert_eq!(stat(&set.join("Pong (USA).a26")).0, 0o644);
}

#[test]
fn a_symlinked_file_is_never_compared_since_a_restore_never_follows_a_link() {
    let env = Env::new();
    let set = env.set("Alpha");
    write_file(&set.join("Pong (USA).a26"), b"pong");
    set_mode(&set.join("Pong (USA).a26"), 0o660);
    let outside = env.tmp.path().join("outside.a26");
    write_file(&outside, b"elsewhere");
    set_mode(&outside, 0o644);
    std::os::unix::fs::symlink(&outside, set.join("Linked (USA).a26")).unwrap();
    env.audit("Alpha");
    declare(&env.store);

    let view = env.walk("Alpha");

    let paths: Vec<_> = view
        .report
        .unwrap()
        .differences
        .into_iter()
        .map(|d| d.path)
        .collect();
    assert!(!paths.contains(&"Linked (USA).a26".to_owned()), "{paths:?}");
    assert_eq!(
        stat(&outside).0,
        0o644,
        "what a link points at is never changed"
    );
}

#[test]
fn a_permissions_finding_never_makes_a_source_set_stale() {
    let env = Env::new();
    let set = env.set("Alpha");
    write_file(&set.join("Pong (USA).a26"), b"pong");
    set_mode(&set.join("Pong (USA).a26"), 0o604);
    env.audit("Alpha");
    declare(&env.store);

    let view = env.walk("Alpha");

    assert!(view.differing() > 0);
    assert!(env.store.source_set("Alpha").unwrap().stale.is_empty());
    assert!(env.store.stat_walks("Alpha").unwrap().is_empty());
}

#[test]
fn without_a_policy_nothing_is_compared_or_reported() {
    let env = Env::new();
    write_file(&env.set("Alpha").join("Pong (USA).a26"), b"pong");
    set_mode(&env.set("Alpha").join("Pong (USA).a26"), 0o604);
    env.audit("Alpha");

    let view = env.walk("Alpha");

    assert_eq!(view, PermissionsView::default());
}

#[test]
fn a_policy_stage_select_would_lock_itself_out_with_or_for_no_root_is_refused() {
    let env = Env::new();
    let with = |p: PermissionPolicy, root: Option<&str>| {
        env.store.update_settings(
            &SettingsUpdate {
                permissions: Some(match root {
                    None => PermissionSettings {
                        global: Some(p),
                        roots: Default::default(),
                    },
                    Some(root) => PermissionSettings {
                        global: None,
                        roots: [(root.to_owned(), p)].into(),
                    },
                }),
                ..Default::default()
            },
            "Operator",
        )
    };
    let base = policy();
    for bad in [
        PermissionPolicy {
            file_mode: Mode(0o440),
            ..base
        },
        PermissionPolicy {
            folder_mode: Mode(0o2570),
            ..base
        },
        PermissionPolicy {
            file_mode: Mode(0o2660),
            ..base
        },
    ] {
        assert!(matches!(with(bad, None), Err(Error::Refused(_))), "{bad:?}");
    }
    assert!(matches!(
        with(base, Some("nowhere")),
        Err(Error::Refused(_))
    ));
    let own = PermissionPolicy {
        file_mode: Mode(0o640),
        ..base
    };
    with(base, None).unwrap();
    with(own, Some("sets")).unwrap();
    let settings = env.store.settings_view().unwrap();
    assert_eq!(settings.permissions.of("sets"), Some(own));
    assert_eq!(
        env.store.roots().root("sets").unwrap().permissions,
        Some(own),
        "in force at once"
    );
}

#[test]
fn a_game_placed_by_a_copy_where_its_rename_fails_across_mounts_comes_out_as_declared() {
    use crate::single_copy as sc;
    use stage_select_api::build::{ItemState, PlaceHow};
    use stage_select_api::storage::StorageFormat;
    let env = sc::Env::new();
    let bytes = sc::pong_zip(&env);
    set_mode(&env.set().join("Pong (USA).zip"), 0o644);
    env.audit(AuditEvidence::Full);
    env.single_copy(StorageFormat::TorrentZip);
    declare(&env.store);
    let plan = env.plan(&env.folder, env.select(&["Pong (USA)"]));
    assert_eq!(
        sc::item(&plan, "Pong (USA)").place.as_ref().unwrap().how,
        PlaceHow::Rename
    );
    // Two bind mounts of one filesystem: the same device, and still no rename between them.
    stage_select_store::pretend_mount(&env.library(), true);

    let job = env.build(&plan);

    stage_select_store::pretend_mount(&env.library(), false);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let pong = env.item(job.id, "Pong (USA)");
    assert_eq!(pong.state, ItemState::Applied, "{pong:?}");
    let placed = env.library().join("Pong (USA).zip");
    assert_eq!(std::fs::read(&placed).unwrap(), bytes);
    assert_declared(&[placed]);
}
