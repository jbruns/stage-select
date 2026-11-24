use std::process::Command;

use crate::common::*;

fn stage_select() -> Command {
    Command::new(env!("CARGO_BIN_EXE_stage-select"))
}

fn pong_fixture() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    (f, dat)
}

#[test]
fn audit_prints_a_json_report_to_stdout() {
    let (f, dat) = pong_fixture();

    let out = stage_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], "stage-select.audit/1");
    assert_eq!(report["dat_revision"]["name"], "Test - System");
    assert_eq!(report["summary"]["findings"]["matched"], 1);
    assert_eq!(report["entries"][0]["finding"], "matched");
    assert_eq!(report["entries"][0]["evidence"]["level"], "payload");
    assert_eq!(report["games"][0]["status"], "complete");
}

#[test]
fn a_stateless_audit_says_it_ran_in_full() {
    let (f, dat) = pong_fixture();

    let out = stage_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .output()
        .unwrap();

    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["evidence"], "full");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no earlier payload reads to reuse: it ran as a full audit"),
        "{stderr}"
    );
}

#[test]
fn audit_writes_the_report_to_the_output_file() {
    let (f, dat) = pong_fixture();
    let output = f.dat_dir.path().join("report.json");

    let out = stage_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(report["summary"]["findings"]["matched"], 1);
}

#[test]
fn audit_fails_with_a_message_for_an_unreadable_dat_or_a_missing_source_set() {
    let (f, dat) = pong_fixture();
    let not_xml = f.dat_dir.path().join("not.dat");
    std::fs::write(&not_xml, "<datafile><game name=\"x\"></datafile>").unwrap();
    let missing_set = f.dat_dir.path().join("no-such-set");

    for (dat, set) in [
        (not_xml.as_path(), f.set.path()),
        (dat.as_path(), missing_set.as_path()),
    ] {
        let out = stage_select()
            .arg("audit")
            .arg("--dat")
            .arg(dat)
            .arg(set)
            .output()
            .unwrap();

        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}

fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_headerless_nes_match_is_warned_of_in_the_report_and_counted_in_the_history() {
    let f = Fixture::new();
    let dat = write_dat_with_header(
        f.dat_dir.path(),
        "No-Intro_NES.xml",
        &[("Zelda (USA)", vec![rom("Zelda (USA).nes", b"zelda")])],
    );
    write_file(&f.set_path("Zelda (USA).nes"), b"zelda");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));

    let report: serde_json::Value =
        serde_json::from_str(&run_ok(ts().args(["audit", "--set", rel]))).unwrap();

    assert_eq!(report["games"][0]["warnings"][0], "header_missing");
    assert_eq!(report["summary"]["warnings"]["header_missing"], 1);
    let history = run_ok(ts().args(["set", "history", rel]));
    assert!(history.contains(" header_missing=1\t"), "{history}");
}

