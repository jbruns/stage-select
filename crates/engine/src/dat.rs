//! Reading a DAT into a DAT Revision: Logiqx XML, MAME's ListXML (`<mame>`, named `MAME` and versioned by its build),
//! or a MAME software list (`<softwarelist>`, named by its list name, unversioned). A Logiqx or ListXML Game also
//! gives the machine model the arcade selector and family counting read (ADRs 0025 to 0027), and its Disks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use sha2::Digest;
use title_select_api::HeaderRule;
use title_select_api::dat::{DatFormat, HeaderClaims, recognise_provider};

use crate::Error;

pub struct Rom {
    pub game: usize,
    pub name: String,
    /// Its size in bytes; 0 for a Disk, which a DAT gives no size.
    pub size: u64,
    pub crc: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    /// The name of the ROM it shares with the Game it is `romof` (`merge=`), as that Game lists it.
    pub merge: Option<String>,
    pub status: DumpStatus,
    /// Whether the Game may run without it (`optional="yes"`).
    pub optional: bool,
    /// Whether it is a Disk (`<disk>`): a CHD named `<disk>.chd`, listed by SHA-1 alone and with no size, which only
    /// its name finds.
    pub disk: bool,
}

impl Rom {
    /// Its name as the DAT lists it: a Disk's less `.chd`.
    pub fn listed_name(&self) -> &str {
        match self.disk {
            true => self.name.strip_suffix(".chd").unwrap_or(&self.name),
            false => &self.name,
        }
    }
}

/// Whether a ROM's dump is known good, as a DAT's `status` says; good when it says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpStatus {
    /// `good`, or Logiqx's `verified`.
    Good,
    /// `baddump`: its checksums are of a dump known to be bad.
    BadDump,
    /// `nodump`: no good dump is known, and its checksums are left out.
    NoDump,
}

impl DumpStatus {
    fn parse(text: Option<&str>) -> DumpStatus {
        match text {
            Some("baddump") => DumpStatus::BadDump,
            Some("nodump") => DumpStatus::NoDump,
            _ => DumpStatus::Good,
        }
    }
}

/// What a Game's entry says of the machine it is (ADR 0027). Every field is optional: a ListXML machine fills them
/// all, a Logiqx game only what it carries, and a software item none. A flag the entry leaves out takes its DTD's
/// default, and an element it leaves out is none or empty.
///
/// A field says something only where its DAT carries its [`Signal`] ([`Dat::carries`]): otherwise it is its default
/// and means "not known", not "no". Whether a DAT carries a signal is the DAT's to say, not one machine's: MAME writes
/// `<display>` for every machine with a screen, so in ListXML no displays means screenless, while a Logiqx DAT that
/// writes `<display>` for no game says nothing of any screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// The Game whose ROMs it shares (`romof`): its parent, or its BIOS for a parent.
    pub romof: Option<String>,
    pub is_bios: bool,
    pub is_device: bool,
    pub is_mechanical: bool,
    pub runnable: bool,
    /// The Game whose samples it uses (`sampleof`).
    pub sample_of: Option<String>,
    /// The driver source it is defined in (`sourcefile`), such as `atari/pong.cpp`.
    pub source_file: Option<String>,
    /// The devices it names (`<device_ref>`), in DAT order.
    pub device_refs: Vec<String>,
    /// The samples it plays (`<sample>`), in DAT order.
    pub samples: Vec<String>,
    pub year: Option<String>,
    pub manufacturer: Option<String>,
    pub driver: Option<Driver>,
    /// Its screens (`<display>`), in DAT order; none for a screenless machine.
    pub displays: Vec<Display>,
    pub input: Option<Input>,
    /// Whether it names a software list (`<softwarelist>`).
    pub has_software_list: bool,
    /// The parts it emulates imperfectly or not at all (`<feature>`), in DAT order.
    pub features: Vec<Feature>,
}

/// What a DAT may say of its machines, one per field of [`Machine`]. A ListXML DAT carries every one, as its DTD
/// gives each attribute a default and MAME writes each element wherever it applies; a Logiqx DAT carries those it
/// writes for at least one Game, its DTD giving the flags the same defaults; a software list carries none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Romof,
    Bios,
    Device,
    Mechanical,
    Runnable,
    SampleOf,
    SourceFile,
    DeviceRefs,
    Samples,
    Year,
    Manufacturer,
    Driver,
    Displays,
    Input,
    SoftwareLists,
    Features,
}

