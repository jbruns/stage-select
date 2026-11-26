//! Building an arcade Selection into a MAME Library Folder (ADR 0026): each selected Game is `<shortname>.zip`,
//! non-merged with its Dependencies' ROMs inside, copied from a non-merged Source Set or repacked from a split or merged
//! one, and its Disks are copied into `<shortname>/`. A Library Folder lacking a Dependency is Incomplete, and Single
//! Copy places only a non-merged Source Set's ZIPs that no other Game is written from, never a Disk.
//!
//! The tests with Disks make their CHDs with chdman, found as the store finds it. Where it is missing they are skipped,
//! except in CI, where they fail.

use stage_select_api::roots::RootSpec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::common::noise;
use stage_select_api::build::{
    AddLibraryFolder, BuildItem, FileOutcome, FolderCheck, IncompleteReason, ItemKind, ItemState,
    LibraryFolder, Plan, PlanItem,
};
use stage_select_api::selection::{
    CoverageDrift, Dependency, KeptGame, Rule, Selection, SelectionGroup, SelectionPins,
    SelectionProfile, SelectionSummary,
};
use stage_select_api::storage::{SetLayout, SystemStorage};
use stage_select_api::{AuditEvidence, Job, JobKind, JobState};
use stage_select_store::{Fault, Store, Workers};
use stage_select_test_fixtures::{mame_rom_xml, write_torrentzip};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// A ROM's payload, by its name.
fn payload(name: &str) -> &'static [u8] {
    match name {
        "bios.u1" => b"bios",
        "snd.u2" => b"sound",
        "prog.u3" => b"program",
        "gfx.u4" | "gfxj.u4" => b"graphics",
        "progj.u3" => b"program j",
        other => panic!("no ROM {other}"),
    }
}

fn rom(name: &str, merge: Option<&str>) -> String {
    mame_rom_xml(name, merge, payload(name))
}

/// A BIOS; a sound device; a parent on the BIOS that names the device; and its clone, which shares the BIOS's ROM and
/// the parent's graphics, the latter under a name of its own. With `disk`, the parent lists a Disk of that SHA-1, and
/// the clone shares it.
fn listxml(disk: Option<&str>) -> String {
    let (disk, disk_m) = match disk {
        Some(sha1) => (
            format!(r#"<disk name="blaster" sha1="{sha1}"/>"#),
            format!(r#"<disk name="blaster" merge="blaster" sha1="{sha1}"/>"#),
        ),
        None => Default::default(),
    };
    format!(
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="acmebios" isbios="yes"><description>Acme BIOS</description>{bios}</machine>
  <machine name="acmesnd" isdevice="yes" runnable="no"><description>Acme Sound</description>{snd}</machine>
  <machine name="blaster" romof="acmebios"><description>Blaster</description>{bios_m}{prog}{gfx}{disk}
    <device_ref name="acmesnd"/></machine>
  <machine name="blasterj" cloneof="blaster" romof="blaster"><description>Blaster (Japan)</description>{bios_m}{progj}{gfxj}{disk_m}
    <device_ref name="acmesnd"/></machine>
</mame>
"#,
        bios = rom("bios.u1", None),
        snd = rom("snd.u2", None),
        bios_m = rom("bios.u1", Some("bios.u1")),
        prog = rom("prog.u3", None),
        gfx = rom("gfx.u4", None),
        progj = rom("progj.u3", None),
        gfxj = rom("gfxj.u4", Some("gfx.u4")),
    )
}

/// The family's ZIPs as `layout` keeps them.
fn family(layout: SetLayout) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut zips = vec![
        ("acmebios.zip", vec!["bios.u1"]),
        ("acmesnd.zip", vec!["snd.u2"]),
    ];
    zips.extend(match layout {
        SetLayout::NonMerged => vec![
            (
                "blaster.zip",
                vec!["bios.u1", "gfx.u4", "prog.u3", "snd.u2"],
            ),
            (
                "blasterj.zip",
                vec!["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"],
            ),
        ],
        SetLayout::Split => vec![
            ("blaster.zip", vec!["gfx.u4", "prog.u3"]),
            ("blasterj.zip", vec!["progj.u3"]),
        ],
        SetLayout::Merged => vec![("blaster.zip", vec!["gfx.u4", "prog.u3", "progj.u3"])],
    });
    zips
}

/// chdman, as the store finds it; `None`, outside CI, where it isn't there.
fn chdman() -> Option<PathBuf> {
    match stage_select_engine::chd::locate(None) {
        Ok(path) if path.is_file() => Some(path),
        found if std::env::var_os("CI").is_some() => {
            panic!("CI runs these tests with chdman: {found:?}")
        }
        _ => {
            eprintln!("skipped: chdman isn't here; set STAGE_SELECT_CHDMAN to run this test");
            None
        }
    }
}

/// Makes a CHD of 64 KiB of noise at `path` with chdman; returns its combined SHA-1.
fn make_chd(chdman: &Path, path: &Path) -> String {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let raw = scratch.path().join("disk.raw");
    std::fs::write(&raw, noise(7, 64 * 1024)).unwrap();
    let out = std::process::Command::new(chdman)
        .args(["createraw", "-hs", "4096", "-us", "512", "-i"])
        .arg(&raw)
        .arg("-o")
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    stage_select_engine::chd::header(&std::fs::File::open(path).unwrap())
        .unwrap()
        .sha1
}

/// A store with the MAME Source Set `mame` in the Root `sets`, which lets Builds write it, and the Library Folder
/// `arcade` bound to it in the writable Root `play`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    folder: LibraryFolder,
    sha256: String,
}

