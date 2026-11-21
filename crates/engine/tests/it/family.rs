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
    // Every DAT, not only MAME's: two identical tracks of one disc, of which only the first is held.
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

#[test]
fn in_a_games_own_zip_a_misnamed_entry_counts_for_a_rom_no_entry_is_named_for() {
    let f = Fixture::new();
    let dat = alien_trilogy(&f);
    // The misnamed copies come first, so they cannot take a track that an Entry later in the ZIP is named for.
    let mut members: Vec<(&str, &[u8])> = vec![
        ("audio copy 1.bin", b"identical audio"),
        ("audio copy 2.bin", b"identical audio"),
    ];
    members.extend(TRACKS[2..].iter().map(|t| (*t, &b"identical audio"[..])));
    members.push(("Alien Trilogy (Germany).cue", b"cue sheet"));
    members.push(("Alien Trilogy (Germany) (Track 01).bin", b"data track"));
    write_zip(&f.set_path("Alien Trilogy (Germany).zip"), &members);

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Alien Trilogy (Germany)");
    assert_eq!(g.status, GameStatus::Complete, "{:?}", g.missing_roms);

    // One misnamed copy fewer leaves exactly one track short.
    std::fs::remove_file(f.set_path("Alien Trilogy (Germany).zip")).unwrap();
    members.remove(0);
    write_zip(&f.set_path("Alien Trilogy (Germany).zip"), &members);

    let report = audit(&dat, f.set.path()).unwrap();

    let g = game(&report, "Alien Trilogy (Germany)");
    assert_eq!(g.status, GameStatus::Incomplete);
    assert_eq!(g.missing_roms.len(), 1, "{:?}", g.missing_roms);
}
