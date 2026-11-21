//! Set Layouts of MAME Source Sets (ADR 0026): a Repair repacks a family towards the layout the Storage Policy gives,
//! filling from sibling ZIPs, and keeps a ZIP aside only once every copy written from it is verified.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use title_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan, RepairStep};
use title_select_api::storage::{SetLayout, StoragePolicy, SystemStorage};
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
        // Under the name of a nodump ROM, which no file matches.
        "pal.u5" => b"pal?",
        other => panic!("no ROM {other}"),
    }
}

fn rom(name: &str, merge: Option<&str>) -> String {
    let p = payload(name);
    let sha1: String = <sha1::Sha1 as sha1::Digest>::digest(p)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
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

/// A store with the MAME Source Set `mame` in the Root `sets`, which lets repairs write it.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
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
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\nwritable_paths = [\"mame\"]\n",
            sets.display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let (revision, _) = store
            .import_dat_bytes(xml.as_bytes(), "mame.xml", None)
            .unwrap();
        store
            .add_source_set(&config, "sets", "mame", &revision.dat)
            .unwrap();
        Env { tmp, store, config }
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
        let mut members = members.to_vec();
        members.sort_by(|a, b| torrentzip::compare_names(a, b));
        let file = std::fs::File::create_new(self.set().join(zip)).unwrap();
        let mut writer = torrentzip::Writer::new(file).unwrap();
        for m in members {
            let p = payload(m);
            writer.add(m, p.len() as u64, &mut &p[..]).unwrap();
        }
        writer.finish().unwrap();
    }

    /// Sets the MAME system's set layout.
    fn layout(&self, layout: SetLayout) {
        let mut policy = StoragePolicy::default();
        policy.source_sets.systems.insert(
            "MAME/MAME".into(),
            SystemStorage {
                set_layout: Some(layout),
                ..SystemStorage::default()
            },
        );
        self.store.set_storage_policy(&policy, "Operator").unwrap();
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
