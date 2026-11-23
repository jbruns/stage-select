//! Disks (ADR 0026): a ListXML machine's `<disk>`, kept as a CHD in its family's folder, `<parent>/<disk>.chd`, and
//! matched by the combined SHA-1 its header records. A quick inventory claims it from the header, a full Audit proves
//! it with `chdman verify`, and an incremental one reuses that while the file is unchanged. A delta CHD is Unreadable,
//! and an older CHD Unmatched.
//!
//! The tests that run chdman find it as the store does: `STAGE_SELECT_CHDMAN`, else `chdman` on the `PATH`. Where it is
//! missing they are skipped, except in CI, where they fail.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::common::*;
use stage_select_engine::chd::{Chdman, Unverified};
use stage_select_engine::{
    AuditEvidence, ChdAudit, Entry, EvidenceLevel, Finding, Game, GameStatus, ONE_WORKER_EACH,
    Options, PayloadRead, PayloadReads, Report, Reuse, audit_with,
};

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

/// How an Audit runs `program` as chdman, extracting into `tmp`.
fn chd_audit(program: &Path, tmp: &Path) -> ChdAudit {
    ChdAudit {
        chdman: Ok(Chdman {
            program: program.to_owned(),
            threads: None,
            deadline: Default::default(),
            tally: Default::default(),
        }),
        limits: Default::default(),
        tmp: tmp.to_owned(),
    }
}

/// A fake chdman of this crate's test data.
fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/chdman")
        .join(name)
}

/// Records the reads an Audit makes, and offers `prior` as earlier ones.
#[derive(Default)]
struct Reads {
    prior: Vec<(String, PayloadRead)>,
    recorded: Mutex<Vec<(String, PayloadRead)>>,
}

impl PayloadReads for Reads {
    fn prior(&self, path: &str) -> Option<PayloadRead> {
        self.prior
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, r)| r.clone())
    }

    fn record(&self, path: &str, read: &PayloadRead) {
        self.recorded
            .lock()
            .unwrap()
            .push((path.to_owned(), read.clone()));
    }
}

fn run(
    dat: &Path,
    set: &Path,
    evidence: AuditEvidence,
    disks: Option<&ChdAudit>,
    reads: Option<&dyn PayloadReads>,
) -> Report {
    audit_with(
        dat,
        set,
        &Options {
            file: "mame.xml".into(),
            confine: None,
            workers: ONE_WORKER_EACH,
            pools: None,
            progress: None,
            evidence,
            reads,
            tally: Default::default(),
            placed: &[],
            disks,
        },
    )
    .unwrap()
}

/// Writes a ListXML of `machines`, each `(name, attributes, body)`, to `dir/mame.xml`.
fn listxml(dir: &Path, machines: &[(&str, &str, &str)]) -> PathBuf {
    let path = dir.join("mame.xml");
    std::fs::write(&path, stage_select_test_fixtures::listxml(machines)).unwrap();
    path
}

/// A SHA-1 as hex, `n` repeated.
fn sha(n: u8) -> String {
    format!("{n:02x}").repeat(20)
}

/// The bytes of a CHD of `version` whose header records `sha1` as its combined SHA-1 and `parent` as its parent's,
/// followed by data that is no CHD's: enough for its header to be read, never for chdman to verify.
fn synthetic_chd(version: u32, sha1: &str, parent: Option<&str>) -> Vec<u8> {
    let unhex = |h: &str| -> Vec<u8> {
        (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect()
    };
    let length: u32 = if version == 5 { 124 } else { 108 };
    let mut raw = vec![0u8; length as usize];
    raw[0..8].copy_from_slice(b"MComprHD");
    raw[8..12].copy_from_slice(&length.to_be_bytes());
    raw[12..16].copy_from_slice(&version.to_be_bytes());
    raw[32..40].copy_from_slice(&4096u64.to_be_bytes());
    if version == 5 {
        raw[84..104].copy_from_slice(&unhex(sha1));
        if let Some(parent) = parent {
            raw[104..124].copy_from_slice(&unhex(parent));
        }
    }
    raw.extend_from_slice(&noise(9, 512));
    raw
}

/// Makes a real, compressed CHD of `len` bytes of noise from `seed` at `path` with chdman, made against `parent` as a
/// delta CHD when given; returns its combined SHA-1.
fn real_chd(chdman: &Path, path: &Path, seed: u64, parent: Option<&Path>) -> String {
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let raw = dir.join(format!("seed-{seed}.raw"));
    std::fs::write(&raw, noise(seed, 64 * 1024)).unwrap();
    let mut cmd = std::process::Command::new(chdman);
    cmd.args(["createraw", "-hs", "4096", "-us", "512", "-i"])
        .arg(&raw)
        .arg("-o")
        .arg(path);
    if let Some(parent) = parent {
        cmd.arg("-op").arg(parent);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    std::fs::remove_file(&raw).unwrap();
    stage_select_engine::chd::header(&std::fs::File::open(path).unwrap())
        .unwrap()
        .sha1
}

fn entry<'r>(report: &'r Report, path: &str) -> &'r Entry {
    report
        .entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no entry {path} in {:#?}", report.entries))
}

fn game<'r>(report: &'r Report, name: &str) -> &'r Game {
    report.games.iter().find(|g| g.name == name).unwrap()
}

