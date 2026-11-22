//! A MAME Source Set's Disks (ADR 0026): CHDs in their family's folder, `<parent>/<disk>.chd`. An Audit proves one with
//! `chdman verify` and an incremental one reuses that; an Ingest Folder's folder named for a Game, holding only CHDs,
//! is assessed CHD by CHD, each verified before a repair files it into its family's folder; and a repair never sets a
//! Disk aside, nor writes one into a ZIP.
//!
//! The tests that run chdman find it as the store does: `TITLE_SELECT_CHDMAN`, else `chdman` on the `PATH`. Where it is
//! missing they are skipped, except in CI, where they fail.

use std::path::{Path, PathBuf};

use crate::common::*;
use title_select_api::ingest::{ItemState, Need, Proposal};
use title_select_api::repair::{PlanRepair, RepairChoice, RepairKind, RepairPlan};
use title_select_api::storage::{SetLayout, StoragePolicy, SystemStorage};
use title_select_api::{AuditEvidence, EvidenceLevel, Finding, GameStatus, JobState, Reuse};
use title_select_store::config::Config;
use title_select_store::{Store, Workers};

const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// chdman, as the store finds it; `None`, outside CI, where it isn't there.
fn chdman() -> Option<PathBuf> {
    match title_select_engine::chd::locate(None) {
        Ok(path) if path.is_file() => Some(path),
        found if std::env::var_os("CI").is_some() => {
            panic!("CI runs these tests with chdman: {found:?}")
        }
        _ => {
            eprintln!("skipped: chdman isn't here; set TITLE_SELECT_CHDMAN to run this test");
            None
        }
    }
}

/// Makes a CHD of 64 KiB of noise from `seed` at `path` with chdman, with `extra` arguments, such as a parent to make
/// it a delta CHD against; returns its combined SHA-1.
fn make_chd(chdman: &Path, path: &Path, seed: u64, extra: &[&std::ffi::OsStr]) -> String {
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let raw = scratch.path().join("disk.raw");
    std::fs::write(&raw, noise(seed, 64 * 1024)).unwrap();
    let out = std::process::Command::new(chdman)
        .args(["createraw", "-hs", "4096", "-us", "512", "-i"])
        .arg(&raw)
        .arg("-o")
        .arg(path)
        .args(extra)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    title_select_engine::chd::header(&std::fs::File::open(path).unwrap())
        .unwrap()
        .sha1
}

