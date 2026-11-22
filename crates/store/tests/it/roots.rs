//! Registering, editing and unregistering Roots in the store, and the rules each change keeps.

use std::path::PathBuf;

use title_select_api::roots::{RootKind, RootSpec};
use title_select_store::{Error, Store};

struct Env {
    tmp: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        Env { tmp, store }
    }

    /// A folder `name` in the test's directory, made if it is missing.
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn register(&self, spec: &RootSpec) -> Result<(), Error> {
        self.store.register_root(spec, "Operator").map(|_| ())
    }

    fn refused(&self, spec: &RootSpec) -> String {
        match self.register(spec) {
            Err(Error::Refused(why)) => why,
            other => panic!("{spec:?}: {other:?}"),
        }
    }
}

#[test]
fn a_root_is_registered_edited_and_unregistered_with_who_did_each() {
    let env = Env::new();
    env.store
        .register_root(&RootSpec::new("sets", env.dir("sets")), "Operator")
        .unwrap();
    let root = &env.store.roots_view()[0];
    assert_eq!(
        (root.name.as_str(), root.updated_by.as_str()),
        ("sets", "Operator")
    );

    let edited = env
        .store
        .edit_root(
            "sets",
            &RootSpec {
                readers: Some(3),
                ..RootSpec::new("sets", env.dir("sets"))
            },
            "command line",
        )
        .unwrap();
    assert_eq!(edited.root.readers, Some(3));
    assert_eq!(edited.root.updated_by, "command line");
    assert!(edited.voided.is_empty());

    env.store.unregister_root("sets", "Operator").unwrap();
    assert!(env.store.roots_view().is_empty());
    let changes = env.store.root_changes("sets").unwrap();
    let seen: Vec<(&str, &str)> = changes
        .iter()
        .map(|c| (c.action.as_str(), c.changed_by.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("registered", "Operator"),
            ("edited", "command line"),
            ("unregistered", "Operator")
        ]
    );
}

#[test]
fn a_root_whose_folder_is_missing_is_refused() {
    let env = Env::new();
    let why = env.refused(&RootSpec::new("sets", env.tmp.path().join("missing")));
    assert!(why.contains("doesn't exist"), "{why}");
    assert!(env.store.roots().is_empty());
}

#[test]
fn a_root_declared_writable_that_cant_be_written_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let dir = env.dir("locked");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if rustix::fs::access(&dir, rustix::fs::Access::WRITE_OK).is_ok() {
        eprintln!("skipped: this user writes to read-only folders");
        return;
    }
    let why = env.refused(&RootSpec::new("play", &dir).writable());
    assert!(why.contains("can't write"), "{why}");
    // Read-only, the same folder is fine.
    env.register(&RootSpec::new("play", &dir)).unwrap();
}

#[test]
fn a_second_additions_root_is_refused() {
    let env = Env::new();
    env.register(&RootSpec::new("additions", env.dir("a")).of_kind(RootKind::Additions))
        .unwrap();
    let second = RootSpec::new("more", env.dir("b")).of_kind(RootKind::Additions);
    let why = env.refused(&second);
    assert!(why.contains("only one"), "{why}");

    // Nor can another root become one by an edit.
    env.register(&RootSpec::new("more", env.dir("b")).writable())
        .unwrap();
    let edited = env.store.edit_root("more", &second, "Operator");
    assert!(
        matches!(&edited, Err(Error::Refused(why)) if why.contains("only one")),
        "{edited:?}"
    );
}

#[test]
fn each_kind_keeps_its_writability_and_a_root_needs_a_name() {
    let env = Env::new();
    let dir = env.dir("r");
    for (spec, says) in [
        (
            RootSpec {
                kind: RootKind::Ingest,
                ..RootSpec::new("in", &dir)
            },
            "must be writable",
        ),
        (
            RootSpec {
                kind: RootKind::Additions,
                ..RootSpec::new("additions", &dir)
            },
            "must be writable",
        ),
        (
            RootSpec::new("additions", &dir)
                .of_kind(RootKind::Additions)
                .writing(&["x"]),
            "only for a read-only root",
        ),
        (
            RootSpec::new("play", &dir).writable().writing(&["x"]),
            "only for a read-only root",
        ),
        (RootSpec::new("", &dir), "name"),
        (RootSpec::new("a/b", &dir), "name"),
        (RootSpec::new("sets", "relative"), "absolute"),
        (
            RootSpec::new("sets", &dir).writing(&["../up"]),
            "writable path",
        ),
    ] {
        let why = env.refused(&spec);
        assert!(why.contains(says), "{spec:?}: {why}");
    }
    assert!(env.store.roots().is_empty());
}

#[test]
fn a_roots_name_cant_be_changed() {
    let env = Env::new();
    env.register(&RootSpec::new("sets", env.dir("sets")))
        .unwrap();
    let renamed = env.store.edit_root(
        "sets",
        &RootSpec::new("library", env.dir("sets")),
        "Operator",
    );
    assert!(
        matches!(&renamed, Err(Error::Refused(why)) if why.contains("name can't be changed")),
        "{renamed:?}"
    );
    assert!(env.store.roots().root("sets").is_some());
    assert!(env.store.roots().root("library").is_none());
}

#[test]
fn a_root_is_not_unregistered_while_something_is_registered_in_it() {
    let env = Env::new();
    env.register(&RootSpec::new("in", env.dir("in")).of_kind(RootKind::Ingest))
        .unwrap();
    env.store.register_ingest_folder("in").unwrap();

    let refused = env.store.unregister_root("in", "Operator");
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("registered as an ingest folder")),
        "{refused:?}"
    );
    assert!(env.store.roots().root("in").is_some());

    env.store.unregister_ingest_folder("in").unwrap();
    env.store.unregister_root("in", "Operator").unwrap();
    assert!(env.store.roots().is_empty());
}