/// `blaster`, with a ROM and a Disk listed by `sha1`.
fn blaster(f: &Fixture, sha1: &str) -> PathBuf {
    write_zip(&f.set_path("blaster.zip"), &[("prog.u1", b"program")]);
    let rom = format!(
        r#"    <rom name="prog.u1" size="7" crc="{:08x}"/>
    <disk name="blaster" sha1="{sha1}" region="ide:0:hdd" index="0" writable="no"/>"#,
        crc32fast::hash(b"program")
    );
    listxml(f.dat_dir.path(), &[("blaster", "", &rom)])
}

#[test]
fn a_quick_inventory_claims_a_disk_by_the_sha1_its_chds_header_records() {
    let f = Fixture::new();
    let dat = blaster(&f, &sha(1));
    write_file(
        &f.set_path("blaster/blaster.chd"),
        &synthetic_chd(5, &sha(1), None),
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None, None);

    let disk = entry(&report, "blaster/blaster.chd");
    assert_eq!(disk.finding, Finding::Matched);
    let evidence = disk.evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Claim);
    assert_eq!(evidence.algorithms, ["sha1"]);
    let g = game(&report, "blaster");
    assert_eq!(g.status, GameStatus::Complete);
    assert_eq!(g.claimed_roms, ["prog.u1", "blaster.chd"]);
}

#[test]
fn a_nodump_disk_is_never_matched_and_a_game_is_complete_without_it() {
    let f = Fixture::new();
    write_zip(&f.set_path("blaster.zip"), &[("prog.u1", b"program")]);
    let roms = format!(
        r#"    <rom name="prog.u1" size="7" crc="{:08x}"/>
    <disk name="blaster" sha1="{}" status="nodump" region="ide:0:hdd" index="0" writable="no"/>"#,
        crc32fast::hash(b"program"),
        sha(1)
    );
    let dat = listxml(f.dat_dir.path(), &[("blaster", "", &roms)]);
    // A CHD under the nodump Disk's name, whose header records the SHA-1 the DAT lists beside it.
    write_file(
        &f.set_path("blaster/blaster.chd"),
        &synthetic_chd(5, &sha(1), None),
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None, None);

    assert_ne!(
        entry(&report, "blaster/blaster.chd").finding,
        Finding::Matched
    );
    let g = game(&report, "blaster");
    assert_eq!(g.status, GameStatus::Complete);
    assert_eq!(g.missing_roms, ["blaster.chd"]);
    assert_eq!(g.not_dumped, ["blaster.chd"]);
    assert_eq!(g.disks, ["blaster.chd"]);
    assert!(entry(&report, "blaster/blaster.chd").disks.is_empty());
}