#[test]
fn a_registered_source_set_is_audited_into_the_store_and_listed_in_its_history() {
    let (f, dat) = pong_fixture();
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));

    let imported = run_ok(ts().args(["dat", "import"]).arg(&dat));
    assert!(imported.contains("No-Intro/Test - System"), "{imported}");
    let pack = f.dat_dir.path().join("cues.zip");
    write_zip(
        &pack,
        &[("Pong (USA).cue", b"FILE \"Pong (USA).bin\" BINARY\n")],
    );
    let cues = run_ok(
        ts().args(["cue-pack", "import"])
            .arg(&pack)
            .args(["--dat", "No-Intro/Test - System"]),
    );
    assert!(cues.contains("(1 cue sheet) imported"), "{cues}");
    let listed = run_ok(ts().args(["cue-pack", "list"]));
    assert!(
        listed.starts_with("No-Intro/Test - System\tcues.zip\t1 cue sheet"),
        "{listed}"
    );
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    let report: serde_json::Value =
        serde_json::from_str(&run_ok(ts().args(["audit", "--set", rel]))).unwrap();
    assert_eq!(report["schema"], "stage-select.audit/1");
    assert_eq!(report["summary"]["findings"]["matched"], 1);

    let sets = run_ok(ts().args(["set", "list"]));
    let sha = report["dat_revision"]["sha256"].as_str().unwrap();
    assert!(sets.starts_with(rel), "{sets}");
    assert!(
        sets.contains(&format!("last verified against {sha}")),
        "{sets}"
    );
    let history = run_ok(ts().args(["set", "history", rel]));
    assert_eq!(history.lines().count(), 1);
    assert!(
        history.contains("matched=1") && history.contains("complete=1"),
        "{history}"
    );

    let again = ts().args(["audit", "--set", rel]).output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert_eq!(report["evidence"], "incremental");
    assert!(
        stderr.contains("0 entries read, 1 reused") && stderr.contains("heuristic"),
        "{stderr}"
    );
    let full: serde_json::Value = serde_json::from_str(&run_ok(ts().args([
        "audit",
        "--set",
        rel,
        "--evidence",
        "full",
    ])))
    .unwrap();
    assert_eq!(full["evidence"], "full");
    assert!(full["entries"][0]["evidence"].get("reused").is_none());
    let history = run_ok(ts().args(["set", "history", rel]));
    assert!(
        history.lines().next().unwrap().contains("evidence=full"),
        "{history}"
    );
}