impl Env {
    /// The Source Set holds the family as `layout` keeps it, and the DAT lists no Disk.
    fn new(layout: SetLayout) -> Env {
        let env = Env::with(listxml(None), None);
        env.layout(layout);
        env.stored(layout);
        env
    }

    /// The DAT is `xml`, and chdman is found at `chdman`.
    fn with(xml: String, chdman: Option<&Path>) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("sets/mame")).unwrap();
        std::fs::create_dir_all(tmp.path().join("play")).unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store.use_chdman(chdman.map(Path::to_path_buf));
        crate::register(
            &store,
            &[
                RootSpec::new("sets", tmp.path().join("sets")),
                RootSpec::new("play", tmp.path().join("play")).library(),
            ],
        );
        let (revision, _) = store
            .import_dat_bytes(xml.as_bytes(), "mame.xml", None)
            .unwrap();
        store.add_source_set("sets", "mame", &revision.dat).unwrap();
        crate::let_change(&store, "mame");
        store
            .save_profile(
                "Mine",
                &SelectionProfile::default(),
                None,
                "Operator",
                &[],
                WORKERS,
            )
            .unwrap();
        let folder = store
            .add_library_folder(&AddLibraryFolder {
                root: "play".into(),
                path: Some("arcade".into()),
                template: None,
                source_set: "mame".into(),
                profile: "Mine".into(),
            })
            .unwrap();
        Env {
            tmp,
            store,
            folder,
            sha256: revision.sha256,
        }
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("sets/mame")
    }

    fn library(&self) -> PathBuf {
        self.tmp.path().join("play/arcade")
    }

    /// Writes the family as `layout` keeps it.
    fn stored(&self, layout: SetLayout) {
        for (zip, members) in family(layout) {
            write_zip(&self.set().join(zip), &members);
        }
    }

    /// Sets the MAME system's set layout.
    fn layout(&self, layout: SetLayout) {
        let mut policy = self.store.storage_policy().unwrap();
        policy.source_sets.systems.insert(
            "MAME/MAME".into(),
            SystemStorage {
                set_layout: Some(layout),
                ..SystemStorage::default()
            },
        );
        self.store.set_storage_policy(&policy, "Operator").unwrap();
    }

    /// Turns Single Copy on for play libraries.
    fn single_copy(&self) {
        let mut policy = self.store.storage_policy().unwrap();
        policy.play_libraries.single_copy = true;
        self.store.set_storage_policy(&policy, "Operator").unwrap();
    }

    fn audit(&self) {
        self.store
            .audit_source_set("mame", None, AuditEvidence::Full)
            .unwrap();
    }

    /// Selects `titles`, each the Winner of its family, with the BIOS and the device as their Dependencies.
    fn select(&self, titles: &[&str]) -> i64 {
        self.store
            .enqueue_selection("mame", None, "Mine", None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        assert_eq!(job.kind, JobKind::Selection);
        let rule = |stage: &str| Rule {
            stage: stage.into(),
            reference: None,
            message: "m".into(),
        };
        let group = |name: &str| SelectionGroup {
            group: name.into(),
            unresolved: false,
            kept: Vec::new(),
            clones: Vec::new(),
            left_out: Vec::new(),
            dependencies: Vec::new(),
            choice: None,
            drift: false,
        };
        let needed_by: Vec<String> = titles.iter().map(|t| (*t).to_owned()).collect();
        let mut groups: Vec<SelectionGroup> = ["acmebios", "acmesnd"]
            .into_iter()
            .map(|dep| SelectionGroup {
                dependencies: vec![Dependency {
                    title: dep.into(),
                    needed_by: needed_by.clone(),
                    rule: rule("dependency"),
                }],
                ..group(dep)
            })
            .collect();
        groups.extend(titles.iter().map(|title| SelectionGroup {
            kept: vec![KeptGame {
                title: (*title).into(),
                rule: rule("election"),
                unresolved: false,
            }],
            ..group(title)
        }));
        let selection = Selection {
            pins: SelectionPins::default(),
            dat_sha256: self.sha256.clone(),
            system: "MAME".into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups,
            stale_choices: BTreeMap::new(),
        };
        self.store
            .record_selection(job.id, &selection)
            .unwrap()
            .selection
            .unwrap()
    }

    fn plan(&self, selection: i64) -> Plan {
        self.store.plan_build(self.folder.id, selection).unwrap()
    }

    /// Checks the Library Folder, as a Check Job does.
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

    /// Approves `plan`, then claims and runs its Build Job, which succeeds; returns the ended Job.
    fn build(&self, plan: &Plan) -> Job {
        assert!(
            plan.content.blockers.is_empty(),
            "{:#?}",
            plan.content.blockers
        );
        let job = self
            .store
            .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        let ended = self.store.run_build_job(job.id, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        ended
    }

    /// The item for `target` of the Build the ended Job `job` ran.
    fn built_item(&self, job: &Job, target: &str) -> BuildItem {
        let build = self.store.build_of_job(job.id).unwrap().unwrap();
        let detail = self.store.build(build.id).unwrap();
        detail
            .items
            .into_iter()
            .find(|i| i.target == target)
            .unwrap_or_else(|| panic!("no item {target}"))
    }
}

/// Writes the TorrentZip `path`, holding `members`.
fn write_zip(path: &Path, members: &[&str]) {
    write_torrentzip(path, &payloads(members));
}

/// The members of the ZIP at `path`, sorted.
fn members(path: &Path) -> Vec<String> {
    let zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    names.sort();
    names
}

/// Every file under `dir`, as paths relative to it, leaving out Stage Select's own.
fn files(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".stage-select")
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            e.path()
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

fn item<'a>(plan: &'a Plan, target: &str) -> &'a PlanItem {
    plan.content
        .items
        .iter()
        .find(|i| i.target == target)
        .unwrap_or_else(|| panic!("no item {target} in {:#?}", plan.content.items))
}