#[test]
fn a_clones_disks_in_its_parents_folder_count_for_it_by_name_and_by_sha1_under_another_name() {
    let f = Fixture::new();
    let dat = listxml(
        f.dat_dir.path(),
        &[
            (
                "blaster",
                "",
                &format!(r#"    <disk name="blaster" sha1="{}"/>"#, sha(1)),
            ),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                &format!(
                    r#"    <disk name="blaster" merge="blaster" sha1="{}"/>
    <disk name="blasterj" sha1="{}"/>
    <disk name="blasterj-alt" sha1="{}"/>"#,
                    sha(1),
                    sha(2),
                    sha(1)
                ),
            ),
        ],
    );
    write_file(
        &f.set_path("blaster/blaster.chd"),
        &synthetic_chd(5, &sha(1), None),
    );
    write_file(
        &f.set_path("blaster/blasterj.chd"),
        &synthetic_chd(5, &sha(2), None),
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None, None);

    assert_eq!(game(&report, "blaster").status, GameStatus::Complete);
    let clone = game(&report, "blasterj");
    assert_eq!(clone.status, GameStatus::Complete, "{clone:#?}");
    assert!(clone.available_elsewhere.is_empty());
    assert_eq!(
        entry(&report, "blaster/blasterj.chd").finding,
        Finding::Matched
    );
    // Each CHD names the Disks it counts for, the parent's under another name included.
    assert_eq!(
        clone.disks,
        ["blaster.chd", "blasterj.chd", "blasterj-alt.chd"]
    );
    let disks = |path: &str| -> Vec<(String, String)> {
        entry(&report, path)
            .disks
            .iter()
            .map(|r| (r.game.clone(), r.name.clone()))
            .collect()
    };
    let disk = |game: &str, name: &str| (game.to_owned(), name.to_owned());
    assert_eq!(
        disks("blaster/blaster.chd"),
        [
            disk("blaster", "blaster.chd"),
            disk("blasterj", "blaster.chd"),
            disk("blasterj", "blasterj-alt.chd"),
        ]
    );
    assert_eq!(
        disks("blaster/blasterj.chd"),
        [disk("blasterj", "blasterj.chd")]
    );
}