/// A store with the MAME Source Set `mame` in the Root `sets`, which a repair may write, and the Ingest Folder `in`,
/// which settles at once, both found with chdman `chdman`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    /// The DAT is a ListXML of `machines`, each `(name, attributes, body)`.
    fn new(chdman: PathBuf, machines: &[(&str, &str, String)]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["roots/sets/mame", "in"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let config = config(tmp.path(), &chdman);
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store
            .update_settings(
                &config,
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
        let mut xml = String::from("<?xml version=\"1.0\"?>\n<mame build=\"0.289 (mame0289)\">\n");
        for (name, attrs, body) in machines {
            xml += &format!(
                "  <machine name=\"{name}\" {attrs}>\n    <description>{name}</description>\n{body}\n  </machine>\n"
            );
        }
        xml += "</mame>\n";
        let (revision, _) = store
            .import_dat_bytes(xml.as_bytes(), "mame.xml", None)
            .unwrap();
        store
            .add_source_set(&config, "sets", "mame", &revision.dat)
            .unwrap();
        store.register_ingest_folder(&config, "in").unwrap();
        Env { tmp, store, config }
    }

    /// Finds chdman at `chdman` from now on, as when the Operator corrects its path in the configuration.
    fn use_chdman(&mut self, chdman: &Path) {
        self.config = config(self.tmp.path(), chdman);
    }

    fn set(&self) -> PathBuf {
        self.tmp.path().join("roots/sets/mame")
    }

    fn ingest_dir(&self) -> PathBuf {
        self.tmp.path().join("in")
    }

    fn audit(&self, evidence: AuditEvidence) -> title_select_api::Report {
        self.store
            .audit_source_set(&self.config, "mame", None, evidence)
            .unwrap()
            .report
    }

    /// Scans the Ingest Folder twice, so that what is in it settles and is assessed.
    fn scan(&self) {
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

    fn item(&self, name: &str) -> title_select_api::ingest::IngestItem {
        let items = self.store.ingest_items("in").unwrap();
        items
            .iter()
            .find(|i| i.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("no item {name} in {items:#?}"))
    }

    fn plan(&self) -> RepairPlan {
        self.store
            .plan_repair(&self.config, "mame", &PlanRepair::default())
            .unwrap()
    }

    /// Approves `plan`, runs its Repair Job, then the Audit it owes.
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
}

/// The configuration of a store in `tmp`, as [`Env`] says, finding chdman at `chdman`.
fn config(tmp: &Path, chdman: &Path) -> Config {
    Config::parse(&format!(
        "[roots.sets]\npath = {:?}\nwritable_paths = [\"mame\"]\n\n\
         [roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n\n[chd]\nchdman = {:?}\n",
        tmp.join("roots/sets").display().to_string(),
        tmp.join("in").display().to_string(),
        chdman.display().to_string(),
    ))
    .unwrap()
}

/// Every file under `dir`, as paths relative to it, but a marker.
fn files(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            e.path()
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|p| p != ".title-select/marker")
        .collect();
    found.sort();
    found
}

fn disk(name: &str, sha1: &str) -> String {
    format!(r#"    <disk name="{name}" sha1="{sha1}" region="ide:0:hdd" index="0" writable="no"/>"#)
}

fn status(report: &title_select_api::Report, game: &str) -> GameStatus {
    report.games.iter().find(|g| g.name == game).unwrap().status
}

fn entry<'r>(report: &'r title_select_api::Report, path: &str) -> &'r title_select_api::Entry {
    report
        .entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no entry {path} in {:#?}", report.entries))
}

#[test]
fn a_disk_verified_by_a_full_audit_is_reused_by_the_next_incremental_audit() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster.chd"), 1, &[]);
    let env = Env::new(chdman, &[("blaster", "", disk("blaster", &sha1))]);
    std::fs::create_dir_all(env.set().join("blaster")).unwrap();
    std::fs::copy(
        scratch.path().join("blaster.chd"),
        env.set().join("blaster/blaster.chd"),
    )
    .unwrap();

    let full = env.audit(AuditEvidence::Full);
    let incremental = env.audit(AuditEvidence::Incremental);

    for report in [&full, &incremental] {
        let e = entry(report, "blaster/blaster.chd");
        assert_eq!(e.finding, Finding::Matched);
        assert_eq!(e.evidence.as_ref().unwrap().level, EvidenceLevel::Payload);
        assert_eq!(status(report, "blaster"), GameStatus::Complete);
    }
    let full = entry(&full, "blaster/blaster.chd")
        .evidence
        .clone()
        .unwrap();
    let reused = entry(&incremental, "blaster/blaster.chd")
        .evidence
        .clone()
        .unwrap();
    assert_eq!(full.reused, None);
    assert_eq!(reused.reused, Some(Reuse::UnchangedFileHeuristic));
    assert_eq!(reused.read_at, full.read_at);
}

