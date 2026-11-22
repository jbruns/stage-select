//! Resolving a Source Set's or a Library Folder's path beneath its Root: which paths are refused, with what words,
//! and how symlinks are followed.

use std::path::{Path, PathBuf};

use title_select_api::build::AddLibraryFolder;
use title_select_api::roots::RootSpec;
use title_select_api::selection::SelectionProfile;
use title_select_store::{Error, Store, Workers};

use crate::common::*;

const DAT: &str = "No-Intro/Test - System";
const SET: &str = "Test - System";

/// A store with a DAT, a Profile, a read-only Root `sets` holding the folder [`SET`], a writable Root `play`, and a
/// folder `outside` in neither.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("roots/sets").join(SET)).unwrap();
        std::fs::create_dir_all(tmp.path().join("outside")).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", tmp.path().join("roots/sets")),
                RootSpec::new("play", tmp.path().join("roots/play")).writable(),
            ],
        );
        let dat = write_dat_version(
            &tmp.path().join("in"),
            "Test - System (1).dat",
            "20260101-000000",
            &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
        );
        store.import_dat(&dat, None).unwrap();
        let workers = Workers {
            readers: 1,
            hashers: 1,
        };
        store
            .save_profile(
                "Mine",
                &SelectionProfile::default(),
                None,
                "Operator",
                &[],
                workers,
            )
            .unwrap();
        Env { tmp, store }
    }

    fn sets(&self) -> PathBuf {
        self.tmp.path().join("roots/sets")
    }

    fn play(&self) -> PathBuf {
        self.tmp.path().join("roots/play")
    }

    fn outside(&self) -> PathBuf {
        self.tmp.path().join("outside")
    }

    fn add_set(&self, rel: &str) -> Result<String, String> {
        self.store
            .add_source_set("sets", rel, DAT)
            .map(|set| set.path)
            .map_err(refusal)
    }

    fn add_folder(&self, rel: &str) -> Result<String, String> {
        self.store
            .add_library_folder(&AddLibraryFolder {
                root: "play".into(),
                path: Some(rel.into()),
                template: None,
                source_set: SET.into(),
                profile: "Mine".into(),
            })
            .map(|folder| folder.path)
            .map_err(refusal)
    }
}

