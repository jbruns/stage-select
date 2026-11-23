//! Set Layouts of MAME Source Sets (ADR 0026): a Repair repacks a family towards the layout the Storage Policy gives,
//! filling from sibling ZIPs, and keeps a ZIP aside only once every copy written from it is verified.

use stage_select_api::roots::{RootKind, RootSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use stage_select_api::build::AddLibraryFolder;
use stage_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan, RepairStep};
use stage_select_api::selection::SelectionProfile;
use stage_select_api::storage::{SetLayout, StorageFormat, SystemStorage, split_placed};
use stage_select_api::{AuditEvidence, EvidenceLevel, Finding, GameStatus, JobState};
use stage_select_engine::build::is_torrentzip;
use stage_select_store::{Store, Workers};
use stage_select_test_fixtures::{mame_rom_xml, write_torrentzip};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// Every ROM name [`payload`] gives a payload for, but the nodump ROM's.
const ROMS: [&str; 9] = [
    "bios.u1",
    "snd.u2",
    "prog.u3",
    "gfx.u4",
    "gfxj.u4",
    "progj.u3",
    "extra.u6",
    "gamebios.u1",
    "vga.bin",
];

/// A ROM's payload, by its name: every name of [`ROMS`], and the nodump ROM's.
fn payload(name: &str) -> &'static [u8] {
    match name {
        "bios.u1" => b"bios",
        "snd.u2" => b"sound",
        "prog.u3" => b"program",
        "gfx.u4" | "gfxj.u4" => b"graphics",
        "progj.u3" => b"program j",
        "extra.u6" => b"extra",
        // A graphics device's ROM, and another device's copy of it under a name of its own.
        "vga.bin" | "vgax.bin" => b"video",
        // The BIOS's ROM, under a name of a Game's own.
        "gamebios.u1" => b"bios",
        // Under the name of a nodump ROM, which no file matches.
        "pal.u5" => b"pal?",
        other => panic!("no ROM {other}"),
    }
}