#[test]
fn an_ingest_folder_of_a_familys_chds_is_assessed_verified_and_filed_into_the_parents_folder() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let incoming = scratch.path().join("blasterj");
    let parent_sha1 = make_chd(&chdman, &incoming.join("blaster.chd"), 1, &[]);
    let clone_sha1 = make_chd(&chdman, &incoming.join("blasterj.chd"), 2, &[]);
    // The folder is named for the clone, and holds its parent's Disk too: both are the family's.
    let env = Env::new(
        chdman,
        &[
            ("blaster", "", disk("blaster", &parent_sha1)),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                format!(
                    "{}\n{}",
                    disk("blaster", &parent_sha1).replace("<disk ", r#"<disk merge="blaster" "#),
                    disk("blasterj", &clone_sha1)
                ),
            ),
        ],
    );
    copy_dir(&incoming, &env.ingest_dir().join("blasterj"));
    let before = env.audit(AuditEvidence::Full);
    assert_eq!(status(&before, "blasterj"), GameStatus::Missing);

    env.scan();

    for (name, game) in [
        ("blasterj/blaster.chd", "blaster"),
        ("blasterj/blasterj.chd", "blasterj"),
    ] {
        let item = env.item(name);
        assert_eq!(item.state, ItemState::Assessed, "{item:#?}");
        assert!(
            item.assessment.matches.iter().any(|m| m.game == game),
            "{item:#?}"
        );
        assert!(matches!(
            item.assessment.proposal,
            Some(Proposal::SourceSet {
                need: Need::Missing,
                ..
            })
        ));
    }
    let plan = env.plan();
    for target in ["blaster/blaster.chd", "blaster/blasterj.chd"] {
        let item = plan
            .content
            .items
            .iter()
            .find(|i| i.key == format!("disk:{target}"))
            .unwrap_or_else(|| panic!("no disk item {target} in {:#?}", plan.content.items));
        assert_eq!(item.kind, RepairKind::Fill);
        assert_eq!(item.default, RepairChoice::Apply);
    }
    assert_eq!(plan.content.items.len(), 2, "{:#?}", plan.content.items);

    let after = env.approve(&plan);

    assert_eq!(
        files(&env.set()),
        ["blaster/blaster.chd", "blaster/blasterj.chd"]
    );
    assert!(
        files(&env.ingest_dir()).is_empty(),
        "{:?}",
        files(&env.ingest_dir())
    );
    for game in ["blaster", "blasterj"] {
        assert_eq!(status(&after, game), GameStatus::Complete, "{game}");
    }
    let filed = entry(&after, "blaster/blasterj.chd");
    assert_eq!(filed.finding, Finding::Matched);
    assert_eq!(
        filed.evidence.as_ref().unwrap().level,
        EvidenceLevel::Payload
    );
}

#[test]
fn a_disk_a_game_shares_with_its_bios_is_filed_into_the_bioss_folder_under_its_name() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("drv/drvdisk.chd"), 1, &[]);
    let env = Env::new(
        chdman,
        &[
            ("sysbios", r#"isbios="yes""#, disk("biosdisk", &sha1)),
            (
                "drv",
                r#"romof="sysbios""#,
                disk("drvdisk", &sha1).replace("<disk ", r#"<disk merge="biosdisk" "#),
            ),
        ],
    );
    copy_dir(&scratch.path().join("drv"), &env.ingest_dir().join("drv"));
    env.audit(AuditEvidence::Full);
    env.scan();

    let plan = env.plan();

    assert!(
        plan.content
            .items
            .iter()
            .any(|i| i.key == "disk:sysbios/biosdisk.chd"),
        "{:#?}",
        plan.content.items
    );
    let after = env.approve(&plan);
    assert_eq!(files(&env.set()), ["sysbios/biosdisk.chd"]);
    assert_eq!(status(&after, "drv"), GameStatus::Complete);
}

#[test]
fn a_chd_in_an_ingest_folder_that_doesnt_verify_or_is_a_delta_stays_put_with_why() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let parent = scratch.path().join("blaster.chd");
    let parent_sha1 = make_chd(&chdman, &parent, 1, &[]);
    let delta_sha1 = make_chd(
        &chdman,
        &scratch.path().join("blasterj.chd"),
        2,
        &["-op".as_ref(), parent.as_os_str()],
    );
    let raw_sha1 = make_chd(
        &chdman,
        &scratch.path().join("raw.chd"),
        3,
        &["-c".as_ref(), "none".as_ref()],
    );
    let env = Env::new(
        chdman,
        &[
            ("blaster", "", disk("blaster", &parent_sha1)),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                format!(
                    "{}\n{}",
                    disk("blasterj", &delta_sha1),
                    disk("raw", &raw_sha1)
                ),
            ),
        ],
    );
    let incoming = env.ingest_dir().join("blaster");
    std::fs::create_dir_all(&incoming).unwrap();
    std::fs::copy(
        scratch.path().join("blasterj.chd"),
        incoming.join("blasterj.chd"),
    )
    .unwrap();
    std::fs::copy(scratch.path().join("raw.chd"), incoming.join("raw.chd")).unwrap();
    write_file(&env.ingest_dir().join("blasterj/readme.txt"), b"notes");
    // A hidden file beside the CHDs is passed over: the folder is still a folder of disks.
    write_file(&incoming.join(".DS_Store"), b"finder");
    env.audit(AuditEvidence::Full);

    env.scan();

    let delta = env.item("blaster/blasterj.chd");
    assert_eq!(delta.state, ItemState::Unsupported);
    let why = delta.reason.unwrap();
    assert!(why.contains("delta CHD"), "{why}");
    assert!(why.contains("blaster.chd of blaster"), "{why}");
    let raw = env.item("blaster/raw.chd");
    assert_eq!(raw.state, ItemState::Unreadable, "{raw:#?}");
    let why = raw.reason.unwrap();
    assert!(why.contains("didn't confirm its SHA-1"), "{why}");
    assert!(why.contains("uncompressed"), "{why}");
    let mixed = env.item("blasterj");
    assert_eq!(mixed.state, ItemState::Unsupported);
    assert!(
        mixed
            .reason
            .unwrap()
            .contains("readme.txt in it is not a CHD")
    );
    let plan = env.plan();
    assert!(
        plan.content
            .items
            .iter()
            .all(|i| !i.key.starts_with("disk:")),
        "{:#?}",
        plan.content.items
    );
}