#[test]
fn a_root_holding_source_sets_isnt_made_writable() {
    let env = Env::new();
    env.register(&RootSpec::new("sets", env.dir("sets")))
        .unwrap();
    std::fs::create_dir_all(env.dir("sets").join("Alpha")).unwrap();
    let dat = crate::common::write_dat(&env.dir("dats"), &[]);
    env.store.import_dat(&dat, None).unwrap();
    env.store
        .add_source_set("sets", "Alpha", "No-Intro/Test - System")
        .unwrap();

    let edited = env.store.edit_root(
        "sets",
        &RootSpec::new("sets", env.dir("sets")).writable(),
        "Operator",
    );
    assert!(
        matches!(&edited, Err(Error::Refused(why)) if why.contains("Alpha")),
        "{edited:?}"
    );
    // Writable paths are how a source set's folder is written to.
    env.store
        .edit_root(
            "sets",
            &RootSpec::new("sets", env.dir("sets")).writing(&["Alpha"]),
            "Operator",
        )
        .unwrap();
}

#[test]
fn a_roots_path_or_writability_doesnt_change_while_a_job_runs_on_it() {
    let env = Env::new();
    env.register(&RootSpec::new("play", env.dir("play")).writable())
        .unwrap();
    let job = env.store.enqueue_calibration("Operator").unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job.id);

    for (spec, change) in [
        (
            RootSpec::new("play", env.dir("elsewhere")).writable(),
            "path",
        ),
        (RootSpec::new("play", env.dir("play")), "writability"),
    ] {
        let refused = env.store.edit_root("play", &spec, "Operator");
        assert!(
            matches!(&refused, Err(Error::Conflict(why))
                if why.contains(change) && why.contains(&format!("calibration job {} is running", job.id))),
            "{refused:?}"
        );
    }
    // Its Ceilings can still change.
    env.store
        .edit_root(
            "play",
            &RootSpec {
                readers: Some(2),
                ..RootSpec::new("play", env.dir("play")).writable()
            },
            "Operator",
        )
        .unwrap();
    let root = &env.store.roots_view()[0];
    assert_eq!((root.readers, root.writable), (Some(2), true));
}

mod suggestions {
    use std::os::unix::fs::PermissionsExt;

    use title_select_api::roots::{FolderSuggestion, RootSpec};
    use title_select_store::roots::SUGGESTED_PER_MOUNT;

    use super::Env;

    fn paths(env: &Env, bases: &[std::path::PathBuf]) -> (Vec<String>, bool) {
        let found = env.store.folder_suggestions(bases);
        (
            found.folders.into_iter().map(|f| f.path).collect(),
            found.more,
        )
    }

    #[test]
    fn each_mount_point_and_the_folders_directly_in_it_are_suggested_in_order_and_nothing_deeper() {
        let env = Env::new();
        let mnt = env.dir("mnt");
        for dir in [
            "mnt/roms/nested",
            "mnt/games",
            "mnt/.hidden",
            "mnt/lost+found",
        ] {
            env.dir(dir);
        }
        std::fs::write(mnt.join("a-file"), b"").unwrap();
        let data = env.dir("data");
        let bases = [data.clone(), env.tmp.path().join("missing"), mnt.clone()];
        let (found, more) = paths(&env, &bases);
        let at = |p: &std::path::Path| p.display().to_string();
        assert_eq!(
            found,
            [
                at(&data),
                at(&mnt),
                at(&mnt.join("games")),
                at(&mnt.join("roms"))
            ]
        );
        assert!(!more);
    }

    #[test]
    fn a_symlink_is_never_followed_out_of_a_mount_point() {
        let env = Env::new();
        let outside = env.dir("outside");
        env.dir("outside/secret");
        let mnt = env.dir("mnt");
        env.dir("mnt/roms");
        std::os::unix::fs::symlink(&outside, mnt.join("escape")).unwrap();
        let linked = env.tmp.path().join("linked");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        let (found, _) = paths(&env, &[mnt.clone(), linked]);
        assert_eq!(
            found,
            [
                mnt.display().to_string(),
                mnt.join("roms").display().to_string()
            ]
        );
    }

    #[test]
    fn a_full_mount_point_is_cut_short_and_says_so() {
        let env = Env::new();
        let mnt = env.dir("mnt");
        for n in 0..SUGGESTED_PER_MOUNT + 5 {
            env.dir(&format!("mnt/{n:03}"));
        }
        let (found, more) = paths(&env, &[mnt]);
        assert_eq!(
            found.len(),
            SUGGESTED_PER_MOUNT + 1,
            "the mount point and its first folders"
        );
        assert!(found[1].ends_with("000"), "{found:?}");
        assert!(more);
    }

    #[test]
    fn a_registered_folder_is_left_out_and_each_says_whether_it_can_be_written() {
        let env = Env::new();
        let mnt = env.dir("mnt");
        let sets = env.dir("mnt/sets");
        let locked = env.dir("mnt/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        env.register(&RootSpec::new("sets", &sets)).unwrap();
        let found = env
            .store
            .folder_suggestions(std::slice::from_ref(&mnt))
            .folders;
        let can_lock = rustix::fs::access(&locked, rustix::fs::Access::WRITE_OK).is_err();
        assert_eq!(
            found,
            [
                FolderSuggestion {
                    path: mnt.display().to_string(),
                    writable: true
                },
                FolderSuggestion {
                    path: locked.display().to_string(),
                    writable: !can_lock
                },
            ]
        );
    }
}
