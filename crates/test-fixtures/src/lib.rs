//! The fixture writers the integration tests share: ZIPs, TorrentZips, Logiqx DATs and MAME ListXMLs, built from
//! synthetic payloads at test time. A dev-dependency only, never shipped.
//!
//! Each writer's output is byte for byte what the copies it replaced wrote, so the tests that read it see the same
//! files.

use std::fs;
use std::io::{Cursor, Seek, Write};
use std::path::{Path, PathBuf};

use sha1::Digest;
use stage_select_engine::torrentzip;

/// A ROM as a DAT lists it: the checksums are computed from `payload` at test time.
pub struct RomSpec {
    pub name: &'static str,
    pub payload: Vec<u8>,
    pub sha256: bool,
    pub crc: bool,
    /// Whether it is listed with its SHA-1.
    pub sha1: bool,
    /// Whether its Game may run without it (`optional="yes"`).
    pub optional: bool,
}

/// A ROM listed with every checksum.
pub fn rom(name: &'static str, payload: &[u8]) -> RomSpec {
    RomSpec {
        name,
        payload: payload.to_vec(),
        sha256: true,
        crc: true,
        sha1: true,
        optional: false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `<rom>` element a DAT lists `r` with: `optional="yes"` when `r.optional` is on, its size, CRC-32 and MD5 unless
/// `r.crc` is off, SHA-1 unless `r.sha1` is off, and SHA-256 when `r.sha256` is on.
pub fn rom_xml(r: &RomSpec) -> String {
    let p = &r.payload;
    let mut checksums = String::new();
    if r.crc {
        checksums += &format!(
            r#" crc="{:08x}" md5="{}""#,
            crc32fast::hash(p),
            hex(&md5::Md5::digest(p))
        );
    }
    if r.sha1 {
        checksums += &format!(r#" sha1="{}""#, hex(&sha1::Sha1::digest(p)));
    }
    if r.sha256 {
        checksums += &format!(r#" sha256="{}""#, hex(&sha2::Sha256::digest(p)));
    }
    let optional = if r.optional { r#" optional="yes""# } else { "" };
    format!(
        r#"<rom name="{}"{optional} size="{}"{checksums}/>"#,
        r.name,
        p.len()
    )
}

/// The `<rom>` element MAME's ListXML lists `payload` with as `name`: its size, CRC-32 and SHA-1, after
/// `merge="…"` when it merges from its parent's ROM of that name.
pub fn mame_rom_xml(name: &str, merge: Option<&str>, payload: &[u8]) -> String {
    let merge = merge
        .map(|m| format!(r#" merge="{m}""#))
        .unwrap_or_default();
    format!(
        r#"<rom name="{name}"{merge} size="{}" crc="{:08x}" sha1="{}"/>"#,
        payload.len(),
        crc32fast::hash(payload),
        hex(&sha1::Sha1::digest(payload))
    )
}

/// A Logiqx DAT's file name and header. The default is `test.dat`, a No-Intro revision `20260101-000000` of
/// `Test - System`.
pub struct Dat<'a> {
    pub file: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    /// `No-Intro` and `Redump` are named in the header the way their DATs name them; any other provider leaves the
    /// header naming none.
    pub provider: &'a str,
    /// The Header Rule the header names (`<clrmamepro header="…"/>`), as No-Intro's headerless DATs do.
    pub header_rule: Option<&'a str>,
}

impl Default for Dat<'_> {
    fn default() -> Self {
        Dat {
            file: "test.dat",
            name: "Test - System",
            version: "20260101-000000",
            provider: "No-Intro",
            header_rule: None,
        }
    }
}

/// Writes `dat` to `dir/<dat.file>` with `games`, each a name, the parent it is a clone of (`cloneof`) and its ROMs,
/// and returns its path.
pub fn write_logiqx(dir: &Path, dat: &Dat, games: &[(&str, Option<&str>, &[RomSpec])]) -> PathBuf {
    let Dat {
        file,
        name,
        version,
        provider,
        header_rule,
    } = dat;
    let clrmamepro = header_rule
        .map(|r| format!("    <clrmamepro header=\"{r}\"/>\n"))
        .unwrap_or_default();
    let homepage = match *provider {
        "No-Intro" => "    <homepage>No-Intro</homepage>\n",
        "Redump" => "    <homepage>redump.org</homepage>\n",
        _ => "",
    };
    let mut xml = format!(
        r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>{name}</name>
    <version>{version}</version>
{homepage}{clrmamepro}  </header>
"#
    );
    for (game, parent, roms) in games {
        let cloneof = parent
            .map(|p| format!(" cloneof=\"{p}\""))
            .unwrap_or_default();
        xml.push_str(&format!("  <game name=\"{game}\"{cloneof}>\n"));
        for r in *roms {
            xml.push_str(&format!("    {}\n", rom_xml(r)));
        }
        xml.push_str("  </game>\n");
    }
    xml.push_str("</datafile>\n");
    let path = dir.join(file);
    fs::write(&path, xml).unwrap();
    path
}

/// A ListXML machine: what its tag says beside its name, as written (such as `cloneof="p" romof="p"` or
/// `isdevice="yes"`), its ROMs, and the devices it names (`<device_ref>`).
pub struct MachineSpec {
    pub name: &'static str,
    pub attrs: &'static str,
    pub roms: Vec<RomSpec>,
    pub devices: &'static [&'static str],
}

/// Writes a MAME ListXML of `machines` to `dir/mame.xml` and returns its path.
pub fn write_listxml(dir: &Path, machines: &[MachineSpec]) -> PathBuf {
    let machines: Vec<_> = machines
        .iter()
        .map(|m| {
            let roms = m.roms.iter().map(|r| format!("    {}\n", rom_xml(r)));
            let devices = m
                .devices
                .iter()
                .map(|d| format!("    <device_ref name=\"{d}\"/>\n"));
            (m.name, m.attrs, roms.chain(devices).collect::<String>())
        })
        .collect();
    let path = dir.join("mame.xml");
    fs::write(&path, mame(&machines)).unwrap();
    path
}

/// A MAME ListXML of `machines`, each a name, what its tag says beside it, and a body written as given on the lines
/// after its description.
pub fn listxml(machines: &[(&str, &str, impl AsRef<str>)]) -> String {
    let machines: Vec<_> = machines
        .iter()
        .map(|(name, attrs, body)| (*name, *attrs, format!("{}\n", body.as_ref())))
        .collect();
    mame(&machines)
}

/// The ListXML of machines each with the lines of its body, each line ending in a newline.
fn mame(machines: &[(&str, &str, String)]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\"?>\n<mame build=\"0.289 (mame0289)\">\n");
    for (name, attrs, lines) in machines {
        xml += &format!(
            "  <machine name=\"{name}\" {attrs}>\n    <description>{name}</description>\n{lines}  </machine>\n"
        );
    }
    xml += "</mame>\n";
    xml
}

/// A ZIP of `members`, deflated, in memory.
pub fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    zip_into(
        Cursor::new(Vec::new()),
        members,
        zip::CompressionMethod::Deflated,
    )
    .into_inner()
}

/// Writes the ZIP `path` of `members`, deflated, making its folder.
pub fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    zip_into(create(path), members, zip::CompressionMethod::Deflated);
}

/// As [`write_zip`], with members stored uncompressed, so a test can find and alter their bytes in the archive.
pub fn write_stored_zip(path: &Path, members: &[(&str, &[u8])]) {
    zip_into(create(path), members, zip::CompressionMethod::Stored);
}

/// Writes the TorrentZip `path` of `members`, in TorrentZip order whatever their order here, making its folder.
pub fn write_torrentzip(path: &Path, members: &[(&str, &[u8])]) {
    let mut members = members.to_vec();
    members.sort_by(|a, b| torrentzip::compare_names(a.0, b.0));
    let mut writer = torrentzip::Writer::new(create(path)).unwrap();
    for (name, payload) in members {
        writer
            .add(name, payload.len() as u64, &mut &payload[..])
            .unwrap();
    }
    writer.finish().unwrap();
}

/// Writes `members` to `path` with TorrentZip's structure, but deflated at zlib's `level` rather than its 9: not the
/// bytes the TorrentZip of those members is.
pub fn write_torrentzip_deflated_at(path: &Path, members: &[(&str, &[u8])], level: u32) {
    let mut members = members.to_vec();
    members.sort_by(|a, b| torrentzip::compare_names(a.0, b.0));
    let mut writer = torrentzip::Writer::deflating_at(create(path), level).unwrap();
    for (name, payload) in members {
        writer
            .add(name, payload.len() as u64, &mut &payload[..])
            .unwrap();
    }
    writer.finish().unwrap();
}

fn create(path: &Path) -> fs::File {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::File::create(path).unwrap()
}

fn zip_into<W: Write + Seek>(
    out: W,
    members: &[(&str, &[u8])],
    method: zip::CompressionMethod,
) -> W {
    let mut zip = zip::ZipWriter::new(out);
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    for (name, payload) in members {
        zip.start_file(*name, options).unwrap();
        zip.write_all(payload).unwrap();
    }
    zip.finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_torrentzip_is_written_in_torrentzip_order_whatever_the_order_its_members_are_given_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.zip");

        write_torrentzip(&path, &[("b.bin", b"b"), ("A.bin", b"a")]);

        let members = torrentzip::validate(&mut fs::File::open(&path).unwrap()).unwrap();
        let names: Vec<_> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["A.bin", "b.bin"]);
    }
}