#[test]
fn a_repair_leaves_a_complete_games_disk_where_it_is_and_never_writes_it_into_its_zip() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster.chd"), 1, &[]);
    let rom = format!(
        r#"    <rom name="prog.u1" size="7" crc="{:08x}"/>"#,
        crc32fast::hash(b"program")
    );
    let env = Env::new(
        chdman,
        &[("blaster", "", format!("{rom}\n{}", disk("blaster", &sha1)))],
    );
    // Its ROM is loose, so a repair repacks it into its ZIP, without its Disk.
    write_file(&env.set().join("prog.u1"), b"program");
    std::fs::create_dir_all(env.set().join("blaster")).unwrap();
    std::fs::copy(
        scratch.path().join("blaster.chd"),
        env.set().join("blaster/blaster.chd"),
    )
    .unwrap();
    env.audit(AuditEvidence::Full);

    let plan = env.plan();

    assert!(
        plan.content
            .items
            .iter()
            .all(|i| i.path.as_deref() != Some("blaster/blaster.chd")),
        "{:#?}",
        plan.content.items
    );
    let after = env.approve(&plan);
    assert_eq!(
        files(&env.set()),
        [
            ".title-select/retained/1/prog.u1.kept",
            "blaster.zip",
            "blaster/blaster.chd"
        ]
    );
    assert_eq!(status(&after, "blaster"), GameStatus::Complete);
}

#[test]
fn a_family_repacked_into_any_set_layout_leaves_its_disk_in_its_parents_folder() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster.chd"), 1, &[]);
    let rom = |name: &str, payload: &[u8]| {
        format!(
            r#"    <rom name="{name}" size="{}" crc="{:08x}"/>"#,
            payload.len(),
            crc32fast::hash(payload)
        )
    };
    let shared = format!(
        r#"    <disk name="blaster" merge="blaster" sha1="{sha1}" region="ide:0:hdd" index="0" writable="no"/>"#
    );
    for layout in SetLayout::ALL {
        let env = Env::new(
            chdman.clone(),
            &[
                (
                    "blaster",
                    "",
                    format!("{}\n{}", rom("prog.u1", b"program"), disk("blaster", &sha1)),
                ),
                (
                    "blasterj",
                    r#"cloneof="blaster" romof="blaster""#,
                    format!("{}\n{shared}", rom("progj.u1", b"program j")),
                ),
            ],
        );
        // Its ROMs are loose, so a repair writes each Game's ZIP as the Set Layout gives it.
        write_file(&env.set().join("prog.u1"), b"program");
        write_file(&env.set().join("progj.u1"), b"program j");
        std::fs::create_dir_all(env.set().join("blaster")).unwrap();
        std::fs::copy(
            scratch.path().join("blaster.chd"),
            env.set().join("blaster/blaster.chd"),
        )
        .unwrap();
        let mut policy = StoragePolicy::default();
        policy.source_sets.systems.insert(
            "MAME/MAME".into(),
            SystemStorage {
                set_layout: Some(layout),
                ..SystemStorage::default()
            },
        );
        env.store.set_storage_policy(&policy, "Operator").unwrap();
        env.audit(AuditEvidence::Full);

        let plan = env.plan();

        assert!(
            (plan.content.items.iter()).all(|i| i.path.as_deref() != Some("blaster/blaster.chd")),
            "{layout:?}: {:#?}",
            plan.content.items
        );
        let after = env.approve(&plan);
        let zips = match layout {
            SetLayout::Merged => vec!["blaster.zip"],
            _ => vec!["blaster.zip", "blasterj.zip"],
        };
        let files = files(&env.set());
        assert!(
            files.contains(&"blaster/blaster.chd".to_owned()),
            "{layout:?}: {files:?}"
        );
        for zip in zips {
            assert!(files.contains(&zip.to_owned()), "{layout:?}: {files:?}");
        }
        assert!(
            !files.iter().any(|f| f.ends_with(".chd.kept")),
            "{layout:?}: {files:?}"
        );
        for game in ["blaster", "blasterj"] {
            assert_eq!(
                status(&after, game),
                GameStatus::Complete,
                "{layout:?}: {game}"
            );
        }
    }
}