/// The SHA-1 of `bytes`, as a DAT gives it.
fn sha1_hex(bytes: &[u8]) -> String {
    <sha1::Sha1 as sha1::Digest>::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn rom(name: &str, merge: Option<&str>) -> String {
    mame_rom_xml(name, merge, payload(name))
}

/// A BIOS; a sound device; a parent on the BIOS that names the device; and its clone, which shares the BIOS's ROM and
/// the parent's graphics, the latter under a name of its own, by `merge=`. The machine `nodump` names also lists a ROM
/// with no good dump known.
fn listxml(nodump: Option<&str>) -> String {
    let pal = |machine: &str| match nodump == Some(machine) {
        true => r#"<rom name="pal.u5" size="4" status="nodump"/>"#,
        false => "",
    };
    format!(
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="acmebios" isbios="yes"><description>Acme BIOS</description>{bios}</machine>
  <machine name="acmesnd" isdevice="yes" runnable="no"><description>Acme Sound</description>{snd}{snd_pal}</machine>
  <machine name="blaster" romof="acmebios"><description>Blaster</description>{bios_m}{prog}{gfx}{pal}
    <device_ref name="acmesnd"/></machine>
  <machine name="blasterj" cloneof="blaster" romof="blaster"><description>Blaster (Japan)</description>{bios_m}{progj}{gfxj}
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
        pal = pal("blaster"),
        snd_pal = pal("acmesnd"),
    )
}

/// The family's ZIPs as `layout` keeps them.
fn family(layout: SetLayout) -> Vec<(&'static str, Vec<&'static str>)> {
    let devices = [
        ("acmebios.zip", vec!["bios.u1"]),
        ("acmesnd.zip", vec!["snd.u2"]),
    ];
    let mut zips = devices.to_vec();
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

/// A store with the MAME Source Set `mame` in the Root `sets`, which lets repairs write it, and the Library Folder
/// `arcade` bound to it in the writable Root `play`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    /// The Library Folder's id.
    folder: i64,
}

impl Env {
    fn new() -> Env {
        Env::with(listxml(None))
    }

    /// The Source Set bound to the ListXML `xml`.
    fn with(xml: String) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("sets");
        std::fs::create_dir_all(sets.join("mame")).unwrap();
        std::fs::create_dir_all(tmp.path().join("play")).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        crate::register(
            &store,
            &[
                RootSpec::new("sets", sets).writing(&["mame"]),
                RootSpec::new("play", tmp.path().join("play")).writable(),
                RootSpec::new("in", tmp.path().join("in")).of_kind(RootKind::Ingest),
            ],
        );
        let (revision, _) = store
            .import_dat_bytes(xml.as_bytes(), "mame.xml", None)
            .unwrap();
        store.add_source_set("sets", "mame", &revision.dat).unwrap();
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
            .unwrap()
            .id;
        Env { tmp, store, folder }
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("sets/mame")
    }

    /// Writes the family as `layout` keeps it.
    fn stored(&self, layout: SetLayout) {
        for (zip, members) in family(layout) {
            self.zip(zip, &members);
        }
    }

    /// Writes `zip` as TorrentZip, holding `members`.
    fn zip(&self, zip: &str, members: &[&str]) {
        write_zip(&self.set().join(zip), members);
    }

    /// The Library Folder `arcade`: the only one bound to the Source Set, so the one holding its Placed Games.
    fn library(&self) -> PathBuf {
        self.tmp.path().join("play/arcade")
    }

    /// Writes `game`'s TorrentZip holding `members` in the Library Folder, and records it placed there, as Single Copy
    /// places a self-contained Game.
    fn place(&self, game: &str, members: &[&str]) {
        write_zip(&self.library().join(format!("{game}.zip")), members);
        self.placed(game);
    }

    /// Records `game` placed in the Library Folder, as `<game>.zip`.
    fn placed(&self, game: &str) {
        let name = format!("{game}.zip");
        self.store
            .record_placement("mame", game, self.folder, &[&name])
            .unwrap();
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

    /// Turns Single Copy for Play Libraries `on` or off.
    fn single_copy(&self, on: bool) {
        let mut policy = self.store.storage_policy().unwrap();
        policy.play_libraries.single_copy = on;
        self.store.set_storage_policy(&policy, "Operator").unwrap();
    }

    /// Registers the Ingest Folder `in`, which settles at once, holding a TorrentZip `zip` of `members`, and scans it
    /// twice: the first scan sees the item settle, and the second assesses it.
    fn ingest(&self, zip: &str, members: &[&str]) {
        write_zip(&self.tmp.path().join("in").join(zip), members);
        self.store
            .update_settings(
                &stage_select_api::settings::SettingsUpdate {
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
        self.store.register_ingest_folder("in").unwrap();
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self.store.run_ingest_scan_job(job.id).unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
    }

    fn audit(&self) -> stage_select_api::Report {
        let audit = self
            .store
            .audit_source_set("mame", None, AuditEvidence::Full)
            .unwrap();
        self.store.report(audit.id).unwrap()
    }

    fn plan(&self) -> RepairPlan {
        self.store
            .plan_repair("mame", &PlanRepair::default())
            .unwrap()
    }

    /// Approves `plan` and runs its Repair Job, then the Audit it queues, whose report it returns.
    fn approve(&self, plan: &RepairPlan) -> stage_select_api::Report {
        let done = self.run(plan);
        assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
        let audit = done.audit_job.expect("a repair queues an audit");
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, audit);
        let ended = self.store.run_audit_job(audit, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        let latest = &self.store.history("mame").unwrap()[0];
        self.store.report(latest.id).unwrap()
    }

    /// Approves `plan` and runs its Repair Job, whatever steps of it fail, returning the plan as it ran.
    fn run(&self, plan: &RepairPlan) -> RepairPlan {
        self.run_after(plan, || ())
    }

    /// Approves `plan`, then does `meanwhile`, then runs its Repair Job, whatever steps of it fail, returning the plan
    /// as it ran.
    fn run_after(&self, plan: &RepairPlan, meanwhile: impl FnOnce()) -> RepairPlan {
        let approved = self
            .store
            .approve_repair(plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        meanwhile();
        let job = approved.job.expect("an approved repair has a job");
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job);
        let ended = self.store.run_repair_job(job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.store.fulfil_repair_audits().unwrap();
        self.store.repair_plan(plan.id).unwrap()
    }

    /// Each ZIP in the Source Set's folder, with its members sorted, and every other file, kept files included.
    fn listing(&self) -> (Vec<(String, Vec<String>)>, Vec<String>) {
        let mut zips = Vec::new();
        let mut others = Vec::new();
        for e in walkdir::WalkDir::new(self.set()).min_depth(1) {
            let e = e.unwrap();
            if !e.file_type().is_file() {
                continue;
            }
            let rel = e
                .path()
                .strip_prefix(self.set())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            match rel.ends_with(".zip") {
                true => zips.push((rel, members(e.path()))),
                false => others.push(rel),
            }
        }
        zips.sort();
        others.sort();
        (zips, others)
    }
}

/// Writes the TorrentZip `path`, holding `members`.
fn write_zip(path: &Path, members: &[&str]) {
    write_torrentzip(path, &payloads(members));
}

/// Writes the TorrentZip `path`, holding `member` with `bytes`, whatever ROM its name gives.
fn write_zip_of(path: &Path, member: &str, bytes: &[u8]) {
    write_torrentzip(path, &[(member, bytes)]);
}

/// Writes `path` as an ordinary ZIP, not TorrentZip, holding `members`.
fn write_plain_zip(path: &Path, members: &[&str]) {
    stage_select_test_fixtures::write_zip(path, &payloads(members));
}

/// `members` with their payloads.
fn payloads<'a>(members: &[&'a str]) -> Vec<(&'a str, &'static [u8])> {
    members.iter().map(|m| (*m, payload(m))).collect()
}

/// Every file under `dir`, with its bytes, in order of path.
fn contents(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    walkdir::WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| (e.path().to_owned(), std::fs::read(e.path()).unwrap()))
        .collect()
}

/// What becomes of a ZIP after the Audit reads it.
#[derive(Debug, Clone, Copy)]
enum Since {
    /// Replaced by a corrupt file.
    Changed,
    /// Deleted.
    Gone,
}

impl Since {
    /// Replaces or deletes the ZIP at `path`.
    fn apply_to(self, path: &Path) {
        match self {
            Since::Changed => std::fs::write(path, b"not a zip any more").unwrap(),
            Since::Gone => std::fs::remove_file(path).unwrap(),
        }
    }
}

fn members(path: &Path) -> Vec<String> {
    let zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    names.sort();
    names
}

fn statuses(report: &stage_select_api::Report) -> BTreeMap<String, GameStatus> {
    report
        .games
        .iter()
        .map(|g| (g.name.clone(), g.status))
        .collect()
}

/// Every Entry of `report` is Matched on Payload Evidence and every Game is Complete.
fn verified(report: &stage_select_api::Report) {
    for e in &report.entries {
        assert_eq!(e.finding, Finding::Matched, "{e:?}");
        assert_eq!(
            e.evidence.as_ref().map(|ev| ev.level),
            Some(EvidenceLevel::Payload)
        );
    }
    assert!(
        statuses(report)
            .values()
            .all(|s| *s == GameStatus::Complete),
        "{:?}",
        statuses(report)
    );
}

fn expect(layout: SetLayout) -> Vec<(String, Vec<String>)> {
    family(layout)
        .into_iter()
        .map(|(zip, members)| {
            (
                zip.to_owned(),
                members.iter().map(|m| m.to_string()).collect(),
            )
        })
        .collect()
}

#[test]
fn a_family_stored_in_any_layout_audits_to_the_same_game_statuses() {
    let mut seen = Vec::new();
    for layout in SetLayout::ALL {
        let env = Env::new();
        env.stored(layout);
        let report = env.audit();
        verified(&report);
        seen.push(statuses(&report));
    }
    assert!(seen.windows(2).all(|w| w[0] == w[1]), "{seen:?}");
}

#[test]
fn a_merged_family_repaired_non_merged_gets_self_contained_zips_before_its_merged_zip_is_kept_aside()
 {
    let env = Env::new();
    env.stored(SetLayout::Merged);
    env.audit();

    let plan = env.plan();

    for game in ["blaster", "blasterj"] {
        let item = plan
            .content
            .items
            .iter()
            .find(|i| i.key == format!("game:{game}"))
            .unwrap_or_else(|| panic!("{game}: {:#?}", plan.content.items));
        assert_eq!(item.default, RepairChoice::Apply, "{game}");
        assert_eq!(item.kind, RepairKind::Repack, "{game}");
        assert!(
            item.summary.ends_with("as a non-merged set"),
            "{}",
            item.summary
        );
    }
    // The merged ZIP is kept aside only once the clone's ZIP, written from it, is built and verified.
    let steps = &plan.content.steps;
    let clone_build = steps
        .iter()
        .position(|s| s.item == "game:blasterj" && matches!(s.step, RepairStep::Build { .. }))
        .unwrap() as u32;
    let keep = steps
        .iter()
        .find_map(|s| match &s.step {
            RepairStep::Keep { path, needs, .. } if path == "blaster.zip" => Some(needs),
            _ => None,
        })
        .unwrap();
    assert!(keep.contains(&clone_build), "{steps:#?}");

    let report = env.approve(&plan);

    let (zips, others) = env.listing();
    assert_eq!(zips, expect(SetLayout::NonMerged));
    assert_eq!(
        others,
        [
            ".stage-select/marker",
            ".stage-select/retained/1/blaster.zip.kept"
        ]
    );
    for (zip, _) in &zips {
        assert!(is_torrentzip(&env.set().join(zip)), "{zip}");
    }
    verified(&report);
    assert!(
        env.plan().content.items.is_empty(),
        "nothing more to repair"
    );
}

#[test]
fn a_non_merged_family_repaired_split_or_merged_keeps_only_what_each_zip_holds_there() {
    for layout in [SetLayout::Split, SetLayout::Merged] {
        let env = Env::new();
        env.stored(SetLayout::NonMerged);
        env.layout(layout);
        env.audit();

        let report = env.approve(&env.plan());

        let (zips, others) = env.listing();
        assert_eq!(zips, expect(layout), "{layout:?}");
        // Merged, the clone's ZIP is the parent's to read from, kept aside once the merged ZIP is written.
        assert_eq!(
            others,
            [
                ".stage-select/marker",
                ".stage-select/retained/1/blaster.zip.kept",
                ".stage-select/retained/1/blasterj.zip.kept"
            ],
            "{layout:?}"
        );
        verified(&report);
        assert!(
            env.plan().content.items.is_empty(),
            "{layout:?}: nothing more to repair"
        );
    }
}

#[test]
fn a_non_merged_family_as_its_layout_keeps_it_needs_no_repair() {
    let env = Env::new();
    env.stored(SetLayout::NonMerged);
    env.audit();

    assert!(
        env.plan().content.items.is_empty(),
        "{:#?}",
        env.plan().content.items
    );
}

#[test]
fn a_non_merged_family_stays_zips_whatever_format_the_storage_policy_gives_source_sets() {
    let env = Env::new();
    env.stored(SetLayout::NonMerged);
    for format in [StorageFormat::Loose, StorageFormat::LooseFolder] {
        let mut policy = env.store.storage_policy().unwrap();
        policy.source_sets.format = format;
        env.store.set_storage_policy(&policy, "Operator").unwrap();
        env.audit();

        let plan = env.plan();
        assert!(plan.content.items.is_empty(), "{:#?}", plan.content.items);
        assert_eq!(
            plan.content.notes,
            [
                "4 MAME games stay TorrentZip, whatever format you give source sets: MAME reads each game from a ZIP"
            ]
        );
    }
}

#[test]
fn a_non_merged_zip_is_filled_from_a_siblings_copy_of_a_shared_rom_which_stays() {
    let env = Env::new();
    // The clone's ZIP lacks the device's ROM, and so does the device's own: the parent's ZIP holds the one copy.
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("blaster.zip", &["bios.u1", "gfx.u4", "prog.u3", "snd.u2"]);
    env.zip("blasterj.zip", &["bios.u1", "gfxj.u4", "progj.u3"]);
    env.audit();

    let report = env.approve(&env.plan());

    let (zips, others) = env.listing();
    assert_eq!(zips, expect(SetLayout::NonMerged));
    assert_eq!(
        others,
        [
            ".stage-select/marker",
            ".stage-select/retained/1/blasterj.zip.kept"
        ]
    );
    verified(&report);
}

#[test]
fn leaving_one_game_of_a_family_leaves_the_family_as_it_is() {
    let env = Env::new();
    env.stored(SetLayout::Merged);
    env.audit();

    let plan = env
        .store
        .plan_repair(
            "mame",
            &PlanRepair {
                choices: [("game:blasterj".to_owned(), RepairChoice::Leave)].into(),
                additions: Default::default(),
            },
        )
        .unwrap();

    let parent = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "game:blaster")
        .unwrap();
    assert_eq!(parent.chosen, RepairChoice::Leave);
    assert!(
        parent
            .note
            .as_deref()
            .is_some_and(|n| n.contains("of the same family")),
        "{parent:?}"
    );
}