#[test]
fn a_non_merged_source_set_is_copied_and_a_merged_one_repacked_into_the_same_self_contained_zips() {
    let mut built = Vec::new();
    for (layout, kind) in [
        (SetLayout::NonMerged, ItemKind::Copy),
        (SetLayout::Merged, ItemKind::Repack),
    ] {
        let env = Env::new(layout);
        env.audit();
        let plan = env.plan(env.select(&["blaster", "blasterj"]));
        for target in ["blaster.zip", "blasterj.zip"] {
            assert_eq!(item(&plan, target).kind, kind, "{layout:?}: {target}");
        }
        env.build(&plan);

        assert_eq!(files(&env.library()), ["blaster.zip", "blasterj.zip"]);
        assert_eq!(
            members(&env.library().join("blasterj.zip")),
            ["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"]
        );
        built.push(
            ["blaster.zip", "blasterj.zip"].map(|z| std::fs::read(env.library().join(z)).unwrap()),
        );
    }
    assert!(built[0] == built[1], "the copies and the repacks differ");
}

#[test]
fn a_library_folder_lacking_a_selected_games_device_is_incomplete_for_that_dependency() {
    let env = Env::with(listxml(None), None);
    // The Game's own ROMs are all there, but not the sound device's, in its ZIP or of its own.
    write_zip(&env.set().join("acmebios.zip"), &["bios.u1"]);
    write_zip(
        &env.set().join("blaster.zip"),
        &["bios.u1", "gfx.u4", "prog.u3"],
    );
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    let blaster = item(&plan, "blaster.zip");
    assert_eq!(blaster.kind, ItemKind::NoSource);
    assert_eq!(blaster.missing_dependencies, ["acmesnd"]);
    env.build(&plan);

    let folder = env.store.library_folder(env.folder.id).unwrap();
    let counts = folder.incomplete.expect("the folder lacks the device");
    assert_eq!(
        (
            counts.total(),
            counts.get(IncompleteReason::MissingDependency)
        ),
        (1, 1)
    );
    let lacking = env.store.incomplete(env.folder.id).unwrap().items;
    assert_eq!(lacking.len(), 1);
    assert_eq!(
        (lacking[0].reason, lacking[0].game.as_str()),
        (IncompleteReason::MissingDependency, "blaster")
    );
    assert_eq!(
        lacking[0].detail.as_deref(),
        Some(
            "it needs acmesnd (snd.u2), which the latest audit found no source for, so it can't run"
        )
    );
    assert!(files(&env.library()).is_empty());
}