#[test]
fn fixdat_and_have_miss_export_what_the_latest_audit_found_missing() {
    let f = Fixture::new();
    let dat = f.dat(&[
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]),
    ]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    let unaudited = ts().args(["fixdat", rel]).output().unwrap();
    assert!(!unaudited.status.success());
    assert!(
        String::from_utf8_lossy(&unaudited.stderr).contains("audit it"),
        "{}",
        String::from_utf8_lossy(&unaudited.stderr)
    );
    run_ok(ts().args(["audit", "--set", rel]));

    let xml = run_ok(ts().args(["fixdat", rel]));
    let fixdat =
        stage_select_engine::dat::Dat::parse(xml.as_bytes(), std::path::Path::new("x")).unwrap();
    assert_eq!(fixdat.games, ["Breakout (USA)"]);

    let out = f.dat_dir.path().join("exports");
    std::fs::create_dir(&out).unwrap();
    let said = ts()
        .args(["have-miss", rel, "--output"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(said.status.success());
    let written: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [csv] = written.as_slice() else {
        panic!("one file is written into the folder: {written:?}");
    };
    let name = csv.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("have-miss_Test - System (") && name.ends_with(").csv"),
        "{name}"
    );
    assert!(
        String::from_utf8_lossy(&said.stderr).contains(name),
        "it says where it wrote"
    );
    assert_eq!(
        std::fs::read_to_string(csv).unwrap().lines().nth(2),
        Some("Breakout (USA),missing,,Breakout (USA).a26,")
    );

    let unknown = ts()
        .args(["fixdat", rel, "--selection", "999"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());

    run_ok(ts().args(["set", "coverage", rel, "--region", "Japan"]));
    let covered = run_ok(ts().args(["fixdat", rel]));
    assert!(!covered.contains("Breakout"), "{covered}");
    assert!(
        covered.contains("limited to the games in its coverage: Japan"),
        "{covered}"
    );
    let whole = run_ok(ts().args(["fixdat", rel, "--scope", "dat"]));
    assert!(whole.contains("Breakout (USA)"), "{whole}");
}

#[test]
fn a_source_set_is_unregistered_listed_as_unregistered_and_forgotten() {
    let (f, dat) = pong_fixture();
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    run_ok(ts().args(["audit", "--set", rel]));
    let holds = run_ok(ts().args(["root", "holds", "sets"]));
    assert!(holds.contains(rel), "{holds}");
    let holds = run_ok(ts().args(["set", "holds", rel]));
    assert!(holds.contains("nothing holds it"), "{holds}");

    let done = run_ok(ts().args(["set", "unregister", rel]));
    assert!(
        done.contains("unregistered; no game file was touched"),
        "{done}"
    );
    assert!(f.set.path().join("Pong (USA).a26").exists());
    assert!(run_ok(ts().args(["set", "list"])).is_empty());
    let listed = run_ok(ts().args(["unregistered", "list"]));
    assert!(
        listed.starts_with("source_set\t1\t") && listed.contains("audits=1"),
        "{listed}"
    );

    let history = run_ok(ts().args(["unregistered", "history", "source-set", "1"]));
    assert!(
        history.starts_with(&format!("source set {rel} "))
            && history.lines().any(|l| l.starts_with("audit\t")),
        "{history}"
    );

    let forgotten = run_ok(ts().args(["unregistered", "forget", "source-set", "1"]));
    assert!(forgotten.contains("forgotten: 1 audits"), "{forgotten}");
    assert!(run_ok(ts().args(["unregistered", "list"])).is_empty());
    let unknown = ts()
        .args(["unregistered", "forget", "shelf", "1"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
}

#[test]
fn set_add_with_no_path_registers_the_path_the_template_proposes() {
    let (f, dat) = pong_fixture();
    let root = f.dat_dir.path().join("sets");
    std::fs::create_dir_all(root.join("No-Intro/Test - System")).unwrap();
    std::fs::create_dir_all(root.join("Mine/Test - System")).unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(ts().args(["dat", "import"]).arg(&dat));

    let out = ts()
        .args([
            "set",
            "add",
            "sets",
            "--dat",
            "No-Intro/Test - System",
            "--template",
            "{platform}",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("type the path"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let added = run_ok(ts().args(["set", "add", "sets", "--dat", "No-Intro/Test - System"]));

    assert!(added.contains("sets:No-Intro/Test - System"), "{added}");
    let both = ts()
        .args([
            "set",
            "add",
            "sets",
            "Mine/Test - System",
            "--dat",
            "No-Intro/Test - System",
        ])
        .args(["--template", "Mine/{dat}"])
        .output()
        .unwrap();
    assert!(!both.status.success(), "a path and a template conflict");
}

#[test]
fn set_coverage_sets_shows_and_clears_a_source_sets_coverage() {
    let (f, dat) = pong_fixture();
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(ts().args(["dat", "import"]).arg(&dat));

    let added = run_ok(
        ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"])
            .args([
                "--coverage-preset",
                "USA + Canada",
                "--coverage-region",
                "Unknown",
            ]),
    );
    assert!(added.contains("covers USA, Canada, Unknown"), "{added}");
    let shown = run_ok(ts().args(["set", "coverage", rel]));
    assert!(shown.contains("covers USA, Canada, Unknown"), "{shown}");

    let unknown = ts()
        .args(["set", "coverage", rel, "--region", "Atlantis"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown regions: Atlantis"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    let changed = run_ok(ts().args(["set", "coverage", rel, "--region", "Japan"]));
    assert!(changed.contains("covers Japan"), "{changed}");

    let cleared = run_ok(ts().args(["set", "coverage", rel, "--clear"]));
    assert!(cleared.contains("covers every game"), "{cleared}");
    let shown = run_ok(ts().args(["set", "coverage", rel]));
    assert!(shown.contains("covers every game"), "{shown}");
}

#[test]
fn set_add_and_set_edit_say_and_switch_whether_stage_select_may_change_a_source_set() {
    let (f, dat) = pong_fixture();
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(ts().args(["dat", "import"]).arg(&dat));

    let added = run_ok(
        ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"])
            .arg("--read-only"),
    );
    assert!(
        added.contains("read-only: no repair writes to it"),
        "{added}"
    );
    let shown = run_ok(ts().args(["set", "edit", rel]));
    assert!(shown.contains("read-only"), "{shown}");

    let let_it = run_ok(ts().args(["set", "edit", rel, "--may-change"]));
    assert!(let_it.contains("Stage Select may change it"), "{let_it}");
    let roots = run_ok(ts().args(["root", "list"]));
    assert!(
        roots.contains(&format!("read-only but for {rel}")),
        "{roots}"
    );

    let stopped = run_ok(ts().args(["set", "edit", rel, "--read-only"]));
    assert!(stopped.contains("read-only"), "{stopped}");
    let both = ts()
        .args(["set", "edit", rel, "--may-change", "--read-only"])
        .output()
        .unwrap();
    assert!(!both.status.success(), "the two conflict");
}

#[test]
fn version_prints_the_pinned_clone_list_commit() {
    let pins = include_str!("../../../../third_party/clone-lists/pins.toml");
    let commits: Vec<&str> = pins
        .lines()
        .filter_map(|l| l.strip_prefix("commit = \""))
        .map(|l| l.trim_end_matches('"'))
        .collect();
    assert_eq!(commits.len(), 1, "the clone-list commit");

    let out = stage_select().arg("--version").output().unwrap();

    assert!(out.status.success());
    let version = String::from_utf8_lossy(&out.stdout);
    assert!(
        version.starts_with(&format!("stage-select {}\n", env!("CARGO_PKG_VERSION"))),
        "{version}"
    );
    for commit in commits {
        assert!(version.contains(commit), "{commit} in {version}");
    }
}

#[test]
fn a_stored_audit_without_a_store_fails_with_a_message() {
    let out = stage_select()
        .env_remove("STAGE_SELECT_STORE")
        .args(["audit", "--set", "Anything"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--store"));
}

#[test]
fn select_prints_the_selection_and_refuses_a_profile_it_cannot_read() {
    let (f, dat) = pong_fixture();
    let profile = f.dat_dir.path().join("profile.json");
    std::fs::write(&profile, r#"{"region_order": ["USA"]}"#).unwrap();

    let stdout = run_ok(
        stage_select()
            .arg("select")
            .arg("--dat")
            .arg(&dat)
            .arg("--profile")
            .arg(&profile),
    );
    let selection: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(selection["schema"], "stage-select.selection/1");
    assert_eq!(selection["system"], "Test - System");
    assert_eq!(selection["groups"][0]["kept"][0]["title"], "Pong (USA)");

    std::fs::write(&profile, r#"{"region_ordr": ["USA"]}"#).unwrap();
    let out = stage_select()
        .arg("select")
        .arg("--dat")
        .arg(&dat)
        .arg("--profile")
        .arg(&profile)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("region_ordr"));
}

#[test]
fn select_reads_the_support_files_it_is_given_for_a_listxml() {
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../selection/tests/data");
    let f = Fixture::new();
    let profile = f.dat_dir.path().join("profile.json");
    std::fs::write(&profile, "{}").unwrap();
    let stdout = run_ok(
        stage_select()
            .arg("select")
            .arg("--dat")
            .arg(data.join("arcade.xml"))
            .arg("--profile")
            .arg(&profile)
            .arg("--support-file")
            .arg(data.join("support/catver.ini")),
    );
    let selection: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        selection["pins"]["arcade"]["support_files"][0]["kind"],
        "catver"
    );
}

#[test]
fn the_engine_choice_and_the_conformance_command_are_gone() {
    let (f, dat) = pong_fixture();
    let profile = f.dat_dir.path().join("profile.json");
    std::fs::write(&profile, "{}").unwrap();
    for args in [
        &["select", "--engine", "builtin", "--profile"][..],
        &["conformance", "--profile"][..],
    ] {
        let out = stage_select()
            .args(args)
            .arg(&profile)
            .arg("--dat")
            .arg(&dat)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
    }
}

#[test]
fn an_ingest_folder_is_registered_scanned_and_its_items_listed() {
    let f = Fixture::new();
    let inbox = f.dat_dir.path().join("in");
    std::fs::create_dir_all(&inbox).unwrap();
    write_file(&inbox.join("mystery.bin"), b"nobody knows");
    let store = f.dat_dir.path().join("store");
    let opened = stage_select_store::Store::open(&store).unwrap();
    opened
        .register_root(
            &stage_select_api::roots::RootSpec::new("in", &inbox)
                .of_kind(stage_select_api::roots::RootKind::Ingest),
            "test",
        )
        .unwrap();
    opened
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
            "test",
        )
        .unwrap();
    drop(opened);
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };

    let unregistered = ts().args(["ingest", "scan", "in"]).output().unwrap();
    assert!(!unregistered.status.success());
    run_ok(ts().args(["ingest", "register", "in"]));
    run_ok(ts().args(["ingest", "scan", "in"]));
    let arriving = run_ok(ts().args(["ingest", "items", "in"]));
    assert!(arriving.contains("\tarriving\tmystery.bin"), "{arriving}");
    run_ok(ts().args(["ingest", "scan", "in"]));
    let assessed = run_ok(ts().args(["ingest", "items", "in"]));
    assert!(
        assessed.contains("\tassessed\tmystery.bin\taddition"),
        "{assessed}"
    );
}

#[test]
fn dat_import_takes_no_provider_and_says_when_the_header_names_none() {
    let f = Fixture::new();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    let help = run_ok(ts().args(["dat", "import", "--help"]));
    assert!(!help.contains("--provider"), "{help}");
    let dat = write_dat_from(
        f.dat_dir.path(),
        "homebrew.dat",
        "1",
        "Mine",
        &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
    );

    let imported = run_ok(ts().args(["dat", "import"]).arg(&dat));

    assert!(
        imported.contains("unknown-logiqx/Test - System")
            && imported.contains("doesn't say which provider"),
        "{imported}"
    );
    let refused = ts()
        .args(["dat", "import", "--provider", "Mine"])
        .arg(&dat)
        .output()
        .unwrap();
    assert!(!refused.status.success());
}

#[test]
fn dat_delete_refuses_while_a_source_set_uses_the_dat_and_deletes_once_none_does() {
    let (f, dat) = pong_fixture();
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    let imported = run_ok(ts().args(["dat", "import"]).arg(&dat));
    let sha = imported
        .split_whitespace()
        .find(|w| w.len() == 64)
        .unwrap()
        .to_string();
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));

    let refused = ts().args(["dat", "delete", &sha]).output().unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("1 source set uses its DAT") && stderr.contains("Nothing was changed"),
        "{stderr}"
    );

    run_ok(ts().args(["set", "unregister", rel]));
    let deleted = run_ok(ts().args(["dat", "delete", &sha]));
    assert!(deleted.contains("deleted"), "{deleted}");
    assert!(deleted.contains("no longer listed"), "{deleted}");
}

#[test]
fn support_file_import_keeps_a_support_file_and_list_prints_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    let ini = dir.path().join("languages.ini");
    std::fs::write(
        &ini,
        ";; languages.ini 0.289 / 21-Aug-26 / MAME 0.289 ;;\r\n[English]\r\npong\r\n",
    )
    .unwrap();

    let imported = run_ok(ts().args(["support-file", "import"]).arg(&ini));
    assert!(
        imported.starts_with("languages.ini for MAME 0.289: support file ")
            && imported.ends_with(" imported\n"),
        "{imported}"
    );
    let again = run_ok(ts().args(["support-file", "import"]).arg(&ini));
    assert!(again.ends_with(" already imported\n"), "{again}");
    let listed = run_ok(ts().args(["support-file", "list"]));
    assert!(
        listed.starts_with("languages.ini\tMAME 0.289\tlanguages.ini\timported\t"),
        "{listed}"
    );

    let other = dir.path().join("notes.ini");
    std::fs::write(&other, "[English]\r\npong\r\n").unwrap();
    let out = ts()
        .args(["support-file", "import"])
        .arg(&other)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a MAME support file"), "{err}");
}

/// A root is registered, edited and unregistered from the command line, through the rules the service applies, and
/// each change is recorded as the command line's.
#[test]
fn a_root_is_registered_edited_and_unregistered_from_the_command_line() {
    let (f, dat) = pong_fixture();
    let store = f.dat_dir.path().join("store");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    let missing = ts()
        .args(["root", "register", "sets"])
        .arg(root.join("nowhere"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("doesn't exist"));

    let registered = run_ok(
        ts().args(["root", "register", "sets"])
            .arg(root)
            .args(["--readers", "4"]),
    );
    assert!(registered.contains("sets\t"), "{registered}");
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(
        ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"])
            .arg("--read-only"),
    );
    let held = ts().args(["root", "unregister", "sets"]).output().unwrap();
    assert!(!held.status.success());
    assert!(String::from_utf8_lossy(&held.stderr).contains(&format!("source set {rel}")));

    run_ok(ts().args([
        "root",
        "edit",
        "sets",
        "--writable-path",
        rel,
        "--readers",
        "default",
    ]));
    let listed = run_ok(ts().args(["root", "list"]));
    assert_eq!(
        listed,
        format!(
            "sets\t{}\tgeneral\tread-only but for {rel}\n",
            root.display()
        )
    );
    run_ok(ts().args(["set", "unregister", rel]));
    run_ok(ts().args(["root", "unregister", "sets"]));
    assert!(run_ok(ts().args(["root", "list"])).starts_with("no roots are registered"));

    let changes = stage_select_store::Store::open(&store)
        .unwrap()
        .root_changes("sets")
        .unwrap();
    assert_eq!(
        changes
            .iter()
            .map(|c| (c.action.as_str(), c.changed_by.as_str()))
            .collect::<Vec<_>>(),
        [
            ("registered", "command line"),
            ("edited", "command line"),
            ("edited", "Stage Select, unregistering a source set"),
            ("unregistered", "command line")
        ]
    );
}

#[test]
fn root_register_and_set_add_create_a_missing_folder_with_create() {
    let (f, dat) = pong_fixture();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    let root = f.dat_dir.path().join("new/sets");
    let registered = run_ok(
        ts().args(["root", "register", "sets"])
            .arg(&root)
            .arg("--create"),
    );
    assert!(
        registered.contains(&format!("created folder {}", root.display())),
        "{registered}"
    );
    assert!(root.is_dir());

    run_ok(ts().args(["dat", "import"]).arg(&dat));
    let add = [
        "set",
        "add",
        "sets",
        "Empty",
        "--dat",
        "No-Intro/Test - System",
    ];
    let both = ts()
        .args(add)
        .args(["--create", "--read-only"])
        .output()
        .unwrap();
    assert!(!both.status.success(), "the two conflict");
    let added = run_ok(ts().args(add).arg("--create"));
    assert!(
        added.contains("created folder Empty in root sets"),
        "{added}"
    );
    assert!(root.join("Empty").is_dir());
}

#[test]
fn the_configuration_file_flag_is_gone_and_a_trusted_proxy_must_be_a_cidr() {
    let f = Fixture::new();
    let store = f.dat_dir.path().join("store");

    for args in [
        &["--config", "stage-select.toml", "serve"][..],
        &["serve", "--config", "stage-select.toml"],
    ] {
        let out = stage_select()
            .env("STAGE_SELECT_STORE", &store)
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unexpected argument '--config'"),
            "{stderr}"
        );
    }
    let out = stage_select()
        .env("STAGE_SELECT_STORE", &store)
        .args(["serve", "--identity-header", "Remote-User"])
        .args(["--trusted-proxies", "10.0.0.0/8,nonsense"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "--trusted-proxies (STAGE_SELECT_TRUSTED_PROXIES): \"nonsense\" is not a CIDR"
        ),
        "{stderr}"
    );
    assert!(
        !store.exists(),
        "nothing is opened when the flags don't parse"
    );
}

/// A `stage-select serve` process, stopped when dropped.
struct Serving(std::process::Child);

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_service_starts_from_environment_variables_alone() {
    use std::io::BufRead;

    let f = Fixture::new();
    let mut serving = Serving(
        stage_select()
            .arg("serve")
            .env("STAGE_SELECT_STORE", f.dat_dir.path().join("store"))
            .env("STAGE_SELECT_LISTEN", "127.0.0.1:0")
            .env("STAGE_SELECT_IDENTITY_HEADER", "Remote-User")
            .env(
                "STAGE_SELECT_TRUSTED_PROXIES",
                " 192.0.2.0/24 , 127.0.0.0/8,::1/128,",
            )
            .env_remove("STAGE_SELECT_OPERATOR_TOKEN_FILE")
            .env_remove("STAGE_SELECT_DEV_IDENTITY")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = std::io::BufReader::new(serving.0.stderr.take().unwrap());
    let mut said = String::new();
    let mut url = None;
    for line in stderr.lines() {
        let line = line.unwrap();
        said.push_str(&line);
        said.push('\n');
        if let Some((_, at)) = line.split_once(" on http://") {
            url = Some(format!("http://{at}/api/v1/jobs"));
            break;
        }
    }
    let url = url.unwrap_or_else(|| panic!("the service didn't start: {said}"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    let named = agent.get(&url).header("Remote-User", "You").call().unwrap();
    let anonymous = agent.get(&url).call().unwrap();

    assert_eq!(named.status(), 200);
    assert_eq!(anonymous.status(), 401);
}

#[test]
fn library_remove_and_restore_say_why_they_are_refused() {
    let (f, dat) = pong_fixture();
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = stage_select();
        cmd.env("STAGE_SELECT_STORE", &store);
        cmd
    };
    let play = f.dat_dir.path().join("play");
    std::fs::create_dir_all(&play).unwrap();
    run_ok(ts().args(["root", "register", "sets"]).arg(root));
    run_ok(
        ts().args(["root", "register", "play", "--writable"])
            .arg(&play),
    );
    run_ok(ts().args(["dat", "import"]).arg(&dat));
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "No-Intro/Test - System"]));
    let folder = {
        let opened = stage_select_store::Store::open(&store).unwrap();
        opened
            .save_profile(
                "Mine",
                &stage_select_api::selection::SelectionProfile::default(),
                None,
                "test",
                &[],
                stage_select_store::Workers {
                    readers: 1,
                    hashers: 1,
                },
            )
            .unwrap();
        opened
            .add_library_folder(&stage_select_api::build::AddLibraryFolder {
                root: "play".into(),
                path: Some("atari".into()),
                template: None,
                source_set: rel.into(),
                profile: "Mine".into(),
            })
            .unwrap()
            .id
            .to_string()
    };

    let refused = |args: &[&str]| {
        let out = ts().args(args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        String::from_utf8(out.stderr).unwrap()
    };
    let why = refused(&[
        "library",
        "remove",
        &folder,
        "Pong (USA)",
        "--action",
        "send-back",
    ]);
    assert!(why.contains("nothing was built"), "{why}");
    // Without --action it sets the games aside: refused for the folder, not for the missing option.
    let why = refused(&["library", "remove", &folder, "Pong (USA)"]);
    assert!(why.contains("nothing was built"), "{why}");
    let why = refused(&["library", "restore", &folder, "Pong (USA)"]);
    assert!(why.contains("Pong (USA) isn't removed"), "{why}");
    let why = refused(&[
        "library",
        "remove",
        &folder,
        "Pong (USA)",
        "--action",
        "lose",
    ]);
    assert!(why.contains("send-back"), "{why}");
}