#[test]
fn a_family_whose_zip_holds_a_file_under_a_nodump_roms_name_is_left_as_it_is_by_default() {
    let env = Env::with(listxml(Some("blaster")));
    // Merged, with a file under the parent's nodump ROM's name, which an emulator may still load.
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip("blaster.zip", &["gfx.u4", "pal.u5", "prog.u3", "progj.u3"]);
    env.audit();

    let plan = env.plan();

    for game in ["blaster", "blasterj"] {
        let item = plan
            .content
            .items
            .iter()
            .find(|i| i.key == format!("game:{game}"))
            .unwrap_or_else(|| panic!("{game}: {:#?}", plan.content.items));
        assert_eq!(
            (item.default, item.chosen),
            (RepairChoice::Leave, RepairChoice::Leave),
            "{item:#?}"
        );
    }
}

#[test]
fn a_non_merged_zip_with_a_file_under_its_devices_nodump_roms_name_is_left_in_any_layout() {
    for layout in SetLayout::ALL {
        a_non_merged_zip_with_a_devices_nodump_file_is_left_under(layout);
    }
}

fn a_non_merged_zip_with_a_devices_nodump_file_is_left_under(layout: SetLayout) {
    let env = Env::with(listxml(Some("acmesnd")));
    env.layout(layout);
    // Non-merged, but the parent's ZIP also holds a file under its device's nodump ROM's name.
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip(
        "blaster.zip",
        &["bios.u1", "gfx.u4", "pal.u5", "prog.u3", "snd.u2"],
    );
    env.zip(
        "blasterj.zip",
        &["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"],
    );
    env.audit();

    let plan = env.plan();

    let parent = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "game:blaster")
        .unwrap_or_else(|| panic!("{layout:?}: {:#?}", plan.content.items));
    assert_eq!(
        (parent.default, parent.chosen),
        (RepairChoice::Leave, RepairChoice::Leave),
        "{layout:?}: {parent:#?}"
    );
    assert!(
        parent.note.as_deref().is_some_and(|n| n.contains("pal.u5")),
        "{layout:?}: {parent:#?}"
    );
}

#[test]
fn choosing_to_repair_one_game_of_a_family_another_is_left_by_default_is_refused() {
    let env = Env::with(listxml(Some("blaster")));
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip("blaster.zip", &["gfx.u4", "pal.u5", "prog.u3", "progj.u3"]);
    env.audit();
    let plan = |game: &str| {
        env.store.plan_repair(
            "mame",
            &PlanRepair {
                choices: [(format!("game:{game}"), RepairChoice::Apply)].into(),
                additions: Default::default(),
            },
        )
    };

    let refused = plan("blasterj").unwrap_err().to_string();
    assert!(
        refused.contains("blaster would be left as it is while blasterj is repaired"),
        "{refused}"
    );

    // Choosing to repair the Game whose file is under the nodump ROM's name repairs its family with it.
    let chosen = plan("blaster").unwrap();
    for game in ["blaster", "blasterj"] {
        let item = (chosen.content.items.iter())
            .find(|i| i.key == format!("game:{game}"))
            .unwrap();
        assert_ne!(item.chosen, RepairChoice::Leave, "{item:#?}");
    }
}

#[test]
fn a_placed_game_stays_self_contained_whatever_the_source_sets_layout() {
    let unchanged = |placed: &str| {
        let mut zips = expect(SetLayout::NonMerged);
        zips.retain(|(zip, _)| *zip != format!("{placed}.zip"));
        zips
    };
    let blaster_split = || {
        let mut zips = unchanged("blasterj");
        let parent = zips.iter_mut().find(|(zip, _)| zip == "blaster.zip");
        parent.unwrap().1 = vec!["gfx.u4".into(), "prog.u3".into()];
        zips
    };
    for (layout, placed, set) in [
        (SetLayout::NonMerged, "blaster", unchanged("blaster")),
        (SetLayout::NonMerged, "blasterj", unchanged("blasterj")),
        // The clone of a placed parent stays non-merged, with no parent beside it.
        (SetLayout::Split, "blaster", unchanged("blaster")),
        (SetLayout::Split, "blasterj", blaster_split()),
        (SetLayout::Merged, "blaster", unchanged("blaster")),
        // The merged parent's ZIP holds none of its placed clone's ROMs.
        (SetLayout::Merged, "blasterj", blaster_split()),
    ] {
        a_placed_game_stays_self_contained_under(layout, placed, set);
    }
}

/// The family is stored non-merged with `placed` placed in the Library Folder, its BIOS and device ROMs inside; the
/// Operator changes the Set Layout to `layout` and applies every item as it is by default, which leaves the Source
/// Set's ZIPs as `set` gives.
fn a_placed_game_stays_self_contained_under(
    layout: SetLayout,
    placed: &str,
    set: Vec<(String, Vec<String>)>,
) {
    let env = Env::new();
    let mut stored = family(SetLayout::NonMerged);
    let at = stored
        .iter()
        .position(|(zip, _)| *zip == format!("{placed}.zip"))
        .unwrap();
    let (_, own) = stored.remove(at);
    for (zip, members) in &stored {
        env.zip(zip, members);
    }
    env.place(placed, &own);
    env.layout(layout);
    env.audit();

    let plan = env.plan();

    let items = &plan.content.items;
    assert!(
        items.iter().all(|i| i.game.as_deref() != Some(placed)),
        "{layout:?}, {placed}: {items:#?}"
    );
    let report = env.approve(&plan);
    let zip = env.library().join(format!("{placed}.zip"));
    assert_eq!(members(&zip), own, "{layout:?}, {placed}");
    assert!(is_torrentzip(&zip), "{layout:?}, {placed}");
    assert_eq!(env.listing().0, set, "{layout:?}, {placed}");
    verified(&report);
    let next = env.plan().content.items;
    assert!(
        next.iter().all(|i| i.chosen == RepairChoice::Leave),
        "{layout:?}, {placed}: {next:#?}"
    );
}