fn refusal(e: Error) -> String {
    match e {
        Error::Refused(why) => why,
        e => panic!("not a refusal: {e:?}"),
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[test]
fn a_source_set_path_is_refused_unless_it_names_a_folder_that_resolves_inside_its_root() {
    let env = Env::new();
    std::os::unix::fs::symlink(env.outside(), env.sets().join("escape")).unwrap();
    std::fs::write(env.sets().join("file"), b"").unwrap();

    for (rel, why) in [
        ("..", "..: a source set's path must not contain `..`"),
        (
            "Test - System/../../outside",
            "Test - System/../../outside: a source set's path must not contain `..`",
        ),
        (
            "/abs",
            "/abs: a source set's path must be relative to its root",
        ),
        (
            "C:folder",
            "C:folder: a source set's path must be relative to its root",
        ),
        (
            "./C:/folder",
            "./C:/folder: a source set's path must be relative to its root",
        ),
        (
            r"Test - System\..\..\outside",
            r"Test - System\..\..\outside: a source set's path must separate folders with /, not \",
        ),
        (
            "",
            ": a source set's path must name a folder inside its root",
        ),
        (
            ".",
            ".: a source set's path must name a folder inside its root",
        ),
        (
            "./",
            "./: a source set's path must name a folder inside its root",
        ),
        ("escape", "escape: resolves outside root sets"),
        ("escape/", "escape: resolves outside root sets"),
    ] {
        assert_eq!(env.add_set(rel), Err(why.to_owned()), "{rel:?}");
    }
    let absent = env.sets().join("absent");
    let why = env.add_set("absent").unwrap_err();
    assert!(
        why.starts_with(&format!("{}: ", absent.display())) && why.contains("os error"),
        "{why}"
    );
    assert_eq!(
        env.add_set("file"),
        Err(format!(
            "{}: not a directory",
            env.sets().join("file").display()
        ))
    );
    assert_eq!(
        env.store
            .add_source_set("nope", SET, DAT)
            .map_err(refusal)
            .unwrap_err(),
        "no root named nope is registered"
    );
    assert!(env.store.source_sets().unwrap().is_empty());
}

#[test]
fn a_source_set_path_is_normalized_and_may_be_a_symlink_that_stays_inside_its_root() {
    let env = Env::new();
    std::fs::create_dir_all(env.sets().join("real/inner")).unwrap();
    std::os::unix::fs::symlink(env.sets().join("real"), env.sets().join("alias")).unwrap();

    assert_eq!(env.add_set("./Test - System/"), Ok(SET.to_owned()));
    assert_eq!(env.add_set("alias//inner/."), Ok("alias/inner".to_owned()));
}

#[test]
fn a_source_set_in_a_root_whose_folder_is_gone_is_refused_naming_the_root() {
    let env = Env::new();
    std::fs::remove_dir_all(env.sets()).unwrap();

    let why = env.add_set(SET).unwrap_err();

    assert!(
        why.starts_with(&format!("root sets ({}): ", env.sets().display()))
            && why.contains("os error"),
        "{why}"
    );
}

#[test]
fn a_library_folder_path_is_refused_unless_it_is_a_plain_path_inside_its_root() {
    let env = Env::new();
    env.add_set(SET).unwrap();

    for (rel, why) in [
        ("..", "..: a library folder's path must not contain `..`"),
        (
            "a/../../outside",
            "a/../../outside: a library folder's path must not contain `..`",
        ),
        (
            "/abs",
            "/abs: a library folder's path must be relative to its root",
        ),
        (
            "C:folder",
            "C:folder: a library folder's path must be relative to its root",
        ),
        (
            r"a\..\..\outside",
            r"a\..\..\outside: a library folder's path must separate folders with /, not \",
        ),
        (
            "",
            ": a library folder's path must name a folder inside its root",
        ),
        (
            ".",
            ".: a library folder's path must name a folder inside its root",
        ),
        (
            "./",
            "./: a library folder's path must name a folder inside its root",
        ),
    ] {
        assert_eq!(env.add_folder(rel), Err(why.to_owned()), "{rel:?}");
    }
    let refused = env.store.add_library_folder(&AddLibraryFolder {
        root: "nope".into(),
        path: Some("a".into()),
        template: None,
        source_set: SET.into(),
        profile: "Mine".into(),
    });
    assert_eq!(
        refused.map_err(refusal).unwrap_err(),
        "no root named nope is registered"
    );
    assert!(env.store.library_folders().unwrap().is_empty());
}

#[test]
fn a_library_folder_path_that_does_not_exist_yet_is_normalized_and_created() {
    let env = Env::new();
    env.add_set(SET).unwrap();

    assert_eq!(env.add_folder("./new//deep/"), Ok("new/deep".to_owned()));

    assert!(env.play().join("new/deep").is_dir());
}

#[test]
fn a_library_folder_path_through_a_symlink_is_checked_where_it_leads_and_then_refused() {
    let env = Env::new();
    env.add_set(SET).unwrap();
    std::fs::create_dir_all(env.play().join("real")).unwrap();
    std::os::unix::fs::symlink(env.play().join("real"), env.play().join("alias")).unwrap();
    std::os::unix::fs::symlink(env.outside(), env.play().join("escape")).unwrap();
    std::os::unix::fs::symlink(env.sets().join(SET), env.play().join("into-set")).unwrap();

    // Where a symlink leads is what is checked for overlap, through whatever of the path exists.
    let set = canonical(&env.sets().join(SET));
    for rel in ["into-set", "into-set/new"] {
        assert_eq!(
            env.add_folder(rel),
            Err(format!(
                "{rel} in root play would overlap a source set at {}",
                set.display()
            )),
        );
    }
    // A symlink that stays inside the Root, or leads out of it, passes that check, and is then refused as a symlink.
    for rel in ["alias", "alias/new", "escape", "escape/new"] {
        let link = rel.split('/').next().unwrap();
        let why = env.add_folder(rel).unwrap_err();
        assert!(
            why.starts_with(&format!("{}: ", env.play().join(link).display()))
                && why.ends_with(
                    " (no part of a library folder's path inside its root may be a symlink)"
                ),
            "{rel}: {why}"
        );
    }
    assert!(env.store.library_folders().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(env.outside()).unwrap().count(), 0);
}
