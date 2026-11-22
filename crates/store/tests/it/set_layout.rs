//! Set Layouts of MAME Source Sets (ADR 0026): a Repair repacks a family towards the layout the Storage Policy gives,
//! filling from sibling ZIPs, and keeps a ZIP aside only once every copy written from it is verified.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use title_select_api::build::AddLibraryFolder;
use title_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan, RepairStep};
use title_select_api::selection::SelectionProfile;
use title_select_api::storage::{SetLayout, SystemStorage, split_placed};
use title_select_api::{AuditEvidence, EvidenceLevel, Finding, GameStatus, JobState};
use title_select_engine::build::is_torrentzip;
use title_select_engine::torrentzip;
use title_select_store::config::Config;
use title_select_store::{Store, Workers};

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
        "extra.u6" => b"extra",
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
    let p = payload(name);
    let sha1 = sha1_hex(p);
    let merge = merge
        .map(|m| format!(r#" merge="{m}""#))
        .unwrap_or_default();
    format!(
        r#"<rom name="{name}"{merge} size="{}" crc="{:08x}" sha1="{sha1}"/>"#,
        p.len(),
        crc32fast::hash(p)
    )
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
    config: Config,
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
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"mame\"]\n\n[roots.play]\npath = {:?}\nwritable = true\n\n\
             [roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
            sets.display().to_string(),
            tmp.path().join("play").display().to_string(),
            tmp.path().join("in").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let (revision, _) = store
            .import_dat_bytes(xml.as_bytes(), "mame.xml", None)
            .unwrap();
        store
            .add_source_set(&config, "sets", "mame", &revision.dat)
            .unwrap();
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
            .add_library_folder(
                &config,
                &AddLibraryFolder {
                    root: "play".into(),
                    path: Some("arcade".into()),
                    template: None,
                    source_set: "mame".into(),
                    profile: "Mine".into(),
                },
            )
            .unwrap()
            .id;
        Env {
            tmp,
            store,
            config,
            folder,
        }
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
            .record_placement(&self.config, "mame", game, self.folder, &[&name])
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
                &self.config,
                &title_select_api::settings::SettingsUpdate {
                    ingest_folders: Some(
                        [(
                            "in".to_owned(),
                            title_select_api::settings::IngestCadence {
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
        self.store
            .register_ingest_folder(&self.config, "in")
            .unwrap();
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self
                .store
                .run_ingest_scan_job(&self.config, job.id)
                .unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
    }

    fn audit(&self) -> title_select_api::Report {
        let audit = self
            .store
            .audit_source_set(&self.config, "mame", None, AuditEvidence::Full)
            .unwrap();
        self.store.report(audit.id).unwrap()
    }

    fn plan(&self) -> RepairPlan {
        self.store
            .plan_repair(&self.config, "mame", &PlanRepair::default())
            .unwrap()
    }

    /// Approves `plan` and runs its Repair Job, then the Audit it queues, whose report it returns.
    fn approve(&self, plan: &RepairPlan) -> title_select_api::Report {
        let approved = self
            .store
            .approve_repair(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        let job = approved.job.expect("an approved repair has a job");
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job);
        let ended = self.store.run_repair_job(&self.config, job, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        self.store.fulfil_repair_audits().unwrap();
        let done = self.store.repair_plan(plan.id).unwrap();
        assert!(done.outcomes.iter().all(|o| o.done), "{:#?}", done.outcomes);
        let audit = done.audit_job.expect("a repair queues an audit");
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, audit);
        let ended = self.store.run_audit_job(&self.config, audit, None).unwrap();
        assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        let latest = &self.store.history("mame").unwrap()[0];
        self.store.report(latest.id).unwrap()
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
    let mut members = members.to_vec();
    members.sort_by(|a, b| torrentzip::compare_names(a, b));
    let file = std::fs::File::create_new(path).unwrap();
    let mut writer = torrentzip::Writer::new(file).unwrap();
    for m in members {
        let p = payload(m);
        writer.add(m, p.len() as u64, &mut &p[..]).unwrap();
    }
    writer.finish().unwrap();
}

/// Writes `path` as an ordinary ZIP, not TorrentZip, holding `members`.
fn write_plain_zip(path: &Path, members: &[&str]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create_new(path).unwrap());
    for m in members {
        zip.start_file(*m, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, payload(m)).unwrap();
    }
    zip.finish().unwrap();
}

fn members(path: &Path) -> Vec<String> {
    let zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    names.sort();
    names
}

fn statuses(report: &title_select_api::Report) -> BTreeMap<String, GameStatus> {
    report
        .games
        .iter()
        .map(|g| (g.name.clone(), g.status))
        .collect()
}

/// Every Entry of `report` is Matched on Payload Evidence and every Game is Complete.
fn verified(report: &title_select_api::Report) {
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
            ".title-select/marker",
            ".title-select/retained/1/blaster.zip.kept"
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
                ".title-select/marker",
                ".title-select/retained/1/blaster.zip.kept",
                ".title-select/retained/1/blasterj.zip.kept"
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
            ".title-select/marker",
            ".title-select/retained/1/blasterj.zip.kept"
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
            &env.config,
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
            &env.config,
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
fn a_file_kept_aside_by_default_that_holds_the_last_copy_of_a_rom_is_left() {
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
    let file = std::fs::File::create_new(env.set().join("spare.zip")).unwrap();
    let mut writer = torrentzip::Writer::new(file).unwrap();
    writer.add("gfxj.u4", 8, &mut &b"graphicz"[..]).unwrap();
    writer.finish().unwrap();
    env.layout(SetLayout::Split);
    env.audit();

    let plan = env.plan();

    let spare = (plan.content.items.iter())
        .find(|i| i.path.as_deref() == Some("spare.zip"))
        .unwrap_or_else(|| panic!("{:#?}", plan.content.items));
    assert_eq!(spare.default, RepairChoice::Leave, "{spare:#?}");
    assert_eq!(spare.chosen, RepairChoice::Leave, "{spare:#?}");
    assert_eq!(
        spare.note.as_deref(),
        Some(
            "left as it is: it holds the last copy in the source set of gfxj.u4 of blasterj; if you choose to keep \
             it aside, you can restore it from the kept files"
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
fn a_zip_changed_since_the_audit_never_stands_in_for_a_copy_of_a_shared_rom_in_any_layout() {
    for stored in SetLayout::ALL {
        for layout in SetLayout::ALL {
            a_zip_changed_since_the_audit_never_stands_in_for_a_copy_of_a_shared_rom_under(
                stored, layout,
            );
        }
    }
}

/// The family of [`listxml_same_name`] is stored as `stored` gives it and audited healthy, then the parent's ZIP is
/// replaced by a corrupt file, and the layout turns to `layout`. The audit's word for the parent's ZIP is stale, so it
/// holds no copy a repair may count on: the plan as it is by default leaves every file of the Source Set's folder as it
/// is, as no ZIP of the family can be repacked without the parent's ROMs.
fn a_zip_changed_since_the_audit_never_stands_in_for_a_copy_of_a_shared_rom_under(
    stored: SetLayout,
    layout: SetLayout,
) {
    let env = Env::with(listxml_same_name());
    let at = format!("stored {stored:?}, repaired {layout:?}");
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
    std::fs::write(env.set().join("blaster.zip"), b"not a zip any more").unwrap();
    let files = || -> Vec<(PathBuf, Vec<u8>)> {
        walkdir::WalkDir::new(env.set())
            .sort_by_file_name()
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().is_file())
            .map(|e| (e.path().to_owned(), std::fs::read(e.path()).unwrap()))
            .collect()
    };
    let before = files();

    let plan = env.plan();

    assert!(
        plan.content.notes.iter().any(|n| n
            == "blaster.zip changed since the audit read it, so it is left as it is; audit again to repair it"),
        "{at}: {:#?}",
        plan.content.notes
    );
    if plan
        .content
        .items
        .iter()
        .any(|i| i.chosen != RepairChoice::Leave)
    {
        env.approve(&plan);
    }
    assert!(files() == before, "{at}: {:#?}", plan.content.items);
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
            &env.config,
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
    let placed: Vec<(PathBuf, Vec<u8>)> = walkdir::WalkDir::new(env.library())
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| (e.path().to_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    let after = env.approve(&plan);
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