#[test]
fn a_disk_of_another_family_is_available_elsewhere_and_never_present() {
    let f = Fixture::new();
    let dat = listxml(
        f.dat_dir.path(),
        &[
            (
                "blaster",
                "",
                &format!(r#"    <disk name="blaster" sha1="{}"/>"#, sha(1)),
            ),
            (
                "rival",
                "",
                &format!(r#"    <disk name="rival" sha1="{}"/>"#, sha(1)),
            ),
        ],
    );
    write_file(
        &f.set_path("blaster/blaster.chd"),
        &synthetic_chd(5, &sha(1), None),
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Quick, None, None);

    let rival = game(&report, "rival");
    assert_eq!(rival.status, GameStatus::Missing);
    assert_eq!(rival.available_elsewhere[0].games, ["blaster"]);
}

#[test]
fn a_delta_chd_is_unreadable_and_names_the_parent_chd_it_needs() {
    let f = Fixture::new();
    let dat = listxml(
        f.dat_dir.path(),
        &[
            (
                "blaster",
                "",
                &format!(r#"    <disk name="blaster" sha1="{}"/>"#, sha(1)),
            ),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                &format!(r#"    <disk name="blasterj" sha1="{}"/>"#, sha(2)),
            ),
        ],
    );
    write_file(
        &f.set_path("blaster/blasterj.chd"),
        &synthetic_chd(5, &sha(2), Some(&sha(1))),
    );

    for evidence in [AuditEvidence::Quick, AuditEvidence::Full] {
        let report = run(&dat, f.set.path(), evidence, None, None);

        let delta = entry(&report, "blaster/blasterj.chd");
        assert_eq!(delta.finding, Finding::Unreadable, "{evidence:?}");
        let why = delta.reason.as_deref().unwrap();
        assert!(why.contains("delta CHD"), "{why}");
        assert!(why.contains("blaster.chd of blaster"), "{why}");
        assert!(why.contains(&sha(1)), "{why}");
        assert_eq!(delta.parent_sha1.as_deref(), Some(sha(1).as_str()));
        assert_eq!(game(&report, "blasterj").status, GameStatus::Missing);
    }
}

#[test]
fn a_chd_older_than_version_5_is_unmatched_and_says_to_update_it() {
    let f = Fixture::new();
    let dat = blaster(&f, &sha(1));
    write_file(
        &f.set_path("blaster/blaster.chd"),
        &synthetic_chd(4, &sha(1), None),
    );

    for evidence in [AuditEvidence::Quick, AuditEvidence::Full] {
        let report = run(&dat, f.set.path(), evidence, None, None);

        let old = entry(&report, "blaster/blaster.chd");
        assert_eq!(old.finding, Finding::Unmatched, "{evidence:?}");
        let why = old.reason.as_deref().unwrap();
        assert!(why.contains("version 4 CHD"), "{why}");
        assert!(why.contains("update it outside Stage Select"), "{why}");
        assert_eq!(game(&report, "blaster").status, GameStatus::Incomplete);
    }
}

#[test]
fn a_chd_beside_a_dat_that_lists_no_disks_is_read_as_any_file() {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(
        &f.set_path("Pong (USA).chd"),
        &synthetic_chd(5, &sha(1), None),
    );

    let report = run(&dat, f.set.path(), AuditEvidence::Full, None, None);

    let chd = entry(&report, "Pong (USA).chd");
    assert_eq!(chd.finding, Finding::Unmatched);
    assert_eq!(chd.reason, None);
    assert_eq!(chd.evidence.as_ref().unwrap().level, EvidenceLevel::Payload);
}

#[test]
fn a_chdman_verify_that_exits_0_without_its_success_line_is_no_payload_evidence() {
    let f = Fixture::new();
    let dat = blaster(&f, &sha(1));
    write_file(
        &f.set_path("blaster/blaster.chd"),
        &synthetic_chd(5, &sha(1), None),
    );
    let tmp = tempfile::tempdir().unwrap();
    let disks = chd_audit(&fake("unconfirmed"), tmp.path());

    let report = run(&dat, f.set.path(), AuditEvidence::Full, Some(&disks), None);

    let disk = entry(&report, "blaster/blaster.chd");
    assert_eq!(disk.finding, Finding::Unreadable);
    assert_eq!(disk.evidence, None);
    let why = disk.reason.as_deref().unwrap();
    assert!(why.contains("didn't confirm its SHA-1"), "{why}");
    assert_eq!(game(&report, "blaster").status, GameStatus::Incomplete);
}

#[test]
fn only_chdmans_verdict_on_a_chd_refutes_it_never_a_chdman_that_is_missing_crashes_or_says_nothing()
{
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("blaster.chd");
    write_file(&path, &synthetic_chd(5, &sha(1), None));
    let header = stage_select_engine::chd::header(&std::fs::File::open(&path).unwrap()).unwrap();

    let verify = |program: &Path| chd_audit(program, tmp.path()).verify(&path, &header);

    assert!(matches!(
        verify(&fake("unconfirmed")),
        Err(Unverified::Refuted(why)) if why.contains("didn't confirm its SHA-1")
    ));
    for program in [
        fake("crashes"),
        fake("silent"),
        tmp.path().join("no-chdman"),
    ] {
        assert!(
            matches!(verify(&program), Err(Unverified::Unchecked(_))),
            "{program:?}"
        );
    }
}

#[test]
fn a_full_audit_proves_a_disk_with_chdman_verify_and_an_incremental_one_reuses_that() {
    let Some(chdman) = chdman() else { return };
    let f = Fixture::new();
    let tmp = tempfile::tempdir().unwrap();
    let sha1 = real_chd(&chdman, &f.set_path("blaster/blaster.chd"), 1, None);
    let dat = blaster(&f, &sha1);
    let disks = chd_audit(&chdman, tmp.path());
    let reads = Reads::default();

    let full = run(
        &dat,
        f.set.path(),
        AuditEvidence::Full,
        Some(&disks),
        Some(&reads),
    );

    let disk = entry(&full, "blaster/blaster.chd");
    assert_eq!(disk.finding, Finding::Matched, "{disk:#?}");
    let evidence = disk.evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Payload);
    assert_eq!(evidence.algorithms, ["sha1"]);
    assert_eq!(evidence.reused, None);
    assert_eq!(game(&full, "blaster").status, GameStatus::Complete);
    assert!(game(&full, "blaster").claimed_roms.is_empty());
    let recorded = reads.recorded.into_inner().unwrap();
    let (_, read) = recorded
        .iter()
        .find(|(p, _)| p == "blaster/blaster.chd")
        .unwrap();
    assert_eq!(read.disk.as_deref(), Some(sha1.as_str()));
    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "the scratch folder is removed"
    );

    // An incremental Audit without chdman reuses the verification while the file is unchanged.
    let again = Reads {
        prior: recorded.clone(),
        ..Reads::default()
    };
    let incremental = run(
        &dat,
        f.set.path(),
        AuditEvidence::Incremental,
        None,
        Some(&again),
    );

    let disk = entry(&incremental, "blaster/blaster.chd");
    assert_eq!(disk.finding, Finding::Matched);
    let evidence = disk.evidence.as_ref().unwrap();
    assert_eq!(evidence.level, EvidenceLevel::Payload);
    assert_eq!(evidence.reused, Some(Reuse::UnchangedFileHeuristic));
    assert_eq!(evidence.read_at, Some(read.read_at));
}

#[test]
fn a_real_chd_whose_header_sha1_its_data_doesnt_hash_to_is_unreadable_in_a_full_audit() {
    let Some(chdman) = chdman() else { return };
    let f = Fixture::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = f.set_path("blaster/blaster.chd");
    real_chd(&chdman, &path, 1, None);
    // Its header now records another combined SHA-1, the DAT's: chdman verify exits 0 and says it doesn't match.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[84..104].copy_from_slice(&[0x11; 20]);
    std::fs::write(&path, bytes).unwrap();
    let dat = blaster(&f, &sha(0x11));

    let quick = run(&dat, f.set.path(), AuditEvidence::Quick, None, None);
    assert_eq!(
        entry(&quick, "blaster/blaster.chd").finding,
        Finding::Matched
    );

    let disks = chd_audit(&chdman, tmp.path());
    let full = run(&dat, f.set.path(), AuditEvidence::Full, Some(&disks), None);

    let disk = entry(&full, "blaster/blaster.chd");
    assert_eq!(disk.finding, Finding::Unreadable, "{disk:#?}");
    assert!(
        disk.reason
            .as_deref()
            .unwrap()
            .contains("didn't confirm its SHA-1")
    );
}

#[test]
fn a_real_uncompressed_chd_is_unreadable_in_a_full_audit_as_chdman_has_nothing_to_verify() {
    let Some(chdman) = chdman() else { return };
    let f = Fixture::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = f.set_path("blaster/blaster.chd");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let raw = tmp.path().join("disk.raw");
    std::fs::write(&raw, noise(1, 64 * 1024)).unwrap();
    let out = std::process::Command::new(&chdman)
        .args(["createraw", "-hs", "4096", "-us", "512", "-c", "none", "-i"])
        .arg(&raw)
        .arg("-o")
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let sha1 = stage_select_engine::chd::header(&std::fs::File::open(&path).unwrap())
        .unwrap()
        .sha1;
    let dat = blaster(&f, &sha1);

    let disks = chd_audit(&chdman, tmp.path());
    let full = run(&dat, f.set.path(), AuditEvidence::Full, Some(&disks), None);

    let disk = entry(&full, "blaster/blaster.chd");
    assert_eq!(disk.finding, Finding::Unreadable, "{disk:#?}");
    let why = disk.reason.as_deref().unwrap();
    assert!(why.contains("didn't confirm its SHA-1"), "{why}");
    assert_eq!(game(&full, "blaster").status, GameStatus::Incomplete);
}

#[test]
fn a_real_delta_chd_is_unreadable_however_it_is_audited() {
    let Some(chdman) = chdman() else { return };
    let f = Fixture::new();
    let tmp = tempfile::tempdir().unwrap();
    let parent = f.set_path("blaster/blaster.chd");
    let parent_sha1 = real_chd(&chdman, &parent, 1, None);
    let delta_sha1 = real_chd(
        &chdman,
        &f.set_path("blaster/blasterj.chd"),
        2,
        Some(&parent),
    );
    let dat = listxml(
        f.dat_dir.path(),
        &[
            (
                "blaster",
                "",
                &format!(r#"    <disk name="blaster" sha1="{parent_sha1}"/>"#),
            ),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                &format!(r#"    <disk name="blasterj" sha1="{delta_sha1}"/>"#),
            ),
        ],
    );
    let disks = chd_audit(&chdman, tmp.path());

    let full = run(&dat, f.set.path(), AuditEvidence::Full, Some(&disks), None);

    assert_eq!(
        entry(&full, "blaster/blaster.chd").finding,
        Finding::Matched
    );
    let delta = entry(&full, "blaster/blasterj.chd");
    assert_eq!(delta.finding, Finding::Unreadable);
    assert!(
        delta
            .reason
            .as_deref()
            .unwrap()
            .contains(&format!("blaster.chd of blaster (SHA-1 {parent_sha1})"))
    );
}