#[test]
fn a_disk_filed_from_an_ingest_folder_replaces_an_older_chd_which_is_kept_aside() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster/blaster.chd"), 1, &[]);
    let env = Env::new(chdman, &[("blaster", "", disk("blaster", &sha1))]);
    copy_dir(
        &scratch.path().join("blaster"),
        &env.ingest_dir().join("blaster"),
    );
    // A version 4 CHD's header: its tag, length and version, and nothing a disk can match.
    let mut old = b"MComprHD".to_vec();
    old.extend_from_slice(&108u32.to_be_bytes());
    old.extend_from_slice(&4u32.to_be_bytes());
    old.resize(108, 0);
    write_file(&env.set().join("blaster/blaster.chd"), &old);
    let before = env.audit(AuditEvidence::Full);
    assert_eq!(
        entry(&before, "blaster/blaster.chd").finding,
        Finding::Unmatched
    );
    env.scan();

    let plan = env.plan();

    let item = plan
        .content
        .items
        .iter()
        .find(|i| i.key == "disk:blaster/blaster.chd")
        .unwrap_or_else(|| panic!("{:#?}", plan.content.items));
    assert_eq!(item.kind, RepairKind::Replace);
    let after = env.approve(&plan);
    assert_eq!(status(&after, "blaster"), GameStatus::Complete);
    let kept: Vec<String> = files(&env.set())
        .into_iter()
        .filter(|f| f.starts_with(".title-select/retained/"))
        .collect();
    assert_eq!(kept.len(), 1, "{:?}", files(&env.set()));
    assert!(kept[0].ends_with("blaster.chd.kept"), "{kept:?}");
}

#[test]
fn a_games_zip_of_its_roms_and_a_folder_of_its_disk_fill_it_together() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("chds/blaster.chd"), 1, &[]);
    let rom = format!(
        r#"    <rom name="prog.u1" size="7" crc="{:08x}"/>"#,
        crc32fast::hash(b"program")
    );
    let env = Env::new(
        chdman,
        &[("blaster", "", format!("{rom}\n{}", disk("blaster", &sha1)))],
    );
    // The ZIP holds every ROM the Game needs but its Disk, which is a CHD of its own.
    write_zip(
        &env.ingest_dir().join("blaster.zip"),
        &[("prog.u1", b"program")],
    );
    copy_dir(
        &scratch.path().join("chds"),
        &env.ingest_dir().join("blaster"),
    );
    env.audit(AuditEvidence::Full);
    env.scan();

    let zip = env.item("blaster.zip");
    assert_eq!(zip.state, ItemState::Assessed, "{zip:#?}");
    let matched = zip
        .assessment
        .matches
        .iter()
        .find(|m| m.game == "blaster")
        .unwrap_or_else(|| panic!("{zip:#?}"));
    assert!(matched.complete, "{zip:#?}");
    assert_eq!(matched.need, Need::Missing);
    let plan = env.plan();
    for key in ["game:blaster", "disk:blaster/blaster.chd"] {
        let item = plan
            .content
            .items
            .iter()
            .find(|i| i.key == key)
            .unwrap_or_else(|| panic!("no item {key} in {:#?}", plan.content.items));
        assert_eq!(item.kind, RepairKind::Fill, "{item:#?}");
    }

    let after = env.approve(&plan);

    assert_eq!(files(&env.set()), ["blaster.zip", "blaster/blaster.chd"]);
    assert_eq!(status(&after, "blaster"), GameStatus::Complete);
}