/// A store whose DAT gives the family a Disk, which the Source Set keeps merged, in `blaster/`, beside the family's
/// non-merged ZIPs; none without chdman.
fn with_disk() -> Option<Env> {
    let chdman = chdman()?;
    let scratch = tempfile::tempdir().unwrap();
    let chd = scratch.path().join("blaster.chd");
    let sha1 = make_chd(&chdman, &chd);
    let env = Env::with(listxml(Some(&sha1)), Some(&chdman));
    env.stored(SetLayout::NonMerged);
    std::fs::create_dir(env.set().join("blaster")).unwrap();
    std::fs::copy(&chd, env.set().join("blaster/blaster.chd")).unwrap();
    Some(env)
}

#[test]
fn a_clone_winners_disk_is_copied_into_a_folder_of_its_own_and_built_for_the_listxmls_mame() {
    let Some(env) = with_disk() else { return };
    env.audit();
    let selection = env.select(&["blasterj"]);
    let plan = env.plan(selection);
    let disk = item(&plan, "blasterj/blaster.chd");
    assert_eq!((disk.kind, disk.disk), (ItemKind::Copy, true));
    assert_eq!(disk.sources[0].path, "blaster/blaster.chd");
    env.build(&plan);

    assert_eq!(
        files(&env.library()),
        ["blasterj.zip", "blasterj/blaster.chd"]
    );
    assert_eq!(
        std::fs::read(env.library().join("blasterj/blaster.chd")).unwrap(),
        std::fs::read(env.set().join("blaster/blaster.chd")).unwrap()
    );
    let folder = env.store.library_folder(env.folder.id).unwrap();
    assert_eq!(folder.incomplete, None);
    assert_eq!(folder.built_for.as_deref(), Some("0.289 (mame0289)"));
    assert_eq!(folder.disk_folders, ["blasterj"]);

    // Built again, nothing changed: the Disk in its folder is as built, and nothing there is drift.
    let again = env.plan(selection);
    assert!(
        again
            .content
            .items
            .iter()
            .all(|i| i.kind == ItemKind::Unchanged),
        "{:#?}",
        again.content.items
    );
    assert!(
        again.content.removals.is_empty(),
        "{:#?}",
        again.content.removals
    );
    let check = env.check();
    assert!(check.drift.is_empty(), "{:#?}", check.drift);
    assert!(
        check
            .files
            .iter()
            .all(|f| f.outcome == FileOutcome::Unchanged),
        "{:#?}",
        check.files
    );
}

#[test]
fn single_copy_places_a_non_merged_zip_and_copies_its_disk() {
    let Some(env) = with_disk() else { return };
    env.single_copy();
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    assert_eq!(item(&plan, "blaster.zip").kind, ItemKind::Place);
    let disk = item(&plan, "blaster/blaster.chd");
    assert_eq!(
        (disk.kind, disk.changes_source_set),
        (ItemKind::Copy, false)
    );
    env.build(&plan);

    assert_eq!(
        files(&env.library()),
        ["blaster.zip", "blaster/blaster.chd"]
    );
    let set = files(&env.set());
    assert!(!set.contains(&"blaster.zip".to_owned()), "{set:?}");
    assert!(set.contains(&"blaster/blaster.chd".to_owned()), "{set:?}");
}

#[test]
fn single_copy_places_nothing_from_a_merged_source_set_and_says_why() {
    let env = Env::new(SetLayout::Merged);
    env.single_copy();
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    assert_eq!(item(&plan, "blaster.zip").kind, ItemKind::Repack);
    assert!(
        plan.content.notes.iter().any(|n| n
            == "single copy is on, but source set mame uses the merged set layout, so each of its ZIPs holds \
                more than one game's ROMs: nothing is placed and each game is copied. Single copy places games only from a non-merged \
                source set"),
        "{:#?}",
        plan.content.notes
    );
    env.build(&plan);
    assert_eq!(files(&env.library()), ["blaster.zip"]);
    assert!(env.set().join("blaster.zip").is_file());
}

#[test]
fn a_library_folder_lacking_a_selected_games_bios_is_incomplete_for_that_dependency() {
    let env = Env::with(listxml(None), None);
    // The game lists the BIOS's ROM as one it shares, but neither its ZIP nor the BIOS's holds it.
    write_zip(&env.set().join("acmesnd.zip"), &["snd.u2"]);
    write_zip(
        &env.set().join("blaster.zip"),
        &["gfx.u4", "prog.u3", "snd.u2"],
    );
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    let blaster = item(&plan, "blaster.zip");
    assert_eq!(blaster.kind, ItemKind::NoSource);
    assert_eq!(blaster.missing_dependencies, ["acmebios"]);
}