impl Signal {
    pub const ALL: [Signal; 16] = [
        Signal::Romof,
        Signal::Bios,
        Signal::Device,
        Signal::Mechanical,
        Signal::Runnable,
        Signal::SampleOf,
        Signal::SourceFile,
        Signal::DeviceRefs,
        Signal::Samples,
        Signal::Year,
        Signal::Manufacturer,
        Signal::Driver,
        Signal::Displays,
        Signal::Input,
        Signal::SoftwareLists,
        Signal::Features,
    ];

    /// The signal of a `<game>` or `<machine>` attribute.
    fn of_attribute(key: &str) -> Option<Signal> {
        Some(match key {
            "romof" => Signal::Romof,
            "isbios" => Signal::Bios,
            "isdevice" => Signal::Device,
            "ismechanical" => Signal::Mechanical,
            "runnable" => Signal::Runnable,
            "sampleof" => Signal::SampleOf,
            "sourcefile" => Signal::SourceFile,
            _ => return None,
        })
    }
}

/// How well a driver works, as MAME rates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DriverStatus {
    Preliminary,
    Imperfect,
    Good,
}

impl DriverStatus {
    fn parse(text: &str) -> Option<DriverStatus> {
        match text {
            "good" => Some(DriverStatus::Good),
            "imperfect" => Some(DriverStatus::Imperfect),
            "preliminary" => Some(DriverStatus::Preliminary),
            _ => None,
        }
    }
}

/// A machine's `<driver>`: its overall `status` and its `emulation`'s; none for a value MAME doesn't write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Driver {
    pub status: Option<DriverStatus>,
    pub emulation: Option<DriverStatus>,
}

/// A machine's screen: its `type`, such as `raster`, `vector`, `lcd` or `svg`, and its rotation in degrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub kind: Option<String>,
    pub rotate: Option<u16>,
}

/// A machine's `<input>`: how many players and coin slots, and each `<control>`'s type, such as `joy` or `dial`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    pub players: Option<u32>,
    pub coins: Option<u32>,
    pub controls: Vec<String>,
}

/// A part a machine emulates imperfectly or not at all: its `type`, such as `sound` or `protection`, and the
/// `status` or `overall` MAME gives it, such as `imperfect` or `unemulated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature {
    pub kind: String,
    pub status: Option<String>,
    pub overall: Option<String>,
}

impl Default for Machine {
    fn default() -> Self {
        Machine {
            romof: None,
            is_bios: false,
            is_device: false,
            is_mechanical: false,
            runnable: true,
            sample_of: None,
            source_file: None,
            device_refs: Vec::new(),
            samples: Vec::new(),
            year: None,
            manufacturer: None,
            driver: None,
            displays: Vec::new(),
            input: None,
            has_software_list: false,
            features: Vec::new(),
        }
    }
}

pub struct Dat {
    pub sha256: String,
    pub format: DatFormat,
    /// Its native name: the Logiqx header's, a software list's, or `MAME` for ListXML; empty if it gives none.
    pub name: String,
    /// The Logiqx header's or software list's description; none if it gives none.
    pub description: Option<String>,
    /// The Logiqx header's author, homepage and URL, as written; none if it gives none.
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub url: Option<String>,
    pub version: String,
    /// The header's date, as the provider writes it; empty if it gives none.
    pub date: String,
    /// The Header Rule the header names (`<clrmamepro header="…"/>`), as it names it; none if it names none.
    pub header_rule: Option<String>,
    pub games: Vec<String>,
    /// Each Game's description, by the Game's index; empty if the DAT gives none.
    pub descriptions: Vec<String>,
    /// The parent each Game is a clone of (`cloneof`), by the Game's index; none for a parent or a DAT without them.
    pub cloneof: Vec<Option<String>>,
    /// What each Game's entry says of its machine, by the Game's index.
    pub machines: Vec<Machine>,
    /// The signals its machines carry.
    signals: HashSet<Signal>,
    pub roms: Vec<Rom>,
    by_name: HashMap<String, Vec<usize>>,
    by_size_crc: HashMap<(u64, String), Vec<usize>>,
    /// ROMs the DAT lists without a CRC, which the size-and-CRC index cannot find.
    by_size_without_crc: HashMap<u64, Vec<usize>>,
    /// Where a software list's last ROM is read to, and the furthest any read of it reached, while unnamed ROMs that
    /// follow may still read on in its file.
    extent: Option<(u64, u64)>,
}

