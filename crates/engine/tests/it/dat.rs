//! Reading the DAT formats a DAT Revision may be: Logiqx XML, and MAME's ListXML and software lists.

use std::path::Path;

use title_select_api::DatFormat;
use title_select_engine::dat::{Dat, DumpStatus, Signal};

/// The synthetic ListXML of `tests/data/listxml-family.xml`: a BIOS, a device chain, a parent and its clone, and a
/// mechanical non-clone, with nodump, baddump and optional ROMs and Disks.
fn family() -> Dat {
    let xml = include_bytes!("../data/listxml-family.xml");
    Dat::parse(xml, Path::new("listxml-family.xml")).unwrap()
}

/// The signals `dat` carries, in [`Signal::ALL`]'s order.
fn carried(dat: &Dat) -> Vec<Signal> {
    Signal::ALL
        .into_iter()
        .filter(|s| dat.carries(*s))
        .collect()
}

fn index(dat: &Dat, game: &str) -> usize {
    dat.games.iter().position(|g| g == game).unwrap()
}

#[test]
fn a_listxml_machine_says_whether_it_is_a_bios_device_or_mechanical_and_whether_it_runs() {
    let dat = family();
    let kinds: Vec<_> = dat
        .games
        .iter()
        .zip(&dat.machines)
        .map(|(g, m)| {
            (
                g.as_str(),
                m.is_bios,
                m.is_device,
                m.is_mechanical,
                m.runnable,
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("arcbios", true, false, false, true),
            ("acmesnd", false, true, false, false),
            ("z80", false, true, false, false),
            ("blaster", false, false, false, true),
            ("blasterj", false, false, false, true),
            ("crane", false, false, true, true),
        ]
        .map(|(g, b, d, m, r)| (g, Some(b), Some(d), Some(m), Some(r)))
    );
}

#[test]
fn a_listxml_machine_names_its_family_its_devices_its_samples_and_its_driver_source() {
    let dat = family();
    let blasterj = &dat.machines[index(&dat, "blasterj")];
    assert_eq!(
        dat.cloneof[index(&dat, "blasterj")].as_deref(),
        Some("blaster")
    );
    assert_eq!(blasterj.romof.as_deref(), Some("blaster"));
    let blaster = &dat.machines[index(&dat, "blaster")];
    assert_eq!(blaster.romof.as_deref(), Some("arcbios"));
    assert_eq!(blaster.source_file.as_deref(), Some("acme/blaster.cpp"));
    assert_eq!(
        blaster.device_refs.as_deref(),
        Some(&["acmesnd".to_string(), "z80".into()][..])
    );
    assert_eq!(blaster.sample_of.as_deref(), Some("blaster"));
    assert_eq!(
        blaster.samples.as_deref(),
        Some(&["laser".to_string(), "boom".into()][..])
    );
    let acmesnd = &dat.machines[index(&dat, "acmesnd")];
    assert_eq!(
        acmesnd.device_refs.as_deref(),
        Some(&["z80".to_string()][..])
    );
    assert_eq!(
        acmesnd.samples.as_deref(),
        Some(&[][..]),
        "known to play none"
    );
    assert_eq!(dat.machines[index(&dat, "arcbios")].romof, None);
}

#[test]
fn mame_listxml_is_the_mame_dat_versioned_by_its_build() {
    let xml = br#"<?xml version="1.0"?>
<!DOCTYPE mame [
<!ELEMENT mame (machine+)>
]>
<mame build="0.262 (mame0262)" debug="no" mameconfig="10">
  <machine name="pong" sourcefile="atari/pong.cpp">
    <description>Pong (Rev E)</description>
    <rom name="pong.a1" size="4" crc="0f0f0f0f" sha1="0123456789abcdef0123456789abcdef01234567"/>
    <rom name="nodump.b2" size="8" status="nodump"/>
  </machine>
  <machine name="z80" isdevice="yes"/>
</mame>
"#;
    let dat = Dat::parse(xml, Path::new("mame0262.xml")).unwrap();
    assert_eq!(dat.name, "MAME");
    assert_eq!(dat.version, "0.262 (mame0262)");
    assert_eq!(dat.games, ["pong", "z80"]);
    assert_eq!(dat.roms.len(), 2);
    assert_eq!(dat.roms[0].crc.as_deref(), Some("0f0f0f0f"));
}

#[test]
fn a_mame_software_list_is_named_by_its_list_and_skips_unnamed_reloaded_roms() {
    let xml = br#"<?xml version="1.0"?>
<!DOCTYPE softwarelist SYSTEM "softwarelist.dtd">
<softwarelist name="nes" description="Nintendo Entertainment System cartridges">
  <software name="smb">
    <description>Super Mario Bros.</description>
    <part name="cart" interface="nes_cart">
      <dataarea name="prg" size="32768">
        <rom name="smb.prg" size="32768" crc="5cf548d3" sha1="fefe07b0b3f8bbfa86c7b2c5d4a44a3f3a2b3c4d" offset="0"/>
        <rom size="32768" offset="0x8000" loadflag="reload"/>
      </dataarea>
    </part>
  </software>
</softwarelist>
"#;
    let dat = Dat::parse(xml, Path::new("nes.xml")).unwrap();
    assert_eq!(dat.name, "nes");
    assert_eq!(dat.version, "");
    assert_eq!(dat.games, ["smb"]);
    assert_eq!(dat.roms.len(), 1);
    assert_eq!(dat.roms[0].name, "smb.prg");
}

#[test]
fn a_software_list_rom_that_continues_or_skips_in_its_file_adds_to_that_files_size() {
    let xml = br#"<?xml version="1.0"?>
<softwarelist name="megadriv" description="Sega Mega Drive cartridges">
  <software name="game">
    <description>Game</description>
    <part name="cart" interface="megadriv_cart">
      <dataarea name="rom" size="49152">
        <rom name="game.bin" size="16384" crc="11111111" offset="0"/>
        <rom size="16384" offset="0x8000" loadflag="continue"/>
        <rom size="4096" offset="0" loadflag="ignore"/>
        <rom size="16384" offset="0x4000" loadflag="reload"/>
        <rom size="4096" offset="0xc000" loadflag="fill" value="0xff"/>
        <rom name="other.bin" size="8192" crc="22222222" offset="0x10000"/>
        <rom size="8192" offset="0x12000" loadflag="reload_plain"/>
      </dataarea>
    </part>
  </software>
</softwarelist>
"#;
    let dat = Dat::parse(xml, Path::new("megadriv.xml")).unwrap();
    let sizes: Vec<(&str, u64)> = dat.roms.iter().map(|r| (r.name.as_str(), r.size)).collect();
    assert_eq!(
        sizes,
        [("game.bin", 16384 + 16384 + 4096), ("other.bin", 8192)]
    );
    assert_eq!(dat.candidates(36864, "11111111").collect::<Vec<_>>(), [0]);
    assert_eq!(dat.candidates(16384, "11111111").count(), 0);
}

#[test]
fn a_software_list_rom_is_as_large_as_its_furthest_read_counting_from_each_reload() {
    let xml = br#"<?xml version="1.0"?>
<softwarelist name="nes" description="Test">
  <software name="game">
    <description>Game</description>
    <part name="cart" interface="nes_cart">
      <dataarea name="rom" size="8">
        <rom name="game.bin" size="2" crc="11111111" offset="0"/>
        <rom size="2" offset="2" loadflag="continue"/>
        <rom size="2" offset="4" loadflag="reload"/>
        <rom size="2" offset="6" loadflag="continue"/>
        <rom size="2" offset="0" loadflag="fill" value="0xff"/>
        <rom size="2" offset="0" loadflag="continue"/>
      </dataarea>
    </part>
  </software>
</softwarelist>
"#;
    let dat = Dat::parse(xml, Path::new("nes.xml")).unwrap();
    let sizes: Vec<(&str, u64)> = dat.roms.iter().map(|r| (r.name.as_str(), r.size)).collect();
    assert_eq!(sizes, [("game.bin", 4)]);
    assert_eq!(dat.candidates(4, "11111111").collect::<Vec<_>>(), [0]);
}

#[test]
fn xml_that_is_no_dat_is_refused() {
    let err = Dat::parse(b"<html><body/></html>", Path::new("page.xml"))
        .err()
        .expect("not a DAT");
    assert!(err.to_string().contains("not a"), "{err}");
}

#[test]
fn a_logiqx_header_gives_its_format_description_author_homepage_and_url() {
    let xml = br#"<?xml version="1.0"?>
<datafile>
  <header>
    <id>88</id>
    <name>Atari - Atari 2600</name>
    <description>Atari - Atari 2600 (Description)</description>
    <version>20261107-215159</version>
    <author>Alpha, Beta &amp; Gamma</author>
    <homepage>No-Intro</homepage>
    <url>https://www.no-intro.org</url>
    <clrmamepro forcenodump="required"/>
  </header>
  <game name="Game (USA)">
    <description>Game (USA)</description>
    <rom name="Game (USA).a26" size="4" crc="0f0f0f0f"/>
  </game>
</datafile>
"#;
    let dat = Dat::parse(xml, Path::new("a2600.dat")).unwrap();
    assert_eq!(dat.format, DatFormat::Logiqx);
    assert_eq!(dat.name, "Atari - Atari 2600");
    assert_eq!(
        dat.description.as_deref(),
        Some("Atari - Atari 2600 (Description)")
    );
    assert_eq!(dat.version, "20261107-215159");
    assert_eq!(dat.date, "", "the header gives no date");
    assert_eq!(dat.author.as_deref(), Some("Alpha, Beta & Gamma"));
    assert_eq!(dat.homepage.as_deref(), Some("No-Intro"));
    assert_eq!(dat.url.as_deref(), Some("https://www.no-intro.org"));
    assert_eq!(dat.descriptions, ["Game (USA)"]);
    assert_eq!(dat.provider(), Some("No-Intro"));
}

#[test]
fn a_logiqx_header_without_claims_invents_none() {
    let xml = br#"<datafile><header><name>Mine</name></header></datafile>"#;
    let dat = Dat::parse(xml, Path::new("mine.dat")).unwrap();
    assert_eq!(
        (&dat.description, &dat.author, &dat.homepage, &dat.url),
        (&None, &None, &None, &None)
    );
    assert_eq!(dat.provider(), None);
}

#[test]
fn mame_formats_keep_their_native_identity_and_only_the_fields_they_carry() {
    let listxml = br#"<mame build="0.262 (mame0262)"><machine name="pong"/></mame>"#;
    let dat = Dat::parse(listxml, Path::new("mame.xml")).unwrap();
    assert_eq!(
        (dat.format, dat.name.as_str(), dat.version.as_str()),
        (DatFormat::Listxml, "MAME", "0.262 (mame0262)")
    );
    assert_eq!((&dat.description, &dat.author), (&None, &None));
    assert_eq!(dat.provider(), Some("MAME"));

    let list = br#"<softwarelist name="nes" description="Nintendo NES cartridges"><software name="smb"/></softwarelist>"#;
    let dat = Dat::parse(list, Path::new("nes.xml")).unwrap();
    assert_eq!(
        (dat.format, dat.name.as_str(), dat.description.as_deref()),
        (
            DatFormat::SoftwareList,
            "nes",
            Some("Nintendo NES cartridges")
        )
    );
    assert_eq!((&dat.homepage, &dat.url), (&None, &None));
    assert_eq!(dat.provider(), Some("MAME"));
}

#[test]
fn a_dat_with_no_native_name_is_refused_naming_the_file_and_the_missing_name() {
    let logiqx = Dat::parse(
        b"<datafile><header><version>1</version></header></datafile>",
        Path::new("nameless.dat"),
    )
    .unwrap();
    let err = logiqx
        .check_name(Path::new("nameless.dat"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("nameless.dat") && err.contains("<name>"),
        "{err}"
    );

    let list = Dat::parse(b"<softwarelist></softwarelist>", Path::new("list.xml")).unwrap();
    let err = list
        .check_name(Path::new("list.xml"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("list.xml") && err.contains("name"), "{err}");
}

#[test]
fn header_fields_in_cdata_are_read_like_text_without_unescaping() {
    let xml = br#"<datafile><header><name><![CDATA[Atari - 2600]]></name><version>2026<![CDATA[1107]]></version><homepage><![CDATA[No-Intro]]></homepage><author>a &amp; <![CDATA[b &amp; c]]></author></header></datafile>"#;
    let dat = Dat::parse(xml, Path::new("a2600.dat")).unwrap();
    assert_eq!(dat.name, "Atari - 2600");
    assert_eq!(dat.version, "20261107");
    assert_eq!(dat.homepage.as_deref(), Some("No-Intro"));
    assert_eq!(dat.author.as_deref(), Some("a & b &amp; c"));
    assert_eq!(dat.provider(), Some("No-Intro"));
}

#[test]
fn a_listxml_machine_gives_its_year_maker_driver_screens_inputs_software_lists_and_features() {
    use title_select_engine::dat::{Display, Driver, DriverStatus, Feature};
    let dat = family();
    let blaster = &dat.machines[index(&dat, "blaster")];
    assert_eq!(dat.descriptions[index(&dat, "blaster")], "Blaster (World)");
    assert_eq!(blaster.year.as_deref(), Some("1995"));
    assert_eq!(blaster.manufacturer.as_deref(), Some("Acme & Sons"));
    assert_eq!(
        blaster.driver,
        Some(Driver {
            status: Some(DriverStatus::Good),
            emulation: Some(DriverStatus::Good),
        })
    );
    assert_eq!(
        blaster.displays,
        Some(vec![Display {
            kind: Some("raster".into()),
            rotate: Some(0),
        }])
    );
    assert_eq!((blaster.players, blaster.coins), (Some(2), Some(2)));
    assert_eq!(
        blaster.controls,
        Some(vec!["joy".into(), "joy".into(), "dial".into()])
    );
    assert_eq!(blaster.has_software_list, Some(true));
    assert_eq!(
        blaster.features,
        Some(vec![
            Feature {
                kind: "sound".into(),
                status: Some("imperfect".into()),
                overall: None,
            },
            Feature {
                kind: "protection".into(),
                status: None,
                overall: Some("unemulated".into()),
            },
        ])
    );

    let blasterj = &dat.machines[index(&dat, "blasterj")];
    assert_eq!(
        blasterj.driver.as_ref().and_then(|d| d.status),
        Some(DriverStatus::Imperfect)
    );
    assert_eq!(
        blasterj.displays,
        Some(vec![Display {
            kind: Some("vector".into()),
            rotate: Some(90),
        }])
    );
    assert_eq!(blasterj.has_software_list, Some(false));
    assert_eq!(blasterj.features, Some(Vec::new()));

    let crane = &dat.machines[index(&dat, "crane")];
    assert_eq!(crane.year.as_deref(), Some("199?"));
    assert_eq!(crane.manufacturer, None);
    assert_eq!(
        crane.driver.as_ref().and_then(|d| d.emulation),
        Some(DriverStatus::Preliminary)
    );
    assert_eq!(
        crane.displays,
        Some(Vec::new()),
        "a screenless machine lists no display"
    );
    assert_eq!(
        (crane.players, crane.coins, &crane.controls),
        (Some(1), Some(1), &Some(Vec::new())),
        "an input with no control is known to have none"
    );

    let z80 = &dat.machines[index(&dat, "z80")];
    assert_eq!((&z80.driver, &z80.year), (&None, &None));
    assert_eq!(
        (z80.players, z80.coins, &z80.controls),
        (Some(0), Some(0), &Some(Vec::new())),
        "a machine with no input has no players, coins or controls"
    );
}

/// Each ROM of `game` as `(name, merge, status, optional)`.
fn rom_flags<'a>(
    dat: &'a Dat,
    game: &str,
) -> Vec<(
    &'a str,
    Option<&'a str>,
    title_select_engine::dat::DumpStatus,
    bool,
)> {
    let game = index(dat, game);
    dat.roms
        .iter()
        .filter(|r| r.game == game)
        .map(|r| (r.name.as_str(), r.merge.as_deref(), r.status, r.optional))
        .collect()
}

#[test]
fn a_rom_says_what_it_merges_from_whether_it_was_dumped_well_and_whether_it_is_optional() {
    use DumpStatus::{BadDump, Good, NoDump};
    let dat = family();
    assert_eq!(
        rom_flags(&dat, "blaster"),
        [
            ("bios.u1", Some("bios.u1"), Good, false),
            ("prog.u3", None, Good, false),
            ("gfx.u4", None, BadDump, false),
            ("pal.u5", None, NoDump, false),
            ("snd.u6", None, Good, true),
            ("blaster.chd", None, Good, false),
        ]
    );
    let pal = dat.roms.iter().find(|r| r.name == "pal.u5").unwrap();
    assert_eq!(pal.size, 260);
    assert_eq!((&pal.crc, &pal.sha1), (&None, &None));
}

#[test]
fn a_listxml_disk_is_a_rom_named_for_its_chd_and_listed_by_sha1_alone() {
    use DumpStatus::{Good, NoDump};
    let dat = family();
    let disks: Vec<(&str, &str, Option<&str>, Option<&str>)> = dat
        .roms
        .iter()
        .filter(|r| r.disk)
        .map(|r| {
            (
                dat.games[r.game].as_str(),
                r.name.as_str(),
                r.merge.as_deref(),
                r.sha1.as_deref(),
            )
        })
        .collect();
    let hdd = Some("d15c000000000000000000000000000000000001");
    let blaster = Some("d15c000000000000000000000000000000000002");
    assert_eq!(
        disks,
        [
            ("arcbios", "arcbios-hdd.chd", None, hdd),
            ("blaster", "blaster.chd", None, blaster),
            ("blasterj", "blaster.chd", Some("blaster.chd"), blaster),
            ("blasterj", "blasterj-alt.chd", None, None),
            ("crane", "arcbios-hdd.chd", None, hdd),
        ]
    );
    assert_eq!(
        rom_flags(&dat, "blasterj")
            .into_iter()
            .filter(|(name, ..)| name.ends_with(".chd"))
            .collect::<Vec<_>>(),
        [
            ("blaster.chd", Some("blaster.chd"), Good, false),
            ("blasterj-alt.chd", None, NoDump, true),
        ]
    );
    let disk = dat.roms.iter().find(|r| r.disk).unwrap();
    assert_eq!((&disk.crc, &disk.md5, &disk.sha256), (&None, &None, &None));
    assert_eq!(disk.size, 0, "a DAT gives a Disk no size");
    assert_eq!(dat.roms_named("blaster.chd").len(), 2);
    assert_eq!(
        dat.candidates(0, "00000000").count(),
        0,
        "no payload's size and CRC32 can find a Disk"
    );
    assert!(
        dat.roms
            .iter()
            .filter(|r| !r.disk)
            .all(|r| !r.name.ends_with(".chd"))
    );
}

#[test]
fn a_rom_less_device_is_a_game_with_no_roms() {
    let dat = family();
    let z80 = index(&dat, "z80");
    assert_eq!(dat.machines[z80].is_device, Some(true));
    assert!(dat.roms.iter().all(|r| r.game != z80));
}

#[test]
fn a_logiqx_dat_without_machine_fields_gives_machines_that_know_nothing_and_good_roms() {
    use title_select_engine::dat::Machine;
    let xml = br#"<?xml version="1.0"?>
<datafile>
  <header><name>Atari - Atari 2600</name></header>
  <game name="Game (USA)">
    <description>Game (USA)</description>
    <rom name="Game (USA).a26" size="4" crc="0f0f0f0f" sha1="0123456789abcdef0123456789abcdef01234567"/>
  </game>
  <game name="Game (Europe)" cloneof="Game (USA)">
    <description>Game (Europe)</description>
    <rom name="Game (Europe).a26" size="4" crc="1f0f0f0f" status="verified"/>
  </game>
</datafile>
"#;
    let dat = Dat::parse(xml, Path::new("a2600.dat")).unwrap();
    assert_eq!(dat.games, ["Game (USA)", "Game (Europe)"]);
    assert_eq!(dat.cloneof, [None, Some("Game (USA)".into())]);
    assert_eq!(
        dat.machines,
        [Machine::default(), Machine::default()],
        "they know nothing"
    );
    let flags = |game| rom_flags(&dat, game);
    assert_eq!(
        (flags("Game (USA)"), flags("Game (Europe)")),
        (
            vec![("Game (USA).a26", None, DumpStatus::Good, false)],
            vec![("Game (Europe).a26", None, DumpStatus::Good, false)],
        )
    );
    assert!(dat.roms.iter().all(|r| r.size == 4 && !r.disk));
    assert_eq!(dat.candidates(4, "0f0f0f0f").collect::<Vec<_>>(), [0]);
    assert_eq!(carried(&dat), [], "its machines say nothing");
}

#[test]
fn a_listxml_machine_knows_every_flag_list_and_count_even_where_it_leaves_them_to_the_dtd() {
    let xml = br#"<?xml version="1.0"?>
<mame build="0.289">
  <machine name="pong">
    <description>Pong</description>
    <rom name="pong.bin" size="4" crc="0f0f0f0f"/>
  </machine>
</mame>
"#;
    let dat = Dat::parse(xml, Path::new("mame.xml")).unwrap();
    use Signal::*;
    assert_eq!(
        carried(&dat),
        [
            Bios,
            Device,
            Mechanical,
            Runnable,
            DeviceRefs,
            Samples,
            Displays,
            Players,
            Coins,
            Controls,
            SoftwareLists,
            Features,
        ],
        "every signal but the names and driver it gives none of"
    );
    let pong = &dat.machines[0];
    assert_eq!(
        (
            pong.is_bios,
            pong.is_device,
            pong.is_mechanical,
            pong.runnable
        ),
        (Some(false), Some(false), Some(false), Some(true)),
        "the DTD's defaults"
    );
    assert_eq!(pong.displays, Some(Vec::new()), "known to have no screen");
    assert_eq!(pong.has_software_list, Some(false));
    assert_eq!((pong.romof.as_deref(), pong.year.as_deref()), (None, None));
    assert!(Signal::ALL.into_iter().all(|s| family().carries(s)));
}

#[test]
fn a_logiqx_dat_carries_only_the_signals_some_game_of_it_writes() {
    let xml = br#"<?xml version="1.0"?>
<datafile>
  <header><name>FinalBurn Neo - Arcade Games</name></header>
  <game name="neogeo" isbios="yes">
    <description>Neo Geo</description>
    <rom name="sp-s2.sp1" size="4" crc="9036d879"/>
  </game>
  <game name="mslug" romof="neogeo">
    <description>Metal Slug</description>
    <year>1996</year>
    <driver status="good"/>
    <rom name="201-p1.p1" size="8" crc="08d8daa5"/>
  </game>
</datafile>
"#;
    let dat = Dat::parse(xml, Path::new("fbneo.dat")).unwrap();
    assert_eq!(
        carried(&dat),
        [Signal::Romof, Signal::Bios, Signal::Year, Signal::Driver]
    );
    assert_eq!(
        (dat.machines[0].is_bios, dat.machines[1].is_bios),
        (Some(true), None)
    );
    assert_eq!(dat.machines[1].runnable, None);
    assert_eq!(dat.machines[1].displays, None);
}

#[test]
fn a_logiqx_game_without_a_display_is_unknown_even_where_another_game_has_one() {
    use title_select_engine::dat::Display;
    let game = |name: &str, display: &str| {
        format!(
            r#"<game name="{name}"><description>{name}</description>{display}<rom name="{name}.bin" size="4" crc="0f0f0f0f"/></game>"#
        )
    };
    let dat_of = |games: &[String]| {
        let xml = format!(
            r#"<?xml version="1.0"?><datafile><header><name>FinalBurn Neo - Arcade Games</name></header>{}</datafile>"#,
            games.concat()
        );
        Dat::parse(xml.as_bytes(), Path::new("fbneo.dat")).unwrap()
    };
    let a = game("a", "");
    let b = game("b", r#"<display type="raster" rotate="90"/>"#);

    let both = dat_of(&[a.clone(), b]);
    assert!(both.carries(Signal::Displays));
    assert_eq!(
        both.machines[0].displays, None,
        "a says nothing of its screens"
    );
    assert_eq!(
        both.machines[1].displays,
        Some(vec![Display {
            kind: Some("raster".into()),
            rotate: Some(90),
        }])
    );

    let alone = dat_of(&[a]);
    assert!(!alone.carries(Signal::Displays));
    assert_eq!(
        alone.machines[0], both.machines[0],
        "taking b away changes nothing of a"
    );
}

#[test]
fn a_logiqx_input_gives_only_the_counts_and_controls_it_writes() {
    let xml = br#"<?xml version="1.0"?>
<datafile>
  <header><name>FinalBurn Neo - Arcade Games</name></header>
  <game name="duo">
    <description>Duo</description>
    <input players="2"/>
    <rom name="duo.bin" size="4" crc="0f0f0f0f"/>
  </game>
  <game name="paddle">
    <description>Paddle</description>
    <input players="1" coins="1">
      <control type="paddle"/>
    </input>
    <rom name="paddle.bin" size="4" crc="1f0f0f0f"/>
  </game>
</datafile>
"#;
    let dat = Dat::parse(xml, Path::new("fbneo.dat")).unwrap();
    let duo = &dat.machines[0];
    assert_eq!(
        (duo.players, duo.coins, &duo.controls),
        (Some(2), None, &None),
        "no coins or controls are known"
    );
    let paddle = &dat.machines[1];
    assert_eq!(
        (paddle.players, paddle.coins, &paddle.controls),
        (Some(1), Some(1), &Some(vec!["paddle".into()]))
    );
    assert_eq!(
        carried(&dat),
        [Signal::Players, Signal::Coins, Signal::Controls]
    );
}

#[test]
fn a_logiqx_dat_fills_the_machine_fields_it_carries_but_lists_no_disks() {
    let xml = br#"<?xml version="1.0"?>
<datafile>
  <header><name>FinalBurn Neo - Arcade Games</name></header>
  <game name="neogeo" isbios="yes">
    <description>Neo Geo</description>
    <year>1990</year>
    <manufacturer>SNK</manufacturer>
    <rom name="sp-s2.sp1" size="4" crc="9036d879"/>
  </game>
  <game name="mslug" romof="neogeo">
    <description>Metal Slug</description>
    <rom name="sp-s2.sp1" merge="sp-s2.sp1" size="4" crc="9036d879"/>
    <rom name="201-p1.p1" size="8" status="nodump"/>
    <disk name="mslug" sha1="ABCDEF0123456789ABCDEF0123456789ABCDEF01"/>
  </game>
</datafile>
"#;
    let dat = Dat::parse(xml, Path::new("fbneo.dat")).unwrap();
    assert_eq!(dat.machines[0].is_bios, Some(true));
    assert_eq!(dat.machines[0].year.as_deref(), Some("1990"));
    assert_eq!(dat.machines[0].manufacturer.as_deref(), Some("SNK"));
    assert_eq!(dat.machines[1].romof.as_deref(), Some("neogeo"));
    assert_eq!(dat.machines[1].driver, None);
    assert_eq!(dat.roms[1].merge.as_deref(), Some("sp-s2.sp1"));
    assert_eq!(dat.roms[2].status, DumpStatus::NoDump);
    assert_eq!(dat.roms.len(), 3, "disks come only from ListXML");
}

#[test]
fn a_software_list_gives_machines_that_know_nothing_and_no_disks() {
    use title_select_engine::dat::Machine;
    let xml = br#"<?xml version="1.0"?>
<softwarelist name="cdi" description="Philips CD-i discs">
  <software name="game" cloneof="parent">
    <description>Game</description>
    <year>1992</year>
    <publisher>Philips</publisher>
    <part name="cdrom" interface="cdi_cdrom">
      <diskarea name="cdrom">
        <disk name="game" sha1="0123456789abcdef0123456789abcdef01234567"/>
      </diskarea>
    </part>
  </software>
</softwarelist>
"#;
    let dat = Dat::parse(xml, Path::new("cdi.xml")).unwrap();
    assert_eq!(dat.games, ["game"]);
    assert_eq!(dat.cloneof, [Some("parent".into())]);
    assert_eq!(dat.machines, [Machine::default()]);
    assert_eq!(carried(&dat), []);
    assert!(dat.roms.is_empty());
}

#[test]
fn a_dats_format_is_known_by_its_root_element() {
    let format = |text: &str| Dat::format_of(text.as_bytes()).unwrap();
    assert_eq!(
        format(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE mame [\n<!ELEMENT mame (machine+)>\n]>\n<mame build=\"0.289\">"
        ),
        Some(DatFormat::Listxml)
    );
    assert_eq!(
        format("<?xml version=\"1.0\"?><!-- a comment --><datafile><header>"),
        Some(DatFormat::Logiqx)
    );
    assert_eq!(
        format("<softwarelist name=\"nes\">"),
        Some(DatFormat::SoftwareList)
    );
    assert_eq!(format("clrmamepro (\n\tname \"Test\"\n)"), None);
    assert_eq!(format("<?xml version=\"1.0\"?><catalogue>"), None);
    assert_eq!(format(""), None);
    assert_eq!(format("<?xml version=\"1.0\"?><datafile"), None);
}

#[test]
fn a_dats_format_is_found_past_a_prologue_of_any_length() {
    let preamble = format!(
        "<?xml version=\"1.0\"?>\n<!-- {} -->\n",
        "x".repeat(4 << 20)
    );
    for (root, format) in [
        ("<mame build=\"0.289\"></mame>", DatFormat::Listxml),
        (
            "<softwarelist name=\"nes\"></softwarelist>",
            DatFormat::SoftwareList,
        ),
    ] {
        let text = preamble.clone() + root;
        let reader = std::io::BufReader::with_capacity(1 << 16, text.as_bytes());
        assert_eq!(Dat::format_of(reader).unwrap(), Some(format));
        assert_eq!(
            Dat::parse(text.as_bytes(), Path::new("dat.xml"))
                .unwrap()
                .format,
            format
        );
    }
}

/// A ClrMamePro DAT whose header names an XML root in its text, as `<mame>` in its description.
fn clrmamepro_naming(root: &str) -> String {
    format!(
        "clrmamepro (\n\tname \"Test\"\n\tdescription \"A {root} guide\"\n)\n\ngame (\n\tname \"game\"\n)\n"
    )
}

#[test]
fn a_dat_that_opens_with_no_markup_is_no_xml_whatever_its_text_names() {
    for root in ["<mame>", "<softwarelist>", "<datafile>"] {
        let text = clrmamepro_naming(root);
        assert_eq!(Dat::format_of(text.as_bytes()).unwrap(), None, "{root}");
        assert!(
            Dat::parse(text.as_bytes(), Path::new("test.dat")).is_err(),
            "{root}"
        );
    }
    let format = |text: &str| Dat::format_of(text.as_bytes()).unwrap();
    assert_eq!(
        format("\u{feff}\r\n\t <mame build=\"0.289\">"),
        Some(DatFormat::Listxml)
    );
    assert_eq!(
        format("<?pi data?>\n<softwarelist name=\"nes\">"),
        Some(DatFormat::SoftwareList)
    );
    assert_eq!(format("\u{feff}  \n"), None);
    let parsed = Dat::parse(
        "\u{feff}\n<mame build=\"0.289\"></mame>".as_bytes(),
        Path::new("mame.xml"),
    );
    assert_eq!(parsed.unwrap().format, DatFormat::Listxml);
}

/// Counts the bytes read through it.
struct Counting<R> {
    inner: R,
    read: std::rc::Rc<std::cell::Cell<usize>>,
}

impl<R: std::io::Read> std::io::Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n);
        Ok(n)
    }
}

#[test]
fn a_text_dats_format_is_known_from_a_bounded_prefix() {
    let text = clrmamepro_naming("plain") + &"game (\n\tname \"game\"\n)\n".repeat(1 << 20);
    assert!(text.len() > 16 << 20);
    let read = std::rc::Rc::default();
    let reader = std::io::BufReader::with_capacity(
        1 << 16,
        Counting {
            inner: text.as_bytes(),
            read: std::rc::Rc::clone(&read),
        },
    );
    assert_eq!(Dat::format_of(reader).unwrap(), None);
    assert!(read.get() <= 1 << 16, "read {} bytes", read.get());
}
