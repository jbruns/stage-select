#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

pub use stage_select_test_fixtures::*;

pub fn rom_without_sha256(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        sha256: false,
        ..rom(name, payload)
    }
}

/// A ROM listed with SHA-1 only, as some DATs do for ROMs whose CRC is unknown.
pub fn rom_without_crc(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        sha256: false,
        crc: false,
        ..rom(name, payload)
    }
}

/// A ROM listed with its size, CRC-32 and MD5, but no SHA-1 or SHA-256.
pub fn rom_without_sha(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        sha256: false,
        sha1: false,
        ..rom(name, payload)
    }
}

/// A ROM listed with its size alone.
pub fn rom_size_only(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        sha256: false,
        sha1: false,
        crc: false,
        ..rom(name, payload)
    }
}

/// A ROM its Game may run without (`optional="yes"`).
pub fn optional_rom(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        optional: true,
        ..rom(name, payload)
    }
}

/// Writes a No-Intro style Logiqx DAT with the given Games and returns its path.
pub fn write_dat(dir: &Path, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
    write_dat_as(dir, &Dat::default(), games)
}

/// Writes a No-Intro revision of the `Test - System` DAT, with its header `version`, to `dir/file`.
pub fn write_dat_version(
    dir: &Path,
    file: &str,
    version: &str,
    games: &[(&str, Vec<RomSpec>)],
) -> PathBuf {
    write_dat_from(dir, file, version, "No-Intro", games)
}

/// As [`write_dat_version`], with a header that names `provider` the way its DATs do: `No-Intro` and `Redump` are
/// recognised, and any other provider leaves the header naming none.
pub fn write_dat_from(
    dir: &Path,
    file: &str,
    version: &str,
    provider: &str,
    games: &[(&str, Vec<RomSpec>)],
) -> PathBuf {
    let dat = Dat {
        file,
        version,
        provider,
        ..Dat::default()
    };
    write_dat_as(dir, &dat, games)
}

/// Writes a No-Intro DAT whose header names the Header Rule `rule` (`<clrmamepro header="…"/>`), as No-Intro's
/// headerless DATs do.
pub fn write_dat_with_header(dir: &Path, rule: &str, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
    write_dat_with_header_from(dir, rule, "No-Intro", games)
}

/// As [`write_dat_with_header`], from `provider` as [`write_dat_from`] names it.
pub fn write_dat_with_header_from(
    dir: &Path,
    rule: &str,
    provider: &str,
    games: &[(&str, Vec<RomSpec>)],
) -> PathBuf {
    let dat = Dat {
        provider,
        header_rule: Some(rule),
        ..Dat::default()
    };
    write_dat_as(dir, &dat, games)
}

/// Writes a DAT whose Games may each name a parent (`cloneof`), as MAME and No-Intro's parent/clone DATs do.
pub fn write_dat_with_clones(dir: &Path, games: &[(&str, Option<&str>, Vec<RomSpec>)]) -> PathBuf {
    let games: Vec<_> = games
        .iter()
        .map(|(game, parent, roms)| (*game, *parent, roms.as_slice()))
        .collect();
    write_logiqx(dir, &Dat::default(), &games)
}

/// Writes `dat` with `games`, none a clone, and returns its path.
pub fn write_dat_as(dir: &Path, dat: &Dat, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
    let games: Vec<_> = games
        .iter()
        .map(|(game, roms)| (*game, None, roms.as_slice()))
        .collect();
    write_logiqx(dir, dat, &games)
}

pub fn write_file(path: &Path, payload: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, payload).unwrap();
}

/// Two temp dirs: one for the DAT, one for the Source Set.
pub struct Fixture {
    pub dat_dir: tempfile::TempDir,
    pub set: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            dat_dir: tempfile::tempdir().unwrap(),
            set: tempfile::tempdir().unwrap(),
        }
    }

    pub fn dat(&self, games: &[(&str, Vec<RomSpec>)]) -> PathBuf {
        write_dat(self.dat_dir.path(), games)
    }

    pub fn dat_with_clones(&self, games: &[(&str, Option<&str>, Vec<RomSpec>)]) -> PathBuf {
        write_dat_with_clones(self.dat_dir.path(), games)
    }

    pub fn set_path(&self, rel: &str) -> PathBuf {
        self.set.path().join(rel)
    }
}

/// `len` bytes of noise from `seed`, the same for the same seed: a synthetic disc track.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A synthetic Redump-style disc, `<game>.cue` and `tracks` tracks `<game> (Track N).bin` of noise, the first
/// `track_len` bytes long; the cue sheet names each track, as a real one does.
pub fn disc(game: &str, tracks: usize, track_len: usize, seed: u64) -> Vec<RomSpec> {
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let mut cue = String::new();
    let mut roms = Vec::new();
    for n in 1..=tracks {
        let name = if tracks == 1 {
            format!("{game}.bin")
        } else {
            format!("{game} (Track {n}).bin")
        };
        let kind = if n == 1 { "MODE2/2352" } else { "AUDIO" };
        cue += &format!("FILE \"{name}\" BINARY\n  TRACK {n:02} {kind}\n    INDEX 01 00:00:00\n");
        let len = if n == 1 { track_len } else { 2352 * 4 + n };
        roms.push(rom(leak(name), &noise(seed + n as u64, len)));
    }
    roms.insert(0, rom(leak(format!("{game}.cue")), cue.as_bytes()));
    roms
}

/// The ROMs of a synthetic disc game, each written loose into `<dir>/<game>/`: a loose folder.
pub fn write_loose_folder(dir: &Path, game: &str, roms: &[RomSpec]) {
    for r in roms {
        write_file(&dir.join(game).join(r.name), &r.payload);
    }
}
