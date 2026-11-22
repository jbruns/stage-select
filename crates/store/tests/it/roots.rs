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
    assert_eq!((root.name.as_str(), root.updated_by.as_str()), ("sets", "Operator"));

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
    assert!(why.contains("can't be read"), "{why}");
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
        (RootSpec::new("sets", &dir).writing(&["../up"]), "writable path"),
    ] {
        let why = env.refused(&spec);
        assert!(why.contains(says), "{spec:?}: {why}");
    }
    assert!(env.store.roots().is_empty());
}

#[test]
fn a_roots_name_cant_be_changed() {
    let env = Env::new();
    env.register(&RootSpec::new("sets", env.dir("sets"))).unwrap();
    let renamed = env
        .store
        .edit_root("sets", &RootSpec::new("library", env.dir("sets")), "Operator");
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
    env.register(&RootSpec::new("sets", env.dir("sets"))).unwrap();
    std::fs::create_dir_all(env.dir("sets").join("Alpha")).unwrap();
    let dat = crate::common::write_dat(&env.dir("dats"), &[]);
    env.store.import_dat(&dat, None).unwrap();
    env.store
        .add_source_set("sets", "Alpha", "No-Intro/Test - System")
        .unwrap();

    let edited = env
        .store
        .edit_root("sets", &RootSpec::new("sets", env.dir("sets")).writable(), "Operator");
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
        (RootSpec::new("play", env.dir("elsewhere")).writable(), "path"),
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