#[test]
fn single_copy_places_no_game_whose_zip_isnt_yet_self_contained() {
    let env = Env::with(listxml(None), None);
    env.single_copy();
    // The set layout is non-merged, but the game's ZIP is still split: its BIOS's and device's ROMs are elsewhere.
    env.stored(SetLayout::Split);
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    assert_eq!(item(&plan, "blaster.zip").kind, ItemKind::Repack);
    assert!(
        plan.content.notes.iter().any(|n| n
            == "1 game's files are copied rather than placed: its ZIP in the source set doesn't yet hold every ROM it \
                needs, so it is written from other games' ZIPs too, which stay there"),
        "{:#?}",
        plan.content.notes
    );
    env.build(&plan);
    assert_eq!(
        files(&env.set()),
        ["acmebios.zip", "acmesnd.zip", "blaster.zip", "blasterj.zip"]
    );
}

#[test]
fn a_disk_whose_chd_was_damaged_after_its_audit_is_never_put_in_place() {
    let Some(env) = with_disk() else { return };
    env.audit();
    // A byte of its data changes, and its header, with the SHA-1 it records, doesn't.
    let chd = env.set().join("blaster/blaster.chd");
    let mut bytes = std::fs::read(&chd).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0xff;
    std::fs::write(&chd, bytes).unwrap();
    let plan = env.plan(env.select(&["blaster"]));
    let ended = env.build(&plan);

    let disk = env.built_item(&ended, "blaster/blaster.chd");
    assert_eq!(disk.state, ItemState::Failed, "{disk:?}");
    let error = disk.error.as_deref().unwrap();
    assert!(
        error.contains("blaster/blaster.chd does not verify"),
        "{error}"
    );
    // A Game's files are put in place together, so nothing of it is.
    assert!(files(&env.library()).is_empty());
    let folder = env.store.library_folder(env.folder.id).unwrap();
    let counts = folder.incomplete.expect("the folder lacks the game");
    // Its ZIP and its Disk.
    assert_eq!(counts.get(IncompleteReason::Failed), 2, "{counts:?}");
}

#[test]
fn a_placed_games_disk_stays_the_folders_own_copy_when_planned_again() {
    let Some(env) = with_disk() else { return };
    env.single_copy();
    env.audit();
    let selection = env.select(&["blaster"]);
    env.build(&env.plan(selection));

    // Its ZIP is placed, and its Disk, a copy, is as built.
    let again = env.plan(selection);
    assert_eq!(item(&again, "blaster.zip").kind, ItemKind::Unchanged);
    let disk = item(&again, "blaster/blaster.chd");
    assert_eq!(disk.kind, ItemKind::Unchanged, "{disk:#?}");

    // Once the copy is gone, it is copied again from the Source Set.
    std::fs::remove_file(env.library().join("blaster/blaster.chd")).unwrap();
    env.check();
    let rebuilt = env.plan(selection);
    let disk = item(&rebuilt, "blaster/blaster.chd");
    assert_eq!(disk.kind, ItemKind::Copy, "{disk:#?}");
    env.build(&rebuilt);
    assert_eq!(
        std::fs::read(env.library().join("blaster/blaster.chd")).unwrap(),
        std::fs::read(env.set().join("blaster/blaster.chd")).unwrap()
    );
}

#[test]
fn single_copy_places_no_zip_another_game_is_repacked_from() {
    let env = Env::with(listxml(None), None);
    env.layout(SetLayout::NonMerged);
    env.single_copy();
    // The parent's ZIP is whole, and the only one to hold the device's ROM, which the clone's lacks.
    write_zip(
        &env.set().join("blaster.zip"),
        &["bios.u1", "gfx.u4", "prog.u3", "snd.u2"],
    );
    write_zip(
        &env.set().join("blasterj.zip"),
        &["bios.u1", "gfxj.u4", "progj.u3"],
    );
    env.audit();
    let plan = env.plan(env.select(&["blaster", "blasterj"]));
    let clone = item(&plan, "blasterj.zip");
    assert_eq!(clone.kind, ItemKind::Repack);
    assert!(
        clone.sources.iter().any(|s| s.path == "blaster.zip"),
        "{clone:#?}"
    );
    let parent = item(&plan, "blaster.zip");
    assert_eq!(parent.kind, ItemKind::Copy, "{parent:#?}");
    assert!(
        plan.content.notes.iter().any(|n| n
            == "1 game's files are copied rather than placed: another game is written from its ZIP in the source \
                set as well, so that ZIP stays there"),
        "{:#?}",
        plan.content.notes
    );
    env.build(&plan);
    assert_eq!(
        members(&env.library().join("blasterj.zip")),
        ["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"]
    );
    assert!(env.set().join("blaster.zip").is_file());
}