#[test]
fn a_placed_game_keeps_its_bios_rom_named_its_own_way_when_the_set_layout_turns_split() {
    let env = Env::with(listxml(None).replacen(
        &rom("bios.u1", Some("bios.u1")),
        &rom("gamebios.u1", Some("bios.u1")),
        1,
    ));
    // The BIOS's ZIP needs repacking: it isn't TorrentZip.
    write_plain_zip(&env.set().join("acmebios.zip"), &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip(
        "blasterj.zip",
        &["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"],
    );
    let own = ["gamebios.u1", "gfx.u4", "prog.u3", "snd.u2"];
    env.place("blaster", &own);
    env.layout(SetLayout::Split);
    env.audit();

    let plan = env.plan();
    assert!(
        (plan.content.items.iter())
            .any(|i| i.key == "game:acmebios" && i.chosen == RepairChoice::Apply),
        "{:#?}",
        plan.content.items
    );
    let report = env.approve(&plan);

    assert_eq!(members(&env.library().join("blaster.zip")), own);
    assert!(is_torrentzip(&env.set().join("acmebios.zip")));
    verified(&report);
}

#[test]
fn a_bios_whose_rom_only_a_placed_game_holds_is_missing_from_the_source_sets_folder() {
    let env = Env::new();
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.place("blaster", &["bios.u1", "gfx.u4", "prog.u3", "snd.u2"]);

    let report = env.audit();
    assert_eq!(
        statuses(&report).get("acmebios"),
        Some(&GameStatus::Missing)
    );

    let plan = env.plan();

    // The placed copy of its ROM is no file of the BIOS's to fill in place of or repack: nothing in the Source Set's
    // folder holds it, so nothing repairs it.
    let items = &plan.content.items;
    assert!(
        items.iter().all(|i| i.game.as_deref() != Some("acmebios")),
        "{items:#?}"
    );
    no_placed_file_in_a_source_set_folder_repair(&plan, "acmebios missing");
}

#[test]
fn a_placed_game_is_repaired_where_it_is_placed_apart_from_its_family() {
    let env = Env::with(listxml(Some("blaster")));
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    // Left by default: it holds a file under its nodump ROM's name.
    env.zip(
        "blaster.zip",
        &["bios.u1", "gfx.u4", "pal.u5", "prog.u3", "snd.u2"],
    );
    let own = ["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"];
    // Placed, and needing repacking: it isn't TorrentZip.
    write_plain_zip(&env.library().join("blasterj.zip"), &own);
    env.placed("blasterj");
    env.layout(SetLayout::Split);
    env.audit();

    let plan = env.plan();

    let item = |game: &str| {
        (plan.content.items.iter())
            .find(|i| i.key == format!("game:{game}"))
            .unwrap_or_else(|| panic!("{game}: {:#?}", plan.content.items))
    };
    assert_eq!(item("blaster").chosen, RepairChoice::Leave);
    let placed = item("blasterj");
    assert_eq!(placed.chosen, RepairChoice::Apply, "{placed:#?}");
    assert!(
        placed.summary.ends_with("as a non-merged set"),
        "{}",
        placed.summary
    );
    env.approve(&plan);
    let zip = env.library().join("blasterj.zip");
    assert_eq!(members(&zip), own);
    assert!(is_torrentzip(&zip));
}

#[test]
fn a_placed_game_whose_zip_is_gone_or_changed_since_the_audit_is_left_where_it_is_placed() {
    for since in [Since::Changed, Since::Gone] {
        a_placed_game_whose_zip_is_gone_or_changed_since_the_audit_is_left_under(since);
    }
}

/// The clone of [`listxml_shared`] is placed as a TorrentZip lacking the program ROM, which no file holds when it is
/// audited; then its ZIP goes or changes as `since` says, and an Ingest Folder holds it whole. The Game is stored where it is
/// placed as the Audit found it, a TorrentZip, so the fill finds that ZIP in the way: nothing is written into the
/// Library Folder, where its ROMs would otherwise be written loose.
fn a_placed_game_whose_zip_is_gone_or_changed_since_the_audit_is_left_under(since: Since) {
    let env = Env::with(listxml_shared());
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip("blaster.zip", &["bios.u1", "extra.u6", "gfx.u4"]);
    env.place("blasterb", &["bios.u1", "gfx.u4"]);
    assert_eq!(
        statuses(&env.audit()).get("blasterb"),
        Some(&GameStatus::Incomplete)
    );
    since.apply_to(&env.library().join("blasterb.zip"));
    env.ingest("blasterb.zip", &["bios.u1", "gfx.u4", "prog.u3"]);
    let before = contents(&env.library());

    let plan = env.plan();

    let items = &plan.content.items;
    assert!(
        plan.content.notes.iter().any(|n| n
            == "blasterb is left as it is: .stage-select/placed/1/blasterb.zip is in the way, and the audit did \
                not read it as it is now"),
        "{since:?}: {:#?}",
        plan.content.notes
    );
    assert!(
        (items.iter())
            .filter(|i| i.game.as_deref() == Some("blasterb"))
            .all(|i| i.chosen == RepairChoice::Leave),
        "{since:?}: {items:#?}"
    );
    applying_by_default_leaves(&env, &env.library(), &before, &plan, &format!("{since:?}"));
}

/// Applying `plan` as it is by default, when it applies anything, leaves every file under `dir` as it was `before`.
fn applying_by_default_leaves(
    env: &Env,
    dir: &Path,
    before: &[(PathBuf, Vec<u8>)],
    plan: &RepairPlan,
    at: &str,
) {
    if (plan.content.items.iter()).any(|i| i.chosen != RepairChoice::Leave) {
        env.approve(plan);
    }
    // Not `assert_eq!`, which would print every file's bytes.
    assert!(contents(dir) == before, "{at}: {:#?}", plan.content.items);
}

/// The BIOS and device of [`listxml`]; a parent on them with a ROM of its own, `extra.u6`; and its clone, which shares
/// every ROM of the parent and may run without `extra.u6`.
fn listxml_optional() -> String {
    let optional =
        rom("extra.u6", Some("extra.u6")).replacen("<rom ", r#"<rom optional="yes" "#, 1);
    format!(
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="acmebios" isbios="yes"><description>Acme BIOS</description>{bios}</machine>
  <machine name="acmesnd" isdevice="yes" runnable="no"><description>Acme Sound</description>{snd}</machine>
  <machine name="blaster" romof="acmebios"><description>Blaster</description>{bios_m}{prog}{gfx}{extra}
    <device_ref name="acmesnd"/></machine>
  <machine name="blasterb" cloneof="blaster" romof="blaster"><description>Blaster (bootleg)</description>{bios_m}{prog_m}{gfx_m}{optional}
    <device_ref name="acmesnd"/></machine>
</mame>
"#,
        bios = rom("bios.u1", None),
        snd = rom("snd.u2", None),
        bios_m = rom("bios.u1", Some("bios.u1")),
        prog = rom("prog.u3", None),
        gfx = rom("gfx.u4", None),
        extra = rom("extra.u6", None),
        prog_m = rom("prog.u3", Some("prog.u3")),
        gfx_m = rom("gfx.u4", Some("gfx.u4")),
    )
}

#[test]
fn a_placed_clone_without_an_optional_rom_its_parent_holds_stays_placed_in_any_layout() {
    for layout in SetLayout::ALL {
        a_placed_clone_without_an_optional_rom_its_parent_holds_stays_placed_under(layout);
    }
}

/// The parent is stored as `layout` keeps it, and its clone, which shares every ROM it needs with the parent, is placed
/// without the optional ROM only the parent's ZIP holds: no item keeps the placed ZIP aside, and applying the plan as it
/// is by default leaves it placed as it is.
fn a_placed_clone_without_an_optional_rom_its_parent_holds_stays_placed_under(layout: SetLayout) {
    let env = Env::with(listxml_optional());
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    // The placed clone is laid out apart, so under split and merged its parent's ZIP holds only the parent's own ROMs.
    match layout {
        SetLayout::NonMerged => env.zip(
            "blaster.zip",
            &["bios.u1", "extra.u6", "gfx.u4", "prog.u3", "snd.u2"],
        ),
        SetLayout::Split | SetLayout::Merged => {
            env.zip("blaster.zip", &["extra.u6", "gfx.u4", "prog.u3"])
        }
    }
    let own = ["bios.u1", "gfx.u4", "prog.u3", "snd.u2"];
    env.place("blasterb", &own);
    env.layout(layout);
    env.audit();

    let plan = env.plan();

    let items = &plan.content.items;
    assert!(
        items.iter().all(|i| i.chosen != RepairChoice::KeepAside
            || !i.path.as_deref().unwrap_or_default().contains("blasterb")),
        "{layout:?}: {items:#?}"
    );
    let notes = &plan.content.notes;
    assert!(
        notes.contains(
            &"blasterb is left as it is where it is placed: no file there holds extra.u6, and no ingest folder \
              holds it whole, with its payloads read"
                .into()
        ),
        "{layout:?}: {notes:#?}"
    );
    if items.iter().any(|i| i.chosen != RepairChoice::Leave) {
        env.approve(&plan);
    }
    let zip = env.library().join("blasterb.zip");
    assert_eq!(members(&zip), own, "{layout:?}");
    let report = env.audit();
    assert_eq!(
        statuses(&report).get("blasterb"),
        Some(&GameStatus::Complete),
        "{layout:?}"
    );
    assert_eq!(
        members(&env.set().join("blaster.zip")),
        match layout {
            SetLayout::NonMerged => vec!["bios.u1", "extra.u6", "gfx.u4", "prog.u3", "snd.u2"],
            SetLayout::Split | SetLayout::Merged => vec!["extra.u6", "gfx.u4", "prog.u3"],
        },
        "{layout:?}"
    );
}

#[test]
fn a_placed_clones_copy_never_makes_the_source_sets_only_copy_a_spare_one_in_any_layout() {
    for layout in SetLayout::ALL {
        a_placed_clones_copy_never_makes_the_source_sets_only_copy_a_spare_one_under(layout);
    }
}

/// The clone is placed, and its parent's ZIP, missing its program ROM, is the Source Set's only copy of the graphics
/// the clone shares: no item keeps that ZIP aside by default.
fn a_placed_clones_copy_never_makes_the_source_sets_only_copy_a_spare_one_under(layout: SetLayout) {
    let env = Env::new();
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip("blaster.zip", &["gfx.u4"]);
    env.place("blasterj", &["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"]);
    env.layout(layout);
    env.audit();

    let plan = env.plan();

    let items = &plan.content.items;
    assert!(
        items.iter().all(|i| i.chosen != RepairChoice::KeepAside
            || !i
                .path
                .as_deref()
                .unwrap_or_default()
                .ends_with("blaster.zip")),
        "{layout:?}: {items:#?}"
    );
    if items.iter().any(|i| i.chosen != RepairChoice::Leave) {
        env.approve(&plan);
    }
    assert_eq!(
        members(&env.set().join("blaster.zip")),
        ["gfx.u4"],
        "{layout:?}"
    );
    assert_eq!(
        members(&env.library().join("blasterj.zip")),
        ["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"],
        "{layout:?}"
    );
}

/// [`listxml_optional`] with no device: the clone shares every ROM it needs with its parent, which needs only its BIOS.
fn listxml_shared() -> String {
    listxml_optional().replace("\n    <device_ref name=\"acmesnd\"/>", "")
}

#[test]
fn an_ingest_fill_of_a_parent_never_keeps_its_placed_clone_aside_in_any_layout() {
    for layout in SetLayout::ALL {
        for single_copy in [true, false] {
            an_ingest_fill_of_a_parent_never_keeps_its_placed_clone_aside_under(
                layout,
                single_copy,
            );
        }
    }
}

/// The parent's ZIP lacks its program ROM, an Ingest Folder holds the parent whole, and the healthy clone, which
/// shares every ROM it needs with the parent, is placed: the parent's fill keeps only files of the Source Set's folder
/// aside, and applying the plan as it is by default leaves the placed clone as it is.
fn an_ingest_fill_of_a_parent_never_keeps_its_placed_clone_aside_under(
    layout: SetLayout,
    single_copy: bool,
) {
    let env = Env::with(listxml_shared());
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    env.zip("blaster.zip", &["extra.u6", "gfx.u4"]);
    let own = ["bios.u1", "gfx.u4", "prog.u3"];
    env.place("blasterb", &own);
    env.single_copy(single_copy);
    env.layout(layout);
    env.audit();
    env.ingest("blaster.zip", &["bios.u1", "extra.u6", "gfx.u4", "prog.u3"]);

    let plan = env.plan();

    let at = format!("{layout:?}, single copy {single_copy}");
    let fill = (plan.content.items.iter())
        .find(|i| i.key == "game:blaster")
        .unwrap_or_else(|| panic!("{at}: {:#?}", plan.content.items));
    assert_eq!(fill.chosen, RepairChoice::Apply, "{at}: {fill:#?}");
    assert!(!fill.summary.contains("blasterb"), "{at}: {}", fill.summary);
    no_placed_file_in_a_source_set_folder_repair(&plan, &at);
    let report = env.approve(&plan);
    assert_eq!(members(&env.library().join("blasterb.zip")), own, "{at}");
    assert_eq!(
        statuses(&report).get("blasterb"),
        Some(&GameStatus::Complete),
        "{at}"
    );
    assert!(
        members(&env.set().join("blaster.zip")).contains(&"prog.u3".to_owned()),
        "{at}"
    );
    let next = env.plan().content.items;
    assert!(
        next.iter().all(|i| i.game.as_deref() != Some("blasterb")),
        "{at}: {next:#?}"
    );
}

#[test]
fn a_placed_clones_repack_never_stands_in_for_the_source_sets_only_copy_of_a_device_rom_in_any_layout()
 {
    for layout in SetLayout::ALL {
        a_placed_clones_repack_never_stands_in_for_the_source_sets_only_copy_of_a_device_rom_under(
            layout,
        );
    }
}

/// The device's ZIP holds only a file under its nodump ROM's name, so it is left by default; the parent, stored
/// non-merged, holds the Source Set's folder's only copy of the device's sound ROM; and the placed clone needs
/// repacking, as it isn't TorrentZip. The clone's repack writes the sound ROM only where it is placed, so the plan as
/// it is by default never strips it from the parent: the device is no more missing after than before.
fn a_placed_clones_repack_never_stands_in_for_the_source_sets_only_copy_of_a_device_rom_under(
    layout: SetLayout,
) {
    let env = Env::with(listxml(Some("acmesnd")));
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["pal.u5"]);
    env.zip("blaster.zip", &["bios.u1", "gfx.u4", "prog.u3", "snd.u2"]);
    let own = ["bios.u1", "gfxj.u4", "progj.u3", "snd.u2"];
    write_plain_zip(&env.library().join("blasterj.zip"), &own);
    env.placed("blasterj");
    env.layout(layout);
    let before = env.audit();

    let plan = env.plan();

    no_placed_file_in_a_source_set_folder_repair(&plan, &format!("{layout:?}"));
    if plan
        .content
        .items
        .iter()
        .all(|i| i.chosen == RepairChoice::Leave)
    {
        return;
    }
    let report = env.approve(&plan);
    assert_eq!(
        statuses(&report).get("acmesnd"),
        statuses(&before).get("acmesnd"),
        "{layout:?}: {:#?}",
        plan.content.items
    );
    let (zips, _) = env.listing();
    assert!(
        zips.iter()
            .any(|(_, members)| members.iter().any(|m| m == "snd.u2")),
        "{layout:?}: {zips:#?}"
    );
    assert_eq!(
        members(&env.library().join("blasterj.zip")),
        own,
        "{layout:?}"
    );
}

#[test]
fn a_file_holding_other_bytes_than_its_merge_parents_rom_is_no_spare_copy() {
    // The clone's graphics are the parent's by `merge=`, alike as the DAT gives them, the parent's by their CRC and the
    // clone's by their SHA-1; but the bytes each DAT line pins differ, so a file of the clone's is no copy of the
    // parent's.
    let env = Env::with(
        listxml(None)
            .replacen(
                &rom("gfx.u4", None),
                &format!(
                    r#"<rom name="gfx.u4" size="8" crc="{:08x}"/>"#,
                    crc32fast::hash(b"graphics")
                ),
                1,
            )
            .replacen(
                &rom("gfxj.u4", Some("gfx.u4")),
                &format!(
                    r#"<rom name="gfxj.u4" merge="gfx.u4" size="8" sha1="{}"/>"#,
                    sha1_hex(b"graphicz")
                ),
                1,
            ),
    );
    env.stored(SetLayout::Split);
    // Under split the clone's ZIP holds none of its graphics, which the parent's ZIP holds as the layout gives them:
    // this holds the only copy of the bytes the clone's line pins.
    write_zip_of(&env.set().join("spare.zip"), "gfxj.u4", b"graphicz");
    env.layout(SetLayout::Split);
    env.audit();

    let plan = env.plan();

    // It holds no copy of a payload the parent's ZIP holds, whatever ROM the two lines are one of.
    assert!(
        (plan.content.items.iter()).all(|i| i.path.as_deref() != Some("spare.zip")),
        "{:#?}",
        plan.content.items
    );
    if (plan.content.items.iter()).any(|i| i.chosen != RepairChoice::Leave) {
        env.approve(&plan);
    }
    assert_eq!(members(&env.set().join("spare.zip")), ["gfxj.u4"]);
}

#[test]
fn a_conflicting_entry_you_choose_to_keep_aside_with_the_last_copy_of_a_rom_says_you_can_restore_it()
 {
    let env = Env::with(listxml_aliased(false));
    // The member is named for the device's ROM, but holds the parent's program, which no other file of the Source
    // Set's folder holds.
    write_zip_of(&env.set().join("acmevga.zip"), "vga.bin", b"program");
    env.audit();
    let conflict = (env.plan().content.items.into_iter())
        .find(|i| i.kind == RepairKind::Conflict)
        .unwrap();

    let plan = env
        .store
        .plan_repair(
            "mame",
            &PlanRepair {
                choices: [(conflict.key.clone(), RepairChoice::KeepAside)].into(),
                ..PlanRepair::default()
            },
        )
        .unwrap();

    let item = (plan.content.items.iter())
        .find(|i| i.key == conflict.key)
        .unwrap();
    assert_eq!(item.chosen, RepairChoice::KeepAside, "{item:#?}");
    assert_eq!(item.default, RepairChoice::Leave, "{item:#?}");
    assert_eq!(
        item.note.as_deref(),
        Some(
            "this sets aside the last copy in the source set of prog.u3 of blaster; you can restore it from the \
             kept files"
        )
    );
}

/// A parent and its clone, which shares the parent's graphics under the same name, as MAME most often lists a shared
/// ROM: the members of their ZIPs are named alike.
fn listxml_same_name() -> String {
    format!(
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="blaster"><description>Blaster</description>{gfx}{prog}</machine>
  <machine name="blasterj" cloneof="blaster" romof="blaster"><description>Blaster (Japan)</description>{gfx_m}{progj}</machine>
</mame>
"#,
        gfx = rom("gfx.u4", None),
        prog = rom("prog.u3", None),
        gfx_m = rom("gfx.u4", Some("gfx.u4")),
        progj = rom("progj.u3", None),
    )
}

#[test]
fn a_zip_gone_or_changed_since_the_audit_never_stands_in_for_a_copy_of_a_shared_rom_in_any_layout()
{
    for stored in SetLayout::ALL {
        for layout in SetLayout::ALL {
            for since in [Since::Changed, Since::Gone] {
                a_zip_gone_or_changed_since_the_audit_never_stands_in_for_a_copy_of_a_shared_rom_under(
                    stored, layout, since,
                );
            }
        }
    }
}

/// The family of [`listxml_same_name`] is stored as `stored` gives it and audited healthy, then the parent's ZIP goes
/// or changes as `since` says, and the layout turns to `layout`. What the Audit read of the parent's ZIP no longer
/// holds, so it holds no copy a repair may count on: the plan as it is by default leaves every file of the Source Set's
/// folder as it is, as no ZIP of the family can be repacked without the parent's ROMs.
fn a_zip_gone_or_changed_since_the_audit_never_stands_in_for_a_copy_of_a_shared_rom_under(
    stored: SetLayout,
    layout: SetLayout,
    since: Since,
) {
    let env = Env::with(listxml_same_name());
    let at = format!("stored {stored:?}, repaired {layout:?}, {since:?}");
    match stored {
        SetLayout::NonMerged => {
            env.zip("blaster.zip", &["gfx.u4", "prog.u3"]);
            env.zip("blasterj.zip", &["gfx.u4", "progj.u3"]);
        }
        SetLayout::Split => {
            env.zip("blaster.zip", &["gfx.u4", "prog.u3"]);
            env.zip("blasterj.zip", &["progj.u3"]);
        }
        SetLayout::Merged => env.zip("blaster.zip", &["gfx.u4", "prog.u3", "progj.u3"]),
    }
    env.layout(layout);
    verified(&env.audit());
    since.apply_to(&env.set().join("blaster.zip"));
    let note = match since {
        Since::Changed => {
            "blaster.zip changed since the audit read it, so it is left as it is; audit again to repair it"
        }
        Since::Gone => "blaster.zip is gone since the audit; audit again to repair what it held",
    };
    let before = contents(&env.set());

    let plan = env.plan();

    assert!(
        plan.content.notes.iter().any(|n| n == note),
        "{at}: {:#?}",
        plan.content.notes
    );
    applying_by_default_leaves(&env, &env.set(), &before, &plan, &at);
}

#[test]
fn a_repair_of_the_source_sets_folder_never_reads_counts_or_displaces_a_placed_file() {
    for layout in SetLayout::ALL {
        for damage in [Damage::Repack, Damage::Spare, Damage::Fill] {
            for placed in ["blaster", "blasterj"] {
                a_repair_of_the_source_sets_folder_never_touches_a_placed_file_under(
                    layout, placed, damage,
                );
            }
            a_repair_of_the_source_sets_folder_never_touches_a_placed_clone_it_shares_with_under(
                layout, damage,
            );
        }
    }
}

/// What is wrong with the Source Set's folder.
#[derive(Debug, Clone, Copy)]
enum Damage {
    /// The other Game's ZIP needs repacking: it is stored merged, or isn't TorrentZip.
    Repack,
    /// A ZIP holds only copies of ROMs other Games hold.
    Spare,
    /// The other Game's ZIP lacks its program ROM, which an Ingest Folder holds it whole with.
    Fill,
}

/// `placed` is placed self-contained, and the Source Set's folder is damaged as `damage` says.
fn a_repair_of_the_source_sets_folder_never_touches_a_placed_file_under(
    layout: SetLayout,
    placed: &str,
    damage: Damage,
) {
    let env = Env::new();
    let at = format!("{layout:?}, {placed} placed, {damage:?}");
    env.zip("acmebios.zip", &["bios.u1"]);
    env.zip("acmesnd.zip", &["snd.u2"]);
    let mut stored = family(SetLayout::NonMerged);
    let own = stored
        .iter()
        .position(|(zip, _)| *zip == format!("{placed}.zip"))
        .map(|at| stored.remove(at).1)
        .unwrap();
    env.place(placed, &own);
    let other = stored
        .iter()
        .find(|(zip, _)| !zip.starts_with("acme"))
        .cloned()
        .unwrap();
    match damage {
        Damage::Repack => env.zip(other.0, &["gfx.u4", "prog.u3", "progj.u3"]),
        Damage::Spare => {
            env.zip(other.0, &other.1);
            env.zip("copies.zip", &["bios.u1", "gfx.u4"]);
        }
        Damage::Fill => {
            // An Ingest Folder item holds the ROMs one Game lists, never a device's.
            let own: Vec<&str> = (other.1.iter().copied())
                .filter(|m| *m != "snd.u2")
                .collect();
            let full: Vec<&str> = (other.1.iter().copied())
                .filter(|m| !m.starts_with("prog"))
                .collect();
            env.zip(other.0, &full);
            env.ingest(other.0, &own);
        }
    }
    env.layout(layout);
    env.audit();
    every_item_of_the_source_sets_folder_stays_there(&env, &at);
}

/// The clone of [`listxml_shared`], which shares every ROM it needs with its parent, is placed, and the parent's ZIP
/// is damaged as `damage` says: a repair of the parent finds every ROM it might take from the clone's ZIP already
/// written, so only the boundary keeps the placed ZIP out of it.
fn a_repair_of_the_source_sets_folder_never_touches_a_placed_clone_it_shares_with_under(
    layout: SetLayout,
    damage: Damage,
) {
    let env = Env::with(listxml_shared());
    let at = format!("{layout:?}, shared clone placed, {damage:?}");
    env.zip("acmebios.zip", &["bios.u1"]);
    env.place("blasterb", &["bios.u1", "gfx.u4", "prog.u3"]);
    match damage {
        Damage::Repack => write_plain_zip(
            &env.set().join("blaster.zip"),
            &["extra.u6", "gfx.u4", "prog.u3"],
        ),
        Damage::Spare => {
            env.zip("blaster.zip", &["extra.u6", "gfx.u4", "prog.u3"]);
            env.zip("copies.zip", &["gfx.u4", "prog.u3"]);
        }
        Damage::Fill => {
            env.zip("blaster.zip", &["extra.u6", "gfx.u4"]);
            env.ingest("blaster.zip", &["bios.u1", "extra.u6", "gfx.u4", "prog.u3"]);
        }
    }
    env.layout(layout);
    env.audit();
    every_item_of_the_source_sets_folder_stays_there(&env, &at);
}

/// Neither the plan as it is by default nor one with every item chosen to be applied or kept aside has an item of the
/// Source Set's folder that reads, writes, renames or keeps aside a placed file. When it applies anything by default,
/// applying it leaves every placed file as it is and no Game missing a ROM it had: no placed copy stood in for the last
/// one in the Source Set's folder.
fn every_item_of_the_source_sets_folder_stays_there(env: &Env, at: &str) {
    let before = env.audit();
    let plan = env.plan();
    no_placed_file_in_a_source_set_folder_repair(&plan, at);
    let choices = (plan.content.items.iter())
        .filter(|i| !i.key.starts_with("conflict:"))
        .filter_map(|i| {
            let apply = (i.choices.iter())
                .find(|c| matches!(c, RepairChoice::Apply | RepairChoice::KeepAside))?;
            Some((i.key.clone(), *apply))
        })
        .collect();
    let chosen = env
        .store
        .plan_repair(
            "mame",
            &PlanRepair {
                choices,
                ..PlanRepair::default()
            },
        )
        .unwrap();
    no_placed_file_in_a_source_set_folder_repair(&chosen, &format!("{at}, every item chosen"));
    if plan
        .content
        .items
        .iter()
        .all(|i| i.chosen == RepairChoice::Leave)
    {
        return;
    }
    let placed = contents(&env.library());
    let held = rom_payloads(env);
    let after = env.approve(&plan);
    let lost = lost_since(&held, env);
    assert!(
        lost.is_empty(),
        "{at}: lost {lost:?}\n{:#?}",
        plan.content.items
    );
    for (path, bytes) in &placed {
        assert_eq!(
            &std::fs::read(path).unwrap(),
            bytes,
            "{at}: {}",
            path.display()
        );
    }
    for game in &before.games {
        let now = (after.games.iter().find(|g| g.name == game.name))
            .unwrap_or_else(|| panic!("{at}: no {}", game.name));
        let lost: Vec<&String> = (now.missing_roms.iter())
            .filter(|r| !game.missing_roms.contains(r))
            .collect();
        assert!(
            lost.is_empty()
                && (now.status != GameStatus::Missing || game.status == GameStatus::Missing),
            "{at}: {} lost {lost:?}, now {:?}\n{:#?}",
            game.name,
            now.status,
            plan.content.items
        );
    }
}

/// No step of an item of a Game in the Source Set's folder, or of a file there, names a placed file: it reads, writes,
/// renames and keeps aside only files of the Source Set's folder.
fn no_placed_file_in_a_source_set_folder_repair(plan: &RepairPlan, at: &str) {
    let placed_items: Vec<&str> = (plan.content.items.iter())
        .filter(|i| i.path.as_deref().is_some_and(|p| split_placed(p).is_some()))
        .map(|i| i.key.as_str())
        .collect();
    for step in &plan.content.steps {
        if placed_items.contains(&step.item.as_str()) {
            continue;
        }
        let paths: Vec<&str> = match &step.step {
            RepairStep::Build {
                target, sources, ..
            } => std::iter::once(target.as_str())
                .chain(sources.keys().map(String::as_str))
                .collect(),
            RepairStep::Keep { path, .. } => vec![path.as_str()],
            RepairStep::Rename { from, to, .. } => vec![from.as_str(), to.as_str()],
            RepairStep::Publish { target, .. } | RepairStep::Fill { target, .. } => {
                vec![target.as_str()]
            }
            RepairStep::Delete { path, .. } => vec![path.as_str()],
        };
        assert!(
            paths.iter().all(|p| split_placed(p).is_none()),
            "{at}: {} touches a placed file: {step:#?}\n{:#?}",
            step.item,
            plan.content.items
        );
    }
}

/// A graphics device, and a second one that lists the first's ROM under a name of its own, by `merge=`, as MAME's
/// `tandy_1000x_graphics` does `tandy_1000_graphics`'s; a parent; and its clone, which needs the first device. With
/// `optional`, the clone may run without the device's ROM.
fn listxml_aliased(optional: bool) -> String {
    let vga = match optional {
        true => rom("vga.bin", None).replacen("<rom ", r#"<rom optional="yes" "#, 1),
        false => rom("vga.bin", None),
    };
    format!(
        r#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="acmevga" isdevice="yes" runnable="no"><description>Acme VGA</description>{vga}</machine>
  <machine name="acmevgax" isdevice="yes" runnable="no" romof="acmevga">
    <description>Acme VGA X</description>{vgax}</machine>
  <machine name="blaster"><description>Blaster</description>{prog}{gfx}</machine>
  <machine name="blasterj" cloneof="blaster" romof="blaster"><description>Blaster (Japan)</description>{progj}{gfxj}
    <device_ref name="acmevga"/></machine>
</mame>
"#,
        vgax = rom("vgax.bin", Some("vga.bin")),
        prog = rom("prog.u3", None),
        gfx = rom("gfx.u4", None),
        progj = rom("progj.u3", None),
        gfxj = rom("gfxj.u4", Some("gfx.u4")),
    )
}

#[test]
fn an_ingest_fill_never_counts_a_dependency_rom_it_doesnt_write_in_any_layout() {
    for layout in SetLayout::ALL {
        an_ingest_fill_never_counts_a_dependency_rom_it_doesnt_write_under(layout, false);
    }
}

#[test]
fn an_ingest_fill_never_counts_an_optional_dependency_rom_it_doesnt_write_in_any_layout() {
    for layout in SetLayout::ALL {
        an_ingest_fill_never_counts_a_dependency_rom_it_doesnt_write_under(layout, true);
    }
}

/// The parent is placed, so its missing clone is laid out non-merged; the clone needs the first graphics device, whose
/// ZIP holds a conflicting member and is left by default; the second device's ZIP holds the Source Set's only usable
/// copy of the device's ROM, under its own name; and an Ingest Folder holds the clone's own ROMs, without the device's.
/// With `optional`, the device's ROM is optional, and the clone wants it as an Ingest Folder holds the device whole
/// too. A fill of the clone writes only the clone's own ROMs, so applying the plan as it is by default leaves the
/// device's ROM in the Source Set's folder.
fn an_ingest_fill_never_counts_a_dependency_rom_it_doesnt_write_under(
    layout: SetLayout,
    optional: bool,
) {
    let env = Env::with(listxml_aliased(optional));
    let at = format!("{layout:?}, optional {optional}");
    // The member is named for the device's ROM, but holds the parent's program.
    write_zip_of(&env.set().join("acmevga.zip"), "vga.bin", b"program");
    env.zip("acmevgax.zip", &["vgax.bin"]);
    env.place("blaster", &["gfx.u4", "prog.u3"]);
    env.layout(layout);
    env.audit();
    if optional {
        // The clone wants the optional ROM, as an Ingest Folder holds the device whole, whose ZIP is in the way.
        write_zip(&env.tmp.path().join("in/acmevga.zip"), &["vga.bin"]);
    }
    env.ingest("blasterj.zip", &["gfxj.u4", "progj.u3"]);

    let plan = env.plan();

    let conflict = (plan.content.items.iter())
        .find(|i| i.kind == RepairKind::Conflict)
        .unwrap_or_else(|| panic!("{at}: {:#?}", plan.content.items));
    assert_eq!(conflict.chosen, RepairChoice::Leave, "{at}: {conflict:#?}");
    assert!(
        (plan.content.items.iter())
            .all(|i| i.path.as_deref() != Some("acmevgax.zip") || i.chosen == RepairChoice::Leave),
        "{at}: {:#?}",
        plan.content.items
    );
    applying_by_default_loses_no_payload(&env, &plan, &at);
}

/// Every ROM payload a file holds before `plan` is applied as it is by default, in the Source Set's folder or where a
/// Game is placed, some file there still holds after, outside the Kept Files: no copy is only kept aside.
fn applying_by_default_loses_no_payload(env: &Env, plan: &RepairPlan, at: &str) {
    let before = rom_payloads(env);
    if (plan.content.items.iter()).any(|i| i.chosen != RepairChoice::Leave) {
        env.approve(plan);
    }
    let lost = lost_since(&before, env);
    assert!(
        lost.is_empty(),
        "{at}: lost {lost:?}\n{:#?}",
        plan.content.items
    );
}

/// Each of the ROM payloads `held` that no file holds now, outside the Kept Files ([`rom_payloads`]).
fn lost_since(held: &BTreeSet<Vec<u8>>, env: &Env) -> Vec<String> {
    (held.difference(&rom_payloads(env)))
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// Each payload of a ROM that a file in the Source Set's folder or the Library Folder holds, as a member of a ZIP or as
/// a loose file, leaving out the Kept Files.
fn rom_payloads(env: &Env) -> BTreeSet<Vec<u8>> {
    let known: Vec<&[u8]> = ROMS.iter().map(|r| payload(r)).collect();
    let mut held = BTreeSet::new();
    for dir in [env.set(), env.library()] {
        for e in walkdir::WalkDir::new(&dir).min_depth(1) {
            let e = e.unwrap();
            let rel = e.path().strip_prefix(&dir).unwrap();
            if !e.file_type().is_file() || rel.starts_with(".stage-select") {
                continue;
            }
            let bytes = std::fs::read(e.path()).unwrap();
            let payloads = match zip::ZipArchive::new(std::io::Cursor::new(&bytes)) {
                Ok(mut zip) => (0..zip.len())
                    .map(|n| {
                        let mut member = Vec::new();
                        std::io::Read::read_to_end(&mut zip.by_index(n).unwrap(), &mut member)
                            .unwrap();
                        member
                    })
                    .collect(),
                Err(_) => vec![bytes],
            };
            held.extend(
                payloads
                    .into_iter()
                    .filter(|p| known.contains(&p.as_slice())),
            );
        }
    }
    held
}

/// Each case [`family_without`] sets up: the layout the family is stored as, the layout it turns, the ZIP missing,
/// the Game placed, and whether an Ingest Folder fills the missing Game.
fn cases() -> Vec<(
    SetLayout,
    SetLayout,
    &'static str,
    Option<&'static str>,
    bool,
)> {
    let mut cases = Vec::new();
    for stored in SetLayout::ALL {
        for layout in SetLayout::ALL {
            for (zip, _) in family(stored) {
                for placed in [None, Some("blaster"), Some("blasterj")] {
                    for fill in [false, true] {
                        let game = zip.trim_end_matches(".zip");
                        // Only a Game of the Source Set's folder, never a device, is filled.
                        if !fill || (!game.starts_with("acme") && placed != Some(game)) {
                            cases.push((stored, layout, zip, placed, fill));
                        }
                    }
                }
            }
        }
    }
    cases
}

#[test]
fn a_default_plan_in_any_layout_only_repacks_and_never_leaves_a_rom_payload_only_in_the_kept_files()
{
    for (stored, layout, missing, placed, fill) in cases() {
        let at = format!(
            "stored {stored:?}, repaired {layout:?}, no {missing}, {placed:?} placed, fill {fill}"
        );
        let env = family_without(stored, layout, missing, placed, fill);

        let plan = env.plan();

        let items = &plan.content.items;
        assert!(
            (items.iter())
                .all(|i| i.kind != RepairKind::KeepAside || i.chosen == RepairChoice::Leave),
            "{at}: {items:#?}"
        );
        // A file is kept aside only as the original of what its item writes.
        let steps = &plan.content.steps;
        let writes = |item: &str| {
            steps.iter().any(|s| {
                s.item == item
                    && matches!(
                        s.step,
                        RepairStep::Build { .. }
                            | RepairStep::Rename { .. }
                            | RepairStep::Fill { .. }
                    )
            })
        };
        assert!(
            (steps.iter()).all(|s| !matches!(s.step, RepairStep::Keep { .. }) || writes(&s.item)),
            "{at}: {steps:#?}"
        );
        applying_by_default_loses_no_payload(&env, &plan, &at);
    }
}

/// The family is stored as `stored` keeps it without `missing`, with `placed` placed self-contained and a ZIP of copies
/// of shared ROMs; with `fill`, an Ingest Folder item holds the ROMs the missing Game lists, never its device's. The
/// layout turns `layout`.
fn family_without(
    stored: SetLayout,
    layout: SetLayout,
    missing: &str,
    placed: Option<&str>,
    fill: bool,
) -> Env {
    let env = Env::new();
    let non_merged = |game: &str| {
        (family(SetLayout::NonMerged).into_iter())
            .find(|(zip, _)| *zip == format!("{game}.zip"))
            .map(|(_, members)| members)
            .unwrap()
    };
    for (zip, members) in family(stored) {
        if zip != missing && Some(zip.trim_end_matches(".zip")) != placed {
            env.zip(zip, &members);
        }
    }
    if let Some(game) = placed {
        env.place(game, &non_merged(game));
    }
    env.zip("copies.zip", &["bios.u1", "gfx.u4", "snd.u2"]);
    env.layout(layout);
    env.audit();
    if fill {
        let own: Vec<&str> = (non_merged(missing.trim_end_matches(".zip")).into_iter())
            .filter(|m| *m != "snd.u2")
            .collect();
        env.ingest(missing, &own);
    }
    env
}

#[test]
fn a_rom_whose_other_copy_a_fill_would_write_stays_out_of_the_kept_files_when_the_fill_fails() {
    for layout in [SetLayout::Split, SetLayout::NonMerged] {
        for keep_aside in [false, true] {
            a_rom_whose_other_copy_a_fill_would_write_stays_out_of_the_kept_files_under(
                layout, keep_aside,
            );
        }
    }
}

/// The parent's ZIP holds a conflicting member, so it is left by default, and a loose file holds the Source Set's only
/// copy of its graphics. The clone is missing, and an Ingest Folder holds it with its graphics under its own name, the
/// parent's by `merge=`. The plan fills the clone, and the loose file, a copy of what the fill writes, is left by
/// default; with `keep_aside`, the Operator chose to keep it aside. The Ingest Folder's ZIP is gone once the plan is
/// approved, so the fill fails, and the loose file's graphics stay in the Source Set's folder either way.
fn a_rom_whose_other_copy_a_fill_would_write_stays_out_of_the_kept_files_under(
    layout: SetLayout,
    keep_aside: bool,
) {
    let at = format!("{layout:?}, set aside {keep_aside}");
    let env = Env::with(listxml_aliased(false));
    env.zip("acmevga.zip", &["vga.bin"]);
    // The member is named for the parent's program, but holds the graphics device's ROM.
    write_zip_of(&env.set().join("blaster.zip"), "prog.u3", b"video");
    std::fs::write(env.set().join("gfx.u4"), payload("gfx.u4")).unwrap();
    env.layout(layout);
    env.audit();
    env.ingest("blasterj.zip", &["gfxj.u4", "progj.u3"]);

    let plan = env.plan();
    let spare = (plan.content.items.iter())
        .find(|i| i.key == "file:gfx.u4")
        .unwrap_or_else(|| panic!("{at}: {:#?}", plan.content.items));
    assert_eq!(spare.default, RepairChoice::Leave, "{at}: {spare:#?}");
    let plan = match keep_aside {
        true => env
            .store
            .plan_repair(
                "mame",
                &PlanRepair {
                    choices: [(spare.key.clone(), RepairChoice::KeepAside)].into(),
                    ..PlanRepair::default()
                },
            )
            .unwrap(),
        false => plan,
    };
    let fill = (plan.content.items.iter())
        .find(|i| i.key == "game:blasterj")
        .unwrap_or_else(|| panic!("{at}: {:#?}", plan.content.items));
    assert_eq!(fill.chosen, RepairChoice::Apply, "{at}: {fill:#?}");
    let steps = &plan.content.steps;
    let keep = (steps.iter()).find_map(|s| match &s.step {
        RepairStep::Keep { path, needs, .. } if path == "gfx.u4" => Some(needs),
        _ => None,
    });
    let fill_step = (steps.iter()).position(|s| matches!(s.step, RepairStep::Fill { .. }));
    match keep_aside {
        // The Keep waits for the fill that writes the other copy.
        true => assert!(
            keep.zip(fill_step)
                .is_some_and(|(needs, f)| needs.contains(&(f as u32))),
            "{at}: {steps:#?}"
        ),
        false => assert!(keep.is_none(), "{at}: {steps:#?}"),
    }

    let ran = env.run_after(&plan, || {
        std::fs::remove_file(env.tmp.path().join("in/blasterj.zip")).unwrap()
    });

    let outcome = |item: &str| (ran.outcomes.iter()).find(|o| o.item == item);
    assert!(
        outcome("game:blasterj").is_some_and(|o| !o.done),
        "{at}: {:#?}",
        ran.outcomes
    );
    if keep_aside {
        assert!(
            outcome("file:gfx.u4").is_some_and(|o| !o.done),
            "{at}: {:#?}",
            ran.outcomes
        );
    }
    assert!(
        rom_payloads(&env).contains(payload("gfx.u4")),
        "{at}: {:#?}\n{:#?}",
        plan.content,
        ran.outcomes
    );
    assert!(env.set().join("gfx.u4").is_file(), "{at}");
}

#[test]
fn a_merged_family_whose_clone_zip_is_misnamed_is_repacked_split_with_the_zip_renamed() {
    let env = Env::new();
    env.stored(SetLayout::Merged);
    // The clone's ZIP as split keeps it, under another name: its ROM is also in the merged ZIP, kept aside before the
    // Rename, which never needs waiting for, as the file it moves holds the ROM whether it is done or not.
    env.zip("blasterj (copy).zip", &["progj.u3"]);
    env.layout(SetLayout::Split);
    env.audit();

    let plan = env.plan();

    let items = &plan.content.items;
    for game in ["game:blaster", "game:blasterj"] {
        let item = (items.iter()).find(|i| i.key == game);
        assert!(
            item.is_some_and(|i| i.chosen != RepairChoice::Leave),
            "{game}: {items:#?}"
        );
    }
    let report = env.approve(&plan);
    assert_eq!(
        members(&env.set().join("blaster.zip")),
        ["gfx.u4", "prog.u3"]
    );
    assert_eq!(members(&env.set().join("blasterj.zip")), ["progj.u3"]);
    verified(&report);
}
