//! Telling which Game of a DAT a file holds.

use std::fs::File;

use crate::common::*;
use title_select_engine::build::{GameIndex, probe};
use title_select_engine::dat::Dat;
use title_select_engine::meter::Tally;

#[test]
fn a_file_is_a_game_whose_needed_roms_it_holds_without_its_optional_or_nodump_ones() {
    let f = Fixture::new();
    let path = f.dat(&[
        (
            "Game (USA)",
            vec![
                rom("game.a", b"needed"),
                rom("game.b", b"optional"),
                rom("game.c", b"nodump"),
            ],
        ),
        ("Exact (USA)", vec![rom("game.a", b"needed")]),
        ("Optional (USA)", vec![rom("game.d", b"only")]),
        (
            "Twin (USA)",
            vec![rom("twin.b", b"same"), rom("twin.a", b"same")],
        ),
    ]);
    let mut xml = std::fs::read_to_string(&path).unwrap();
    for (name, flag) in [
        ("game.b", r#"optional="yes""#),
        ("game.c", r#"status="nodump""#),
        ("game.d", r#"optional="yes""#),
        ("twin.b", r#"optional="yes""#),
    ] {
        xml = xml.replace(
            &format!(r#"<rom name="{name}""#),
            &format!(r#"<rom {flag} name="{name}""#),
        );
    }
    let dat = Dat::parse(xml.as_bytes(), &path).unwrap();
    let index = GameIndex::new(&dat);
    let game_of = |name: &str, members: &[(&str, &[u8])]| {
        let zip = f.set_path(name);
        write_zip(&zip, members);
        index
            .game_of(&probe(&File::open(&zip).unwrap(), &Tally::default()).unwrap())
            .map(str::to_owned)
    };

    let found = [
        // Exact lists game.a alone, so it is the game the file holds whole.
        game_of("needed.zip", &[("game.a", b"needed")]),
        game_of(
            "both.zip",
            &[("game.a", b"needed"), ("game.b", b"optional")],
        ),
        game_of("optional.zip", &[("game.b", b"optional")]),
        game_of("extra.zip", &[("game.a", b"needed"), ("other", b"other")]),
        game_of(
            "nodump.zip",
            &[("game.a", b"needed"), ("game.c", b"nodump")],
        ),
        game_of("only.zip", &[("game.d", b"only")]),
        // The payload is the needed twin.a, though the optional twin.b listed before it has the same checksums.
        game_of("twin.zip", &[("twin.a", b"same")]),
    ];

    let game = |name: &str| Some(name.to_owned());
    assert_eq!(
        found,
        [
            game("Exact (USA)"),
            game("Game (USA)"),
            None,
            None,
            None,
            game("Optional (USA)"),
            game("Twin (USA)"),
        ]
    );
}