#[test]
fn a_library_folder_lacking_a_selected_games_disk_is_incomplete() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster.chd"));
    // The DAT lists the Disk, and the Source Set keeps the family's ZIPs but no CHD.
    let env = Env::with(listxml(Some(&sha1)), Some(&chdman));
    env.stored(SetLayout::NonMerged);
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    assert_eq!(item(&plan, "blaster/blaster.chd").kind, ItemKind::NoSource);
    env.build(&plan);

    let folder = env.store.library_folder(env.folder.id).unwrap();
    // A Game's files go in place together, so its ZIP waits on its Disk.
    let counts = folder.incomplete.expect("the folder lacks the disk");
    assert_eq!(
        (
            counts.total(),
            counts.get(IncompleteReason::NoSource),
            counts.get(IncompleteReason::Blocked)
        ),
        (2, 1, 1)
    );
    let lacking: Vec<(IncompleteReason, String)> = env
        .store
        .incomplete(env.folder.id)
        .unwrap()
        .items
        .into_iter()
        .map(|i| (i.reason, i.target))
        .collect();
    assert_eq!(
        lacking,
        [
            (IncompleteReason::Blocked, "blaster.zip".to_owned()),
            (IncompleteReason::NoSource, "blaster/blaster.chd".to_owned()),
        ]
    );
    assert!(files(&env.library()).is_empty());
}

#[test]
fn single_copy_places_no_zip_whose_disk_fails_to_verify() {
    let Some(env) = with_disk() else { return };
    env.single_copy();
    env.audit();
    // A byte of its data changes, and its header, with the SHA-1 it records, doesn't.
    let chd = env.set().join("blaster/blaster.chd");
    let mut bytes = std::fs::read(&chd).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0xff;
    std::fs::write(&chd, bytes).unwrap();
    let plan = env.plan(env.select(&["blaster"]));
    assert_eq!(item(&plan, "blaster.zip").kind, ItemKind::Place);
    let ended = env.build(&plan);

    let disk = env.built_item(&ended, "blaster/blaster.chd");
    assert_eq!(disk.state, ItemState::Failed, "{disk:?}");
    let zip = env.built_item(&ended, "blaster.zip");
    assert_eq!(zip.state, ItemState::Failed, "{zip:?}");
    assert_eq!(
        zip.error.as_deref(),
        Some(
            "blaster.zip was not put in place, because blaster/blaster.chd of the same game wasn't; blaster stays \
             in source set mame as it was"
        )
    );
    // The game's ZIP stays in the source set, and nothing of it is in the folder.
    assert!(files(&env.library()).is_empty());
    assert!(env.set().join("blaster.zip").is_file());
}

#[test]
fn a_placed_parent_going_home_is_read_first_by_its_clone() {
    let env = Env::with(listxml(None), None);
    env.layout(SetLayout::NonMerged);
    env.single_copy();
    // The parent's ZIP is whole, and the only one to hold the device's ROM, which the clone's lacks.
    write_zip(
        &env.set().join("blaster.zip"),
        &["bios.u1", "gfx.u4", "prog.u3", "snd.u2"],
    );
    write_zip(
        &env.set().join("blasterj.zip"),
        &["bios.u1", "gfxj.u4", "progj.u3"],
    );
    env.audit();
    env.build(&env.plan(env.select(&["blaster"])));
    assert!(!env.set().join("blaster.zip").exists());

    // The clone alone is selected now: it is written from the parent's placed ZIP, which then goes home.
    let plan = env.plan(env.select(&["blasterj"]));
    let clone = item(&plan, "blasterj.zip");
    assert_eq!(clone.kind, ItemKind::Repack);
    assert!(
        clone
            .sources
            .iter()
            .any(|s| s.path.ends_with("placed/1/blaster.zip")),
        "{clone:#?}"
    );
    assert_eq!(plan.content.returns.len(), 1, "{:#?}", plan.content.returns);
    let ended = env.build(&plan);

    let built = env.built_item(&ended, "blasterj.zip");
    assert_eq!(built.state, ItemState::Applied, "{built:?}");
    assert_eq!(files(&env.library()), ["blasterj.zip"]);
    assert_eq!(
        members(&env.library().join("blasterj.zip")),
        ["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"]
    );
    assert_eq!(
        members(&env.set().join("blaster.zip")),
        ["bios.u1", "gfx.u4", "prog.u3", "snd.u2"]
    );
}

