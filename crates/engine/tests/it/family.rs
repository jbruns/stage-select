//! A Game counts its family's files and names its Dependencies (ADR 0025): its parent's and its BIOS's ZIPs are its
//! own, and a BIOS or device it needs has a Game Status of its own, which never changes the Game's.

use crate::common::*;
use title_select_engine::{AvailableElsewhere, Dependency, Game, GameStatus, Report, audit};

/// A BIOS; a device with a ROM, naming a ROM-less device; a parent on the BIOS; its clone, which lists the parent's
/// graphics under a name of its own, as a ROM with `merge=` does; and an unrelated machine sharing the BIOS's ROM.
fn family(f: &Fixture) -> std::path::PathBuf {
    write_listxml(
        f.dat_dir.path(),
        &[
            MachineSpec {
                name: "acmebios",
                attrs: r#"isbios="yes""#,
                roms: vec![rom("bios.u1", b"bios")],
                devices: &[],
            },
            MachineSpec {
                name: "acmesnd",
                attrs: r#"isdevice="yes" runnable="no""#,
                roms: vec![rom("snd.u2", b"sound")],
                devices: &["z80"],
            },
            MachineSpec {
                name: "z80",
                attrs: r#"isdevice="yes" runnable="no""#,
                roms: vec![],
                devices: &[],
            },
            MachineSpec {
                name: "blaster",
                attrs: r#"romof="acmebios""#,
                roms: vec![
                    rom("bios.u1", b"bios"),
                    rom("prog.u3", b"program"),
                    rom("gfx.u4", b"graphics"),
                ],
                devices: &["acmesnd", "z80"],
            },
            MachineSpec {
                name: "blasterj",
                attrs: r#"cloneof="blaster" romof="blaster""#,
                roms: vec![
                    rom("bios.u1", b"bios"),
                    rom("progj.u3", b"program j"),
                    rom("gfxj.u4", b"graphics"),
                ],
                devices: &["acmesnd", "z80"],
            },
            MachineSpec {
                name: "rival",
                attrs: "",
                roms: vec![rom("rival.u1", b"bios"), rom("rival.u2", b"rival")],
                devices: &[],
            },
        ],
    )
}

fn game<'r>(report: &'r Report, name: &str) -> &'r Game {
    report.games.iter().find(|g| g.name == name).unwrap()
}

fn dependency(game: &str, status: GameStatus) -> Dependency {
    Dependency {
        game: game.into(),
        status,
    }
}

#[test]
fn a_clone_in_a_split_set_is_complete_from_its_parents_zip_and_its_bios_zip() {
    let f = Fixture::new();
    let dat = family(&f);
    write_zip(&f.set_path("acmebios.zip"), &[("bios.u1", b"bios")]);
    write_zip(&f.set_path("acmesnd.zip"), &[("snd.u2", b"sound")]);
    write_zip(
        &f.set_path("blaster.zip"),
        &[("prog.u3", b"program"), ("gfx.u4", b"graphics")],
    );
    write_zip(&f.set_path("blasterj.zip"), &[("progj.u3", b"program j")]);

    let report = audit(&dat, f.set.path()).unwrap();

    for name in ["acmebios", "acmesnd", "blaster", "blasterj"] {
        let g = game(&report, name);
        assert_eq!(g.status, GameStatus::Complete, "{name}");
        assert!(g.available_elsewhere.is_empty(), "{name}");
    }
    assert_eq!(
        game(&report, "blasterj").dependencies,
        [
            dependency("acmebios", GameStatus::Complete),
            dependency("acmesnd", GameStatus::Complete),
            dependency("z80", GameStatus::NothingToCheck),
        ]
    );
}