#[test]
fn a_chd_chdman_couldnt_verify_is_verified_at_the_next_scan_once_chdman_is_found() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster/blaster.chd"), 1, &[]);
    let missing = scratch.path().join("no-chdman");
    let mut env = Env::new(missing, &[("blaster", "", disk("blaster", &sha1))]);
    copy_dir(
        &scratch.path().join("blaster"),
        &env.ingest_dir().join("blaster"),
    );
    env.audit(AuditEvidence::Full);
    env.scan();
    let unread = env.item("blaster/blaster.chd");
    assert_eq!(unread.state, ItemState::Unreadable, "{unread:#?}");
    assert!(unread.reason.unwrap().contains("chdman"));

    // The CHD is as it was: only the configuration changed.
    env.use_chdman(&chdman);
    env.scan();

    let item = env.item("blaster/blaster.chd");
    assert_eq!(item.state, ItemState::Assessed, "{item:#?}");
    assert!(matches!(
        item.assessment.proposal,
        Some(Proposal::SourceSet {
            need: Need::Missing,
            ..
        })
    ));
}

#[test]
fn a_disk_chdman_couldnt_verify_is_verified_by_the_next_incremental_audit_once_chdman_is_found() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let sha1 = make_chd(&chdman, &scratch.path().join("blaster/blaster.chd"), 1, &[]);
    let mut env = Env::new(
        scratch.path().join("no-chdman"),
        &[("blaster", "", disk("blaster", &sha1))],
    );
    copy_dir(&scratch.path().join("blaster"), &env.set().join("blaster"));
    let unread = env.audit(AuditEvidence::Full);
    assert_eq!(
        entry(&unread, "blaster/blaster.chd").finding,
        Finding::Unreadable
    );

    env.use_chdman(&chdman);
    let after = env.audit(AuditEvidence::Incremental);

    let e = entry(&after, "blaster/blaster.chd");
    assert_eq!(e.finding, Finding::Matched);
    let evidence = e.evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Payload);
    assert_eq!(evidence.reused, None);
    assert_eq!(status(&after, "blaster"), GameStatus::Complete);
}

/// Copies the files of the folder `from` into `to`.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn a_stored_audit_keeps_which_chd_counts_for_each_disk_and_a_delta_chds_parent() {
    let Some(chdman) = chdman() else { return };
    let scratch = tempfile::tempdir().unwrap();
    let parent = scratch.path().join("blaster.chd");
    let parent_sha1 = make_chd(&chdman, &parent, 1, &[]);
    let delta_sha1 = make_chd(
        &chdman,
        &scratch.path().join("blasterj.chd"),
        2,
        &["-op".as_ref(), parent.as_os_str()],
    );
    let env = Env::new(
        chdman,
        &[
            ("blaster", "", disk("blaster", &parent_sha1)),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                format!(
                    "{}\n{}",
                    disk("blasterj", &delta_sha1),
                    disk("blasterj-alt", &parent_sha1)
                ),
            ),
        ],
    );
    std::fs::create_dir_all(env.set().join("blaster")).unwrap();
    for chd in ["blaster.chd", "blasterj.chd"] {
        std::fs::copy(
            scratch.path().join(chd),
            env.set().join("blaster").join(chd),
        )
        .unwrap();
    }

    env.audit(AuditEvidence::Quick);
    let latest = &env.store.history("mame").unwrap()[0];
    let stored = env.store.report(latest.id).unwrap();

    let held = entry(&stored, "blaster/blaster.chd");
    let counted: Vec<(&str, &str)> = held
        .disks
        .iter()
        .map(|r| (r.game.as_str(), r.name.as_str()))
        .collect();
    assert_eq!(
        counted,
        [("blaster", "blaster.chd"), ("blasterj", "blasterj-alt.chd")]
    );
    let delta = entry(&stored, "blaster/blasterj.chd");
    assert_eq!(delta.parent_sha1.as_deref(), Some(parent_sha1.as_str()));
    assert!(delta.disks.is_empty());
    let clone = stored.games.iter().find(|g| g.name == "blasterj").unwrap();
    assert_eq!(clone.disks, ["blasterj.chd", "blasterj-alt.chd"]);
}