#[test]
fn a_cancel_once_a_games_disk_is_in_place_still_places_its_zip() {
    let Some(env) = with_disk() else { return };
    env.single_copy();
    env.audit();
    let plan = env.plan(env.select(&["blaster"]));
    let job = env
        .store
        .approve_plan(plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job.id);
    // The Operator cancels once the disk is put in place, before its game's ZIP is.
    let (at, published) = std::sync::mpsc::channel::<()>();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let (at, wait) = (std::sync::Mutex::new(at), std::sync::Mutex::new(wait));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if matches!(fault, Fault::AfterPublish(_)) {
                at.lock().unwrap().send(()).unwrap();
                wait.lock().unwrap().recv().unwrap();
            }
        })));
    std::thread::scope(|scope| {
        let run = scope.spawn(|| env.store.run_build_job(job.id, None).unwrap());
        published
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        env.store.cancel_job(job.id).unwrap();
        go.send(()).unwrap();
        run.join().unwrap();
    });
    env.store.hook_faults(None);

    assert_eq!(
        env.built_item(&job, "blaster.zip").state,
        ItemState::Applied
    );
    assert_eq!(
        files(&env.library()),
        ["blaster.zip", "blaster/blaster.chd"]
    );
    assert!(!env.set().join("blaster.zip").exists());
}

/// Writes `path` as an ordinary ZIP, not TorrentZip, holding `members`.
fn write_plain_zip(path: &Path, members: &[&str]) {
    stage_select_test_fixtures::write_zip(path, &payloads(members));
}

/// `members` with their payloads.
fn payloads<'a>(members: &[&'a str]) -> Vec<(&'a str, &'static [u8])> {
    members.iter().map(|m| (*m, payload(m))).collect()
}

/// Plans `selection` again and expects its Game's ZIP to need nothing: every ROM of it, its Dependencies' too, has a
/// source the Audit names.
fn assert_needs_nothing_again(env: &Env, selection: i64) {
    let again = env.plan(selection);
    let blaster = item(&again, "blaster.zip");
    assert_eq!(blaster.kind, ItemKind::Unchanged, "{blaster:#?}");
}

#[test]
fn single_copy_places_a_built_self_contained_zip_with_its_dependencies_evidence() {
    let env = Env::new(SetLayout::NonMerged);
    env.audit();
    let selection = env.select(&["blaster"]);
    env.build(&env.plan(selection));
    env.single_copy();

    let plan = env.plan(selection);
    let placing = item(&plan, "blaster.zip");
    assert_eq!(placing.kind, ItemKind::Place);
    assert_eq!(
        placing.place.as_ref().map(|p| p.how),
        Some(stage_select_api::build::PlaceHow::Built)
    );
    let ended = env.build(&plan);

    let placed = env.built_item(&ended, "blaster.zip");
    assert_eq!(placed.state, ItemState::Applied, "{placed:?}");
    assert!(!env.set().join("blaster.zip").exists());
    assert_needs_nothing_again(&env, selection);
}

#[test]
fn single_copy_converts_an_ordinary_zip_with_its_dependencies_evidence() {
    let env = Env::with(listxml(None), None);
    env.layout(SetLayout::NonMerged);
    env.single_copy();
    // The game's own ZIP is the only one to hold its BIOS's and device's ROMs.
    write_plain_zip(
        &env.set().join("blaster.zip"),
        &["bios.u1", "gfx.u4", "prog.u3", "snd.u2"],
    );
    env.audit();
    let selection = env.select(&["blaster"]);

    let plan = env.plan(selection);
    let placing = item(&plan, "blaster.zip");
    assert_eq!(
        placing.place.as_ref().map(|p| p.how),
        Some(stage_select_api::build::PlaceHow::Convert),
        "{placing:#?} {:#?}",
        plan.content.notes
    );
    let ended = env.build(&plan);

    let placed = env.built_item(&ended, "blaster.zip");
    assert_eq!(placed.state, ItemState::Applied, "{placed:?}");
    assert!(!env.set().join("blaster.zip").exists());
    assert_eq!(
        members(&env.library().join("blaster.zip")),
        ["bios.u1", "gfx.u4", "prog.u3", "snd.u2"]
    );
    assert_needs_nothing_again(&env, selection);
}