#[test]
fn a_clone_in_a_merged_set_is_complete_from_its_parents_zip_and_the_bios_zip() {
    let f = Fixture::new();
    let dat = family(&f);
    write_zip(&f.set_path("acmebios.zip"), &[("bios.u1", b"bios")]);
    write_zip(
        &f.set_path("blaster.zip"),
        &[
            ("prog.u3", b"program"),
            ("gfx.u4", b"graphics"),
            ("progj.u3", b"program j"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    assert_eq!(game(&report, "blaster").status, GameStatus::Complete);
    let clone = game(&report, "blasterj");
    assert_eq!(clone.status, GameStatus::Complete);
    assert!(clone.missing_roms.is_empty());
}

#[test]
fn a_machine_whose_device_is_missing_is_complete_and_names_the_missing_dependency() {
    let f = Fixture::new();
    let dat = family(&f);
    // Non-merged: the machine's ZIP holds its BIOS's ROM, which counts for the machine and not for the BIOS.
    write_zip(
        &f.set_path("blaster.zip"),
        &[
            ("bios.u1", b"bios"),
            ("prog.u3", b"program"),
            ("gfx.u4", b"graphics"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let blaster = game(&report, "blaster");
    assert_eq!(blaster.status, GameStatus::Complete);
    assert_eq!(
        blaster.dependencies,
        [
            dependency("acmebios", GameStatus::Missing),
            dependency("acmesnd", GameStatus::Missing),
            dependency("z80", GameStatus::NothingToCheck),
        ]
    );
    let satisfied: Vec<bool> = blaster
        .dependencies
        .iter()
        .map(|d| d.status.satisfies_dependency())
        .collect();
    assert_eq!(
        satisfied,
        [false, false, true],
        "a ROM-less device is satisfied"
    );
}

#[test]
fn a_rom_only_in_an_unrelated_games_zip_is_still_available_elsewhere() {
    let f = Fixture::new();
    let dat = family(&f);
    write_zip(&f.set_path("acmebios.zip"), &[("bios.u1", b"bios")]);
    write_zip(&f.set_path("rival.zip"), &[("rival.u2", b"rival")]);

    let report = audit(&dat, f.set.path()).unwrap();

    let rival = game(&report, "rival");
    assert_eq!(rival.status, GameStatus::Incomplete);
    assert_eq!(rival.missing_roms, ["rival.u1"]);
    assert_eq!(
        rival.available_elsewhere,
        [AvailableElsewhere {
            rom: "rival.u1".into(),
            games: vec!["acmebios".into()],
        }]
    );
    assert!(rival.dependencies.is_empty());
}

#[test]
fn in_a_games_own_zip_one_entry_counts_for_one_of_its_roms_with_the_same_checksums() {
    let f = Fixture::new();
    // Every DAT, not only MAME's: two identical tracks of one disc, of which only the first is held. The Entry is
    // named for the first, so it never counts for the second by its checksums.
    let dat = f.dat(&[(
        "Game (USA)",
        vec![
            rom("Game (USA) (Track 1).bin", b"silence"),
            rom("Game (USA) (Track 2).bin", b"silence"),
        ],
    )]);
    write_zip(
        &f.set_path("Game (USA).zip"),
        &[("Game (USA) (Track 1).bin", b"silence")],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Game (USA)");
    assert_eq!(g.status, GameStatus::Incomplete);
    assert_eq!(g.missing_roms, ["Game (USA) (Track 2).bin"]);
    assert_eq!(report.entries[0].roms.len(), 1);
}

/// The tracks of a Redump disc that are byte-identical, as `Alien Trilogy (Germany)`'s twelve are.
const TRACKS: [&str; 12] = [
    "Alien Trilogy (Germany) (Track 02).bin",
    "Alien Trilogy (Germany) (Track 03).bin",
    "Alien Trilogy (Germany) (Track 04).bin",
    "Alien Trilogy (Germany) (Track 05).bin",
    "Alien Trilogy (Germany) (Track 06).bin",
    "Alien Trilogy (Germany) (Track 07).bin",
    "Alien Trilogy (Germany) (Track 08).bin",
    "Alien Trilogy (Germany) (Track 09).bin",
    "Alien Trilogy (Germany) (Track 10).bin",
    "Alien Trilogy (Germany) (Track 11).bin",
    "Alien Trilogy (Germany) (Track 12).bin",
    "Alien Trilogy (Germany) (Track 13).bin",
];

fn alien_trilogy(f: &Fixture) -> std::path::PathBuf {
    let mut roms = vec![
        rom("Alien Trilogy (Germany).cue", b"cue sheet"),
        rom("Alien Trilogy (Germany) (Track 01).bin", b"data track"),
    ];
    roms.extend(TRACKS.iter().map(|t| rom(t, b"identical audio")));
    f.dat(&[("Alien Trilogy (Germany)", roms)])
}

#[test]
fn a_disc_with_one_of_its_identical_tracks_held_is_incomplete() {
    let f = Fixture::new();
    let dat = alien_trilogy(&f);
    write_zip(
        &f.set_path("Alien Trilogy (Germany).zip"),
        &[
            ("Alien Trilogy (Germany).cue", b"cue sheet"),
            ("Alien Trilogy (Germany) (Track 01).bin", b"data track"),
            (TRACKS[0], b"identical audio"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Alien Trilogy (Germany)");
    assert_eq!(g.status, GameStatus::Incomplete);
    assert_eq!(g.missing_roms, TRACKS[1..]);
}

/// Two misnamed copies of the identical track, walked before the ZIP's Entries named for the other tracks, so they
/// cannot take a track an Entry is named for.
fn alien_trilogy_with_misnamed_copies(f: &Fixture, copies: &[&str]) -> std::path::PathBuf {
    let dat = alien_trilogy(f);
    let mut members: Vec<(&str, &[u8])> = copies
        .iter()
        .map(|c| (*c, &b"identical audio"[..]))
        .collect();
    members.extend(TRACKS[2..].iter().map(|t| (*t, &b"identical audio"[..])));
    members.push(("Alien Trilogy (Germany).cue", b"cue sheet"));
    members.push(("Alien Trilogy (Germany) (Track 01).bin", b"data track"));
    write_zip(&f.set_path("Alien Trilogy (Germany).zip"), &members);
    dat
}

#[test]
fn in_a_games_own_zip_a_misnamed_entry_counts_for_a_rom_no_entry_is_named_for() {
    let f = Fixture::new();
    let dat = alien_trilogy_with_misnamed_copies(&f, &["audio copy 1.bin", "audio copy 2.bin"]);

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Alien Trilogy (Germany)");
    assert_eq!(g.status, GameStatus::Complete, "{:?}", g.missing_roms);
}

#[test]
fn in_a_games_own_zip_one_misnamed_entry_does_not_stand_in_for_two_tracks() {
    let f = Fixture::new();
    let dat = alien_trilogy_with_misnamed_copies(&f, &["audio copy.bin"]);

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Alien Trilogy (Germany)");
    assert_eq!(g.status, GameStatus::Incomplete);
    assert_eq!(g.missing_roms.len(), 1, "{:?}", g.missing_roms);
}

#[test]
fn in_a_games_own_zip_a_misnamed_entry_skips_a_rom_a_loose_file_counts_for() {
    let f = Fixture::new();
    let dat = f.dat(&[(
        "Disc (USA)",
        vec![
            rom("Disc (USA) (Track 1).bin", b"silence"),
            rom("Disc (USA) (Track 2).bin", b"silence"),
        ],
    )]);
    // A loose file in no Game's folder counts for the first track by its name; the Game's ZIP holds a misnamed copy.
    std::fs::write(f.set_path("Disc (USA) (Track 1).bin"), b"silence").unwrap();
    write_zip(&f.set_path("Disc (USA).zip"), &[("track.bin", b"silence")]);

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Disc (USA)");
    assert_eq!(g.status, GameStatus::Complete, "{:?}", g.missing_roms);
}

/// `spec`'s `<rom>` element with `attrs` added, such as `optional="yes"`.
fn rom_with(spec: RomSpec, attrs: &str) -> String {
    rom_xml(&spec).replace("/>", &format!(" {attrs}/>"))
}

/// A nodump `<rom>` of `size` bytes, with no checksums.
fn nodump(name: &str, size: usize) -> String {
    format!(r#"<rom name="{name}" size="{size}" status="nodump"/>"#)
}

/// Writes a MAME ListXML of `machines`, each a name, its attributes and its `<rom>` elements, and returns its path.
fn write_machines(f: &Fixture, machines: &[(&str, &str, Vec<String>)]) -> std::path::PathBuf {
    let mut xml = String::from(r#"<?xml version="1.0"?><mame build="0.289 (mame0289)">"#);
    for (name, attrs, roms) in machines {
        xml += &format!(r#"<machine name="{name}" {attrs}><description>{name}</description>"#);
        xml += &roms.concat();
        xml += "</machine>";
    }
    xml += "</mame>";
    let path = f.dat_dir.path().join("mame.xml");
    write_file(&path, xml.as_bytes());
    path
}

fn entry<'r>(report: &'r Report, member: &str) -> &'r title_select_engine::Entry {
    report
        .entries
        .iter()
        .find(|e| e.member.as_deref() == Some(member))
        .unwrap()
}

#[test]
fn a_nodump_rom_whose_name_is_in_its_parents_zip_never_matches() {
    let f = Fixture::new();
    let dat = write_machines(
        &f,
        &[
            ("blaster", "", vec![rom_xml(&rom("prog.u3", b"program"))]),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                vec![rom_xml(&rom("progj.u3", b"program j")), nodump("pal.u1", 8)],
            ),
        ],
    );
    // Merged: the clone's files are in its parent's ZIP, a file under its nodump ROM's name among them.
    write_zip(
        &f.set_path("blaster.zip"),
        &[
            ("prog.u3", b"program"),
            ("progj.u3", b"program j"),
            ("pal.u1", b"8 bytes!"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let pal = entry(&report, "pal.u1");
    assert_eq!(pal.finding, title_select_engine::Finding::Mismatched);
    assert!(pal.roms.is_empty());
    let named = pal.named_rom.as_ref().unwrap();
    assert_eq!(
        (named.game.as_str(), named.name.as_str()),
        ("blasterj", "pal.u1")
    );
    assert_eq!(pal.reason.as_deref(), Some("no good dump known"));
    let clone = game(&report, "blasterj");
    assert_eq!(clone.status, GameStatus::Complete);
    assert_eq!(clone.missing_roms, ["pal.u1"]);
    assert_eq!(clone.not_dumped, ["pal.u1"]);
}

#[test]
fn a_file_under_a_nodump_roms_name_never_counts_by_its_checksums_for_another_rom_of_the_game() {
    let f = Fixture::new();
    let dat = write_machines(
        &f,
        &[(
            "blaster",
            "",
            vec![
                rom_xml(&rom("prog.u3", b"program")),
                rom_xml(&rom("gfx.u4", b"8 bytes!")),
                nodump("pal.u1", 8),
            ],
        )],
    );
    // The file under the nodump ROM's name has the bytes of `gfx.u4`, which no file is named for.
    write_zip(
        &f.set_path("blaster.zip"),
        &[("prog.u3", b"program"), ("pal.u1", b"8 bytes!")],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let blaster = game(&report, "blaster");
    assert_eq!(blaster.status, GameStatus::Incomplete);
    assert_eq!(blaster.missing_roms, ["gfx.u4", "pal.u1"]);
}

#[test]
fn a_clone_whose_only_absent_roms_are_optional_or_nodump_is_complete_from_its_parents_zip() {
    let f = Fixture::new();
    let dat = write_machines(
        &f,
        &[
            (
                "acmebios",
                r#"isbios="yes""#,
                vec![
                    rom_xml(&rom("bios.u1", b"bios")),
                    rom_with(rom("diag.u9", b"diagnostics"), r#"optional="yes""#),
                ],
            ),
            (
                "blaster",
                r#"romof="acmebios""#,
                vec![
                    rom_xml(&rom("bios.u1", b"bios")),
                    rom_xml(&rom("prog.u3", b"program")),
                    rom_with(rom("snd.u5", b"sound"), r#"optional="yes""#),
                ],
            ),
            (
                "blasterj",
                r#"cloneof="blaster" romof="blaster""#,
                vec![
                    rom_xml(&rom("bios.u1", b"bios")),
                    rom_xml(&rom("progj.u3", b"program j")),
                    rom_with(rom("snd.u5", b"sound"), r#"optional="yes""#),
                    rom_with(rom("sndj.u6", b"sound j"), r#"optional="yes""#),
                    nodump("pal.u1", 8),
                ],
            ),
        ],
    );
    // Split: the BIOS's ZIP holds its needed ROM, the parent's its own and its clone's, and one of the clone's two
    // optional ROMs; the BIOS's optional ROM, the clone's other one and its nodump ROM are nowhere.
    write_zip(&f.set_path("acmebios.zip"), &[("bios.u1", b"bios")]);
    write_zip(
        &f.set_path("blaster.zip"),
        &[
            ("prog.u3", b"program"),
            ("progj.u3", b"program j"),
            ("snd.u5", b"sound"),
        ],
    );

    let report = audit(&dat, f.set.path()).unwrap();

    let clone = game(&report, "blasterj");
    assert_eq!(clone.status, GameStatus::Complete);
    // An optional ROM in an ancestor's ZIP counts.
    assert_eq!(clone.missing_roms, ["sndj.u6", "pal.u1"]);
    assert_eq!(clone.optional_roms, ["snd.u5", "sndj.u6"]);
    // A Dependency lacking only an optional ROM is Complete too.
    assert_eq!(
        clone.dependencies,
        [dependency("acmebios", GameStatus::Complete)]
    );
}

#[test]
fn a_mismatched_entry_is_named_for_the_holding_games_own_rom_then_a_clones_then_another_descendants()
 {
    // `blaster.zip` holds `x.u9`, whose bytes match nothing. `blasterb` is on `blaster` by `romof` alone, so
    // `blaster` is its ancestor but not its parent; `blasterj` and `blasterk` are its clones. `rival`, outside the
    // family, lists `x.u9` first of all, and `blasterb` before the others, so DAT order alone would name neither of
    // the nearer ones. Each case gives what `blasterb`, `blasterj`, `blasterk` and `blaster` list as `x.u9`.
    let x = |payload: &[u8]| Some(rom_xml(&rom("x.u9", payload)));
    let nodump_x = || Some(nodump("x.u9", 3));
    let no_good = Some("no good dump known");
    let cases = [
        ([x(b"b"), x(b"j"), x(b"k"), x(b"own")], "blaster", None),
        ([x(b"b"), x(b"j"), x(b"k"), None], "blasterj", None),
        // Nodump comes first only among equals, such as two clones: never before a nearer Game's ROM.
        ([None, x(b"j"), nodump_x(), None], "blasterk", no_good),
        ([None, nodump_x(), None, x(b"own")], "blaster", None),
        ([nodump_x(), x(b"j"), None, None], "blasterj", None),
        ([nodump_x(), None, None, None], "blasterb", no_good),
    ];
    for (i, (listed, named_game, reason)) in cases.into_iter().enumerate() {
        let f = Fixture::new();
        let [b, j, k, own] = listed;
        let clone = r#"cloneof="blaster" romof="blaster""#;
        let machines = [
            ("rival", "", vec![x(b"rival").unwrap()]),
            ("blasterb", r#"romof="blaster""#, b.into_iter().collect()),
            ("blasterj", clone, j.into_iter().collect()),
            ("blasterk", clone, k.into_iter().collect()),
            (
                "blaster",
                "",
                [rom_xml(&rom("prog.u3", b"program"))]
                    .into_iter()
                    .chain(own)
                    .collect(),
            ),
        ];
        let dat = write_machines(&f, &machines);
        write_zip(
            &f.set_path("blaster.zip"),
            &[("prog.u3", b"program"), ("x.u9", b"bad")],
        );

        let report = audit(&dat, f.set.path()).unwrap();

        let e = entry(&report, "x.u9");
        assert_eq!(
            e.finding,
            title_select_engine::Finding::Mismatched,
            "case {i}"
        );
        let named = e.named_rom.as_ref().unwrap();
        assert_eq!(named.game, named_game, "case {i}");
        assert_eq!(e.reason.as_deref(), reason, "case {i}");
    }
}

#[test]
fn in_a_games_own_zip_a_misnamed_entry_counts_for_a_needed_rom_before_an_optional_one() {
    let f = Fixture::new();
    // The optional ROM is listed first, with the needed one's checksums.
    let dat = write_machines(
        &f,
        &[(
            "blaster",
            "",
            vec![
                rom_with(rom("alt.u2", b"program"), r#"optional="yes""#),
                rom_xml(&rom("prog.u3", b"program")),
            ],
        )],
    );
    write_zip(&f.set_path("blaster.zip"), &[("copy.bin", b"program")]);

    let report = audit(&dat, f.set.path()).unwrap();

    let blaster = game(&report, "blaster");
    assert_eq!(blaster.status, GameStatus::Complete);
    assert_eq!(blaster.missing_roms, ["alt.u2"]);
}