/// A size as DATs write it: decimal, or hexadecimal after `0x`.
fn parse_size(text: &str) -> Option<u64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn attrs(e: &BytesStart) -> Result<HashMap<String, String>, Error> {
    let mut out = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|err| Error::Dat(err.to_string()))?;
        let key = a.key.as_ref().to_string();
        let value = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| Error::Dat(err.to_string()))?;
        out.insert(key, value.into_owned());
    }
    Ok(out)
}

/// A number attribute; none if it is absent or no number.
fn number<T: std::str::FromStr>(a: &HashMap<String, String>, key: &str) -> Option<T> {
    a.get(key).and_then(|v| v.parse().ok())
}

/// A `yes`/`no` attribute; none if it is absent or neither.
fn yes(a: &HashMap<String, String>, key: &str) -> Option<bool> {
    match a.get(key).map(String::as_str) {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    }
}

impl Dat {
    pub fn read(path: &Path) -> Result<Dat, Error> {
        let bytes =
            std::fs::read(path).map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
        Dat::parse(&bytes, path)
    }

    /// Parses the bytes of the DAT file at `path`; the path only labels errors.
    pub fn parse(bytes: &[u8], path: &Path) -> Result<Dat, Error> {
        let sha256 = crate::hex(&sha2::Sha256::digest(bytes));
        let mut reader = quick_xml::Reader::from_reader(bytes);
        let mut buf = Vec::new();
        let mut dat = Dat {
            sha256,
            format: DatFormat::Logiqx,
            name: String::new(),
            description: None,
            author: None,
            homepage: None,
            url: None,
            version: String::new(),
            date: String::new(),
            header_rule: None,
            games: Vec::new(),
            descriptions: Vec::new(),
            cloneof: Vec::new(),
            machines: Vec::new(),
            signals: HashSet::new(),
            roms: Vec::new(),
            by_name: HashMap::new(),
            by_size_crc: HashMap::new(),
            by_size_without_crc: HashMap::new(),
            extent: None,
        };
        let mut stack: Vec<String> = Vec::new();
        let mut root = None;
        loop {
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
            match event {
                Event::Start(e) if stack.is_empty() => {
                    let tag = e.local_name().as_ref().to_string();
                    let a = attrs(&e)?;
                    root = match tag.as_str() {
                        "datafile" => Some(DatFormat::Logiqx),
                        "mame" => {
                            dat.name = "MAME".into();
                            dat.version = a.get("build").cloned().unwrap_or_default();
                            dat.signals.extend(Signal::ALL);
                            Some(DatFormat::Listxml)
                        }
                        "softwarelist" => {
                            dat.name = a.get("name").cloned().unwrap_or_default();
                            dat.description = a.get("description").cloned();
                            Some(DatFormat::SoftwareList)
                        }
                        _ => None,
                    };
                    stack.push(tag);
                }
                Event::Start(e) => {
                    let tag = dat.open(&e, root)?;
                    stack.push(tag);
                }
                Event::Empty(e) => {
                    dat.open(&e, root)?;
                }
                Event::Text(_) | Event::GeneralRef(_) | Event::CData(_) => {
                    let text = match &event {
                        Event::Text(t) => t.xml10_content().into_owned(),
                        Event::CData(c) => AsRef::<str>::as_ref(c).to_owned(),
                        Event::GeneralRef(r) => match r.resolve_char_ref() {
                            Ok(Some(c)) => c.to_string(),
                            _ => quick_xml::escape::resolve_predefined_entity(&r.xml10_content())
                                .unwrap_or_default()
                                .to_string(),
                        },
                        _ => unreachable!("matched above"),
                    };
                    let in_header = root == Some(DatFormat::Logiqx)
                        && stack.len() >= 2
                        && stack[stack.len() - 2] == "header";
                    let parent = stack.len().checked_sub(2).map(|i| stack[i].as_str());
                    let in_game = matches!(parent, Some("game" | "machine" | "software"));
                    let in_machine = root != Some(DatFormat::SoftwareList)
                        && matches!(parent, Some("game" | "machine"));
                    let machine = dat.machines.last_mut().filter(|_| in_machine);
                    match (stack.last().map(String::as_str), machine) {
                        (Some("year"), Some(machine)) => {
                            dat.signals.insert(Signal::Year);
                            machine.year.get_or_insert_default().push_str(&text)
                        }
                        (Some("manufacturer"), Some(machine)) => {
                            dat.signals.insert(Signal::Manufacturer);
                            machine.manufacturer.get_or_insert_default().push_str(&text)
                        }
                        _ => {}
                    }
                    match stack.last().map(String::as_str) {
                        Some("description") if in_game => {
                            if let Some(description) = dat.descriptions.last_mut() {
                                description.push_str(&text);
                            }
                        }
                        Some("name") if in_header => dat.name.push_str(&text),
                        Some("version") if in_header => dat.version.push_str(&text),
                        Some("date") if in_header => dat.date.push_str(&text),
                        Some("description") if in_header => {
                            dat.description.get_or_insert_default().push_str(&text)
                        }
                        Some("author") if in_header => {
                            dat.author.get_or_insert_default().push_str(&text)
                        }
                        Some("homepage") if in_header => {
                            dat.homepage.get_or_insert_default().push_str(&text)
                        }
                        Some("url") if in_header => dat.url.get_or_insert_default().push_str(&text),
                        _ => {}
                    }
                }
                Event::End(_) => {
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        let Some(format) = root else {
            return Err(Error::Dat(format!(
                "{}: not a Logiqx XML DAT, MAME ListXML or MAME software list",
                path.display()
            )));
        };
        dat.format = format;
        for claim in [
            &mut dat.description,
            &mut dat.author,
            &mut dat.homepage,
            &mut dat.url,
        ] {
            if claim.as_ref().is_some_and(|c| c.trim().is_empty()) {
                *claim = None;
            }
        }
        Ok(dat)
    }

    /// The DAT Provider its format and header signal, or `None` when unknown.
    pub fn provider(&self) -> Option<&'static str> {
        recognise_provider(
            self.format,
            HeaderClaims {
                homepage: self.homepage.as_deref(),
                url: self.url.as_deref(),
                author: self.author.as_deref(),
            },
        )
    }

    /// Whether its machines carry `signal`: if not, each machine's field for it is its default and says nothing, and a
    /// filter on it is unavailable (ADR 0027).
    pub fn carries(&self, signal: Signal) -> bool {
        self.signals.contains(&signal)
    }

    /// Refuses a DAT whose format gives it no native name, naming the file at `path` and the missing field.
    pub fn check_name(&self, path: &Path) -> Result<(), Error> {
        if !self.name.trim().is_empty() {
            return Ok(());
        }
        let missing = match self.format {
            DatFormat::Logiqx => "its header has no <name>",
            DatFormat::Listxml => "it has no name",
            DatFormat::SoftwareList => "its <softwarelist> has no name attribute",
        };
        Err(Error::Dat(format!(
            "{}: the DAT has no name to know it by: {missing}",
            path.display()
        )))
    }

    /// The date in the Logiqx header at the start of `bytes`, read no further than the header's end: empty if there is
    /// none, or the header is not whole in `bytes`.
    pub fn header_date(bytes: &[u8]) -> String {
        let mut reader = quick_xml::Reader::from_reader(bytes);
        let mut buf = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        let mut date = String::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let tag = e.local_name().as_ref().to_string();
                    if stack.is_empty() && tag != "datafile" {
                        return String::new();
                    }
                    stack.push(tag);
                }
                Ok(Event::Text(t))
                    if stack.len() == 3 && stack[1] == "header" && stack[2] == "date" =>
                {
                    date.push_str(&t.xml10_content());
                }
                Ok(Event::End(_)) => {
                    if stack.pop().is_some_and(|t| t == "header") {
                        return date;
                    }
                }
                Ok(Event::Eof) | Err(_) => return String::new(),
                _ => {}
            }
            buf.clear();
        }
    }

    /// Handles an opening or empty tag inside the root and returns its name.
    fn open(&mut self, e: &BytesStart, root: Option<DatFormat>) -> Result<String, Error> {
        let tag = e.local_name().as_ref().to_string();
        match (root, tag.as_str()) {
            (Some(DatFormat::Logiqx | DatFormat::Listxml), "game" | "machine") => {
                self.start_game(e)?;
                let a = attrs(e)?;
                self.signals
                    .extend(a.keys().filter_map(|key| Signal::of_attribute(key)));
                let flag = |key: &str, default: bool| yes(&a, key).unwrap_or(default);
                self.machines.push(Machine {
                    romof: a.get("romof").cloned(),
                    is_bios: flag("isbios", false),
                    is_device: flag("isdevice", false),
                    is_mechanical: flag("ismechanical", false),
                    runnable: flag("runnable", true),
                    sample_of: a.get("sampleof").cloned(),
                    source_file: a.get("sourcefile").cloned(),
                    ..Machine::default()
                });
            }
            (Some(DatFormat::SoftwareList), "software") => {
                self.start_game(e)?;
                self.machines.push(Machine::default());
            }
            // A software list's unnamed ROM has no file of its own: it reads on in the file of the named ROM before
            // it. As MAME's rom_file_size measures that file, one that continues or skips reads on from where the
            // last read ended, and one that reloads reads again from the start; the file is as large as the
            // furthest any read reaches. Any other, such as a fill, reads no file, and ends the run.
            (Some(DatFormat::SoftwareList), "rom") if !attrs(e)?.contains_key("name") => {
                let a = attrs(e)?;
                let read = |pos: u64| -> Result<u64, Error> {
                    a.get("size")
                        .and_then(|s| parse_size(s))
                        .map(|size| pos + size)
                        .ok_or_else(|| {
                            Error::Dat("a rom read in its file has no valid size".into())
                        })
                };
                self.extent = match (self.extent, a.get("loadflag").map(String::as_str)) {
                    (Some((pos, max)), Some("continue" | "ignore")) => Some((read(pos)?, max)),
                    (Some((_, max)), Some("reload" | "reload_plain")) => Some((read(0)?, max)),
                    _ => None,
                };
                if let Some((pos, max)) = self.extent
                    && pos > max
                {
                    self.extent = Some((pos, pos));
                    self.resize_last_rom(pos);
                }
            }
            (Some(_), "rom") => self.add_rom(e)?,
            // Disks come only from ListXML machines (ADR 0026's amendment).
            (Some(DatFormat::Listxml), "disk") => self.add_disk(e)?,
            (
                Some(DatFormat::Logiqx | DatFormat::Listxml),
                "device_ref" | "sample" | "driver" | "display" | "input" | "control"
                | "softwarelist" | "feature",
            ) => {
                let Some(machine) = self.machines.last_mut() else {
                    return Ok(tag);
                };
                self.signals.insert(match tag.as_str() {
                    "device_ref" => Signal::DeviceRefs,
                    "sample" => Signal::Samples,
                    "driver" => Signal::Driver,
                    "display" => Signal::Displays,
                    "input" | "control" => Signal::Input,
                    "softwarelist" => Signal::SoftwareLists,
                    "feature" => Signal::Features,
                    _ => unreachable!("matched above"),
                });
                let mut a = attrs(e)?;
                match tag.as_str() {
                    "device_ref" => machine.device_refs.extend(a.remove("name")),
                    "sample" => machine.samples.extend(a.remove("name")),
                    "driver" => {
                        let status = |key: &str| a.get(key).and_then(|v| DriverStatus::parse(v));
                        machine.driver = Some(Driver {
                            status: status("status"),
                            emulation: status("emulation"),
                        });
                    }
                    "display" => machine.displays.push(Display {
                        kind: a.remove("type"),
                        rotate: number(&a, "rotate"),
                    }),
                    "input" => {
                        machine.input = Some(Input {
                            players: number(&a, "players"),
                            coins: number(&a, "coins"),
                            controls: Vec::new(),
                        })
                    }
                    "control" => machine
                        .input
                        .get_or_insert_default()
                        .controls
                        .extend(a.remove("type")),
                    "softwarelist" => machine.has_software_list = true,
                    "feature" => {
                        if let Some(kind) = a.remove("type") {
                            machine.features.push(Feature {
                                kind,
                                status: a.remove("status"),
                                overall: a.remove("overall"),
                            });
                        }
                    }
                    _ => unreachable!("matched above"),
                }
            }
            (Some(DatFormat::Logiqx), "clrmamepro") if self.games.is_empty() => {
                if let Some(rule) = attrs(e)?.remove("header") {
                    self.header_rule = Some(rule);
                }
            }
            _ => {}
        }
        Ok(tag)
    }

    fn start_game(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a game has no name".into()))?;
        self.games.push(name.clone());
        self.descriptions.push(String::new());
        self.cloneof.push(a.get("cloneof").cloned());
        self.extent = None;
        Ok(())
    }

    fn add_rom(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a rom has no name".into()))?
            .clone();
        let size = a
            .get("size")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Dat(format!("rom {name} has no valid size")))?;
        let lower = |k: &str| a.get(k).map(|v| v.to_ascii_lowercase());
        let rom = self.listed(&a, "rom", name, a.get("merge").cloned())?;
        let rom = Rom {
            size,
            crc: lower("crc"),
            md5: lower("md5"),
            sha1: lower("sha1"),
            sha256: lower("sha256"),
            ..rom
        };
        let idx = self.roms.len();
        self.extent = Some((rom.size, rom.size));
        match &rom.crc {
            Some(crc) => self
                .by_size_crc
                .entry((rom.size, crc.clone()))
                .or_default()
                .push(idx),
            None => self
                .by_size_without_crc
                .entry(rom.size)
                .or_default()
                .push(idx),
        }
        self.push_rom(rom);
        Ok(())
    }

    /// Adds a `<disk>` as a Disk of the last Game: a ROM named for its CHD, which a DAT gives no size. Only its name
    /// finds it here; matching it by its SHA-1 is #228's.
    fn add_disk(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let chd = |name: &String| format!("{name}.chd");
        let name = chd(a
            .get("name")
            .ok_or_else(|| Error::Dat("a disk has no name".into()))?);
        let rom = self.listed(&a, "disk", name, a.get("merge").map(chd))?;
        self.extent = None;
        self.push_rom(Rom {
            sha1: a.get("sha1").map(|v| v.to_ascii_lowercase()),
            disk: true,
            ..rom
        });
        Ok(())
    }

    /// A ROM of the last Game named `name`, with what its `element`'s attributes `a` say of it beside its size and
    /// checksums, which it leaves out.
    fn listed(
        &self,
        a: &HashMap<String, String>,
        element: &str,
        name: String,
        merge: Option<String>,
    ) -> Result<Rom, Error> {
        let game = self
            .games
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::Dat(format!("a {element} is outside any game")))?;
        Ok(Rom {
            game,
            name,
            size: 0,
            crc: None,
            md5: None,
            sha1: None,
            sha256: None,
            merge,
            status: DumpStatus::parse(a.get("status").map(String::as_str)),
            optional: yes(a, "optional").unwrap_or(false),
            disk: false,
        })
    }

    /// Adds `rom` as the next ROM, found by its name.
    fn push_rom(&mut self, rom: Rom) {
        self.by_name
            .entry(rom.name.clone())
            .or_default()
            .push(self.roms.len());
        self.roms.push(rom);
    }

    /// Makes the last ROM read `size` bytes.
    fn resize_last_rom(&mut self, size: u64) {
        let Some(idx) = self.roms.len().checked_sub(1) else {
            return;
        };
        let rom = &self.roms[idx];
        let bucket = match &rom.crc {
            Some(crc) => self.by_size_crc.get_mut(&(rom.size, crc.clone())),
            None => self.by_size_without_crc.get_mut(&rom.size),
        };
        if let Some(bucket) = bucket {
            bucket.retain(|&i| i != idx);
        }
        let rom = &mut self.roms[idx];
        rom.size = size;
        match &rom.crc {
            Some(crc) => self
                .by_size_crc
                .entry((rom.size, crc.clone()))
                .or_default()
                .push(idx),
            None => self
                .by_size_without_crc
                .entry(rom.size)
                .or_default()
                .push(idx),
        }
    }

    /// The built-in Header Rule the header names; none if it names none, or one Title Select does not know.
    pub fn header(&self) -> Option<HeaderRule> {
        self.header_rule.as_deref().and_then(HeaderRule::named)
    }

    /// What a Build expects of `rom`, one of this DAT's ROMs: its size and checksums, and the Header Rule they leave
    /// a Header out under.
    pub fn expected(&self, rom: &Rom) -> title_select_api::build::ExpectedRom {
        title_select_api::build::ExpectedRom {
            name: rom.name.clone(),
            size: rom.size,
            crc: rom.crc.clone(),
            md5: rom.md5.clone(),
            sha1: rom.sha1.clone(),
            sha256: rom.sha256.clone(),
            header: self.header(),
        }
    }

    pub fn roms_named(&self, name: &str) -> &[usize] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// The ROMs a payload of this size and CRC32 could match; each must still be compared in full.
    pub fn candidates(&self, size: u64, crc: &str) -> impl Iterator<Item = usize> + '_ {
        let with_crc = self.by_size_crc.get(&(size, crc.to_string()));
        let without_crc = self.by_size_without_crc.get(&size);
        with_crc.into_iter().chain(without_crc).flatten().copied()
    }
}