#[test]
fn a_placed_zip_goes_home_as_that_zip_whatever_format_the_storage_policy_gives_source_sets() {
    let env = Env::new(SetLayout::NonMerged);
    env.single_copy();
    env.audit();
    env.build(&env.plan(env.select(&["blaster"])));
    assert!(!env.set().join("blaster.zip").exists());
    let mut policy = env.store.storage_policy().unwrap();
    policy.source_sets.format = stage_select_api::storage::StorageFormat::LooseFolder;
    policy.play_libraries.single_copy = false;
    env.store.set_storage_policy(&policy, "Operator").unwrap();

    let plan = env.plan(env.select(&["blasterj"]));
    assert_eq!(plan.content.returns.len(), 1, "{:#?}", plan.content.returns);
    let back: Vec<&str> = plan.content.returns[0]
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(back, ["blaster.zip"]);
    env.build(&plan);

    assert_eq!(
        members(&env.set().join("blaster.zip")),
        ["bios.u1", "gfx.u4", "prog.u3", "snd.u2"]
    );
    assert!(!env.set().join("blaster").exists());
    // Every ROM of the ZIP home, its device's too, has a source the Audit names.
    let again = env.plan(env.select(&["blaster"]));
    let blaster = item(&again, "blaster.zip");
    assert_eq!(blaster.kind, ItemKind::Copy, "{blaster:#?}");
}

impl Env {
    /// Places blaster's self-contained ZIP, its Dependencies' ROMs inside, by a rename from a non-merged Source Set;
    /// returns its bytes.
    fn place_blaster(&self) -> Vec<u8> {
        self.single_copy();
        self.audit();
        let plan = self.plan(self.select(&["blaster"]));
        let placing = item(&plan, "blaster.zip");
        assert_eq!(
            placing.place.as_ref().map(|p| p.how),
            Some(stage_select_api::build::PlaceHow::Rename),
            "{placing:#?}"
        );
        self.build(&plan);
        assert!(!self.set().join("blaster.zip").exists());
        std::fs::read(self.library().join("blaster.zip")).unwrap()
    }

    fn placed_blaster(&self) -> Vec<stage_select_api::build::PlacedFile> {
        let mut placed = self.store.placed_files("mame").unwrap();
        placed.retain(|p| p.game == "blaster");
        placed
    }
}

#[test]
fn a_placed_self_contained_zip_whose_mode_changed_goes_home_every_way() {
    use stage_select_api::build::{RemoveAction, RemoveGames};
    for how in ["deselected", "sent back", "unregistered"] {
        let env = Env::new(SetLayout::NonMerged);
        let bytes = env.place_blaster();
        let at = env.library().join("blaster.zip");
        crate::placed_recovery::chmod(&at);
        assert_eq!(env.check().summary.conflict, 0, "{how}");

        let plan = match how {
            "deselected" => env.plan(env.select(&["blasterj"])),
            "sent back" => env
                .store
                .plan_game_removal(
                    env.folder.id,
                    &RemoveGames {
                        games: vec!["blaster".into()],
                        action: RemoveAction::SendBack,
                    },
                )
                .unwrap(),
            _ => env.store.plan_library_removal(env.folder.id).unwrap(),
        };

        assert_eq!(plan.content.returns.len(), 1, "{how}: {:#?}", plan.content);
        env.build(&plan);
        assert!(env.placed_blaster().is_empty(), "{how}");
        assert_eq!(
            std::fs::read(env.set().join("blaster.zip")).unwrap(),
            bytes,
            "{how}"
        );
    }
}

#[test]
fn an_audit_refreshes_a_placed_self_contained_zip_whose_mode_changed() {
    let env = Env::new(SetLayout::NonMerged);
    let bytes = env.place_blaster();
    crate::placed_recovery::chmod(&env.library().join("blaster.zip"));

    env.audit();

    let placed = env.placed_blaster();
    let sha256 = crate::single_copy::sha256_hex(&bytes);
    assert_eq!(placed[0].sha256.as_deref(), Some(sha256.as_str()));
}

#[test]
fn a_placed_self_contained_zip_whose_dependencys_rom_isnt_home_stays_placed() {
    let env = Env::new(SetLayout::NonMerged);
    env.place_blaster();
    // blaster's own ROMs are home, so it is complete there, but not the sound device's ROM its placed ZIP held: in no
    // ZIP of the source set.
    std::fs::remove_file(env.library().join("blaster.zip")).unwrap();
    for zip in ["acmesnd.zip", "blasterj.zip"] {
        std::fs::remove_file(env.set().join(zip)).unwrap();
    }
    write_zip(
        &env.set().join("blaster.zip"),
        &["bios.u1", "gfx.u4", "prog.u3"],
    );

    env.audit();

    assert_eq!(env.placed_blaster().len(), 1);
}
