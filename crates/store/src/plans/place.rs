//! Single Copy (ADR 0023): which of a plan's items place their Game in the Library Folder holding its Source Set's
//! Placed Games, and how. A release is placed whole or not at all: every file the folder stores of each of its Games,
//! TorrentZip, loose or in a loose folder, from Source Set files that hold nothing but those Games, each matched on its
//! payload, so that consuming them loses nothing. A file the Source Set doesn't hold, such as a disc's `.m3u` or SBI
//! file, is written into the folder as before. Every other release is copied as before, and the plan's notes say why.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::unix::fs::MetadataExt;

use title_select_api::DatFormat;
use title_select_api::build::{
    FileIdentity, FileOutcome, ItemKind, PlaceHow, PlacedOriginal, PlanItem, PlanPlace, PlanSource,
};
use title_select_api::report::{EvidenceLevel, Finding};
use title_select_api::storage::{release_of, split_placed};

use super::{Folder, Inputs, Sources};
use crate::config::Config;

/// Why a release a plan could place is copied instead.
const CHD: &str = "it is stored as CHD here, and a CHD is copied, not placed, unless you choose to place converted \
                   games too";
const SHARED: &str = "its files in the source set hold more than its games, so they stay there";
const UNMATCHED: &str = "the latest audit didn't match each of its files in the source set exactly, by their \
                         contents";
const REPLACING: &str = "a file of it built here earlier is replaced or converted first";
const UNCHECKED: &str = "its files here weren't all found as they were built, so they can't stand in for its source \
                         set's files";
const MIXED: &str = "some of its files are built here already and others aren't";
const NOT_BUILT: &str = "not every file of it can be built here";
const READ_ELSEWHERE: &str = "another game is written from its ZIP in the source set as well, so that ZIP stays \
                              there";
const NOT_SELF_CONTAINED: &str = "its ZIP in the source set doesn't yet hold every ROM it needs, so it is written from \
                                  other games' ZIPs too, which stay there";

/// How one item of a release is placed, before the release decides how all of them are.
struct Found {
    built: bool,
    whole: bool,
    originals: Vec<PlacedOriginal>,
    sources: Vec<PlanSource>,
}

/// Makes each release of `items` that can be placed here placements, under Single Copy, when this folder holds the
/// Source Set's Placed Games, other than one `held` back; and says in `notes` what placing does, and why any release
/// is copied instead.
pub(super) fn place(
    config: &Config,
    inputs: &Inputs,
    folder: Option<&Folder>,
    sources: &mut Sources,
    items: &mut [PlanItem],
    held: &BTreeMap<String, String>,
    notes: &mut Vec<String>,
) {
    if !inputs.storage.single_copy {
        return;
    }
    let set = &inputs.folder.source_set;
    match inputs.holding {
        None => {
            notes.push(format!(
                "single copy is on, but source set {set} has several play library folders and you haven't chosen \
                 the one that holds its games, so nothing is placed and each game is copied. Choose that folder, \
                 and its next plan places them"
            ));
            return;
        }
        Some(holding) if holding != inputs.folder.id => {
            notes.push(format!(
                "single copy is on, and another play library folder holds source set {set}'s games, so this one \
                 copies each game from wherever it is"
            ));
            return;
        }
        Some(_) => {}
    }
    let writable = config
        .root(&inputs.set_root)
        .is_some_and(|r| r.may_write(&inputs.set_path));
    if !writable {
        notes.push(format!(
            "single copy is on, but root {} doesn't let Title Select write to source set {set}'s folder, so \
             nothing is placed and each game is copied",
            inputs.set_root
        ));
        return;
    }
    // An Arcade DAT's Game is placed only as a non-merged ZIP, its Source Set's own, which holds that Game alone (ADR
    // 0026).
    let arcade = inputs.revision.format == DatFormat::Listxml;
    let layout = inputs
        .set_storage
        .set_layout
        .filter(|l| !l.is_self_contained());
    if let (true, Some(layout)) = (arcade, layout) {
        notes.push(format!(
            "single copy is on, but source set {set} uses the {} set layout, so each of its ZIPs holds more than one \
             game's ROMs: nothing is placed and each game is copied. Single copy places games only from a non-merged \
             source set",
            layout.label()
        ));
        return;
    }
    let (Some(folder), Some((set_dir, _))) = (folder, sources.set_dir.clone()) else {
        return;
    };
    // A rename moves a file only within one filesystem; a test may say they are two.
    let same_fs = match (std::fs::metadata(&set_dir), folder.dir.identity()) {
        (Ok(set), Ok(here)) => set.dev() == here.dev && !inputs.copy_across,
        _ => false,
    };
    // Each release's items, in plan order: the files the Source Set holds, then those written fresh.
    let mut releases: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        if item.addition.is_none() {
            releases.entry(release_of(&item.game)).or_default().push(i);
        }
    }
    // Each Source Set file a copy or a repack reads, by the releases that read it: placing it would take it out from
    // under another release, so only a release that alone reads a file may consume it.
    let mut readers: HashMap<String, HashSet<String>> = HashMap::new();
    for item in items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Copy | ItemKind::Repack))
    {
        for source in &item.sources {
            readers
                .entry(source.path.clone())
                .or_default()
                .insert(release_of(&item.game));
        }
    }
    let mut copied: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut renamed, mut copies, mut converted, mut built) = (0, 0, 0, 0);
    let mut chds = 0;
    let (mut freed, mut written) = (0u64, 0u64);
    for (name, members) in releases.iter().filter(|(r, _)| !held.contains_key(*r)) {
        // A Disk is always copied, never placed: one family's folder of Disks may serve several Games.
        let placing: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| items[i].generated.is_none() && !items[i].disk)
            .collect();
        // Placed already, or nothing of the Source Set's to place.
        if placing.is_empty()
            || placing.iter().any(|&i| {
                items[i].roms.is_empty() || inputs.placed.iter().any(|p| p.game == items[i].game)
            })
        {
            continue;
        }
        let planned = match release(inputs, folder, sources, items, &placing, arcade) {
            Ok(planned) => planned,
            Err(Some(why)) => {
                *copied.entry(why).or_default() += 1;
                continue;
            }
            Err(None) => continue,
        };
        let read_elsewhere = planned.iter().flat_map(|f| &f.originals).any(|o| {
            readers
                .get(&o.path)
                .is_some_and(|by| by.iter().any(|r| r != name))
        });
        if read_elsewhere {
            *copied.entry(READ_ELSEWHERE).or_default() += 1;
            continue;
        }
        let how = match (
            planned.iter().all(|f| f.built),
            planned.iter().any(|f| f.built),
            planned.iter().all(|f| f.whole),
        ) {
            (true, ..) => PlaceHow::Built,
            (false, true, _) => {
                *copied.entry(MIXED).or_default() += 1;
                continue;
            }
            (false, false, true) if same_fs => PlaceHow::Rename,
            (false, false, true) => PlaceHow::Copy,
            (false, false, false) => PlaceHow::Convert,
        };
        match how {
            PlaceHow::Rename => renamed += placing.len(),
            PlaceHow::Copy => copies += placing.len(),
            PlaceHow::Convert => converted += placing.len(),
            PlaceHow::Built => built += placing.len(),
        }
        for (&i, found) in placing.iter().zip(planned) {
            let item = &mut items[i];
            chds += usize::from(item.chd);
            let item_freed = found.originals.iter().map(|o| o.file.size).sum();
            let item_written = match how {
                PlaceHow::Rename | PlaceHow::Built => 0,
                PlaceHow::Copy => item_freed,
                PlaceHow::Convert => item.roms.iter().map(|r| r.size).sum(),
            };
            freed += item_freed;
            written += item_written;
            item.kind = ItemKind::Place;
            item.sources = found.sources;
            item.place = Some(PlanPlace {
                how,
                originals: found.originals,
                freed: item_freed,
                written: item_written,
            });
            item.changes_source_set = true;
        }
    }
    let placed = renamed + copies + converted + built;
    if placed > 0 {
        let ways: Vec<String> = [
            (renamed, "moved by a rename"),
            (copies, "copied across filesystems"),
            (converted, "converted on the way"),
            (built, "already built here"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, how)| format!("{n} {how}"))
        .collect();
        notes.push(format!(
            "single copy places {} here as {} only copy, taking {} out of source set {set}: {}. It frees \
             about {} there and writes about {} here. Both are estimates: block cloning may already share some of \
             what it frees with other files, and a converted file's size depends on how it is compressed",
            files(placed),
            if placed == 1 { "its game's" } else { "their games'" },
            if placed == 1 { "it" } else { "them" },
            ways.join(", "),
            bytes(freed),
            bytes(written)
        ));
    }
    if chds > 0 {
        notes.push(format!(
            "{} placed as CHD, its game's only copy: each is checked by extracting it before it is placed, and a \
             CD's cue sheet, which a CHD doesn't hold, is kept in Title Select's store until it goes back to source \
             set {set}",
            match chds {
                1 => "1 disc is".to_owned(),
                n => format!("{n} discs are"),
            }
        ));
    }
    for (why, n) in copied {
        notes.push(match n {
            1 => format!("1 game's files are copied rather than placed: {why}"),
            n => format!("{n} releases' files are copied rather than placed, each because {why}"),
        });
    }
}

fn files(n: usize) -> String {
    match n {
        1 => "1 file".into(),
        n => format!("{n} files"),
    }
}

fn bytes(n: u64) -> String {
    match n {
        1 => "1 byte".into(),
        n => format!("{n} bytes"),
    }
}

/// How each of `placing`, the items of one release the Source Set holds files for, is placed, in their order; or why
/// the release is copied instead, if it says, or none when it is not the Source Set's to place.
fn release(
    inputs: &Inputs,
    folder: &Folder,
    sources: &mut Sources,
    items: &[PlanItem],
    placing: &[usize],
    arcade: bool,
) -> Result<Vec<Found>, Option<&'static str>> {
    let mut found: Vec<Found> = Vec::new();
    for &i in placing {
        let item = &items[i];
        if !matches!(
            item.kind,
            ItemKind::Copy | ItemKind::Repack | ItemKind::Unchanged
        ) {
            return Err(Some(NOT_BUILT));
        }
        if item.chd && !inputs.storage.place_converted {
            return Err(Some(CHD));
        }
        found.push(placement(inputs, folder, sources, item)?);
    }
    // An Arcade DAT's Game is placed only from its own self-contained ZIP: a Game written from its family's or its
    // Dependencies' ZIPs would consume them.
    if arcade
        && placing.iter().zip(&found).any(|(&i, f)| {
            let own = format!("{}.zip", items[i].game);
            f.originals.iter().any(|o| o.path != own)
        })
    {
        return Err(Some(NOT_SELF_CONTAINED));
    }
    // Each Source Set file consumed holds this release alone, every Entry of it matched on its payload and taken by
    // one of its items: nothing else in it is lost. A file two items are written from is consumed once. A
    // self-contained ZIP of an Arcade DAT's Game holds its Dependencies' ROMs too, which other Games may also be found
    // to hold: it is the Game's alone while the Game takes every Entry of it.
    let games: HashSet<&str> = placing.iter().map(|&i| items[i].game.as_str()).collect();
    let taken: HashSet<(String, Option<String>)> = found
        .iter()
        .flat_map(|f| &f.sources)
        .map(|s| (s.path.clone(), s.member.clone()))
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    for f in &mut found {
        let mut own = Vec::new();
        for o in f.originals.drain(..) {
            let entries = sources
                .by_path
                .get(o.path.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if entries.iter().any(|e| {
                e.finding != Finding::Matched
                    || e.evidence
                        .as_ref()
                        .is_none_or(|ev| ev.level != EvidenceLevel::Payload)
            }) {
                return Err(Some(UNMATCHED));
            }
            let all_ours = entries.iter().all(|e| {
                !e.roms.is_empty()
                    && (arcade || e.roms.iter().all(|r| games.contains(r.game.as_str())))
                    && taken.contains(&(e.path.clone(), e.member.clone()))
            });
            if entries.is_empty() || !all_ours {
                return Err(Some(SHARED));
            }
            if seen.insert(o.path.clone()) {
                own.push(o);
            }
        }
        f.originals = own;
    }
    Ok(found)
}

/// How `item` is placed, and the sources it is written from, one per ROM; or why its release is copied instead, if it
/// says, or none when it is not the Source Set's to place.
fn placement(
    inputs: &Inputs,
    folder: &Folder,
    sources: &mut Sources,
    item: &PlanItem,
) -> Result<Found, Option<&'static str>> {
    if item.keep.is_some() || item.converts.is_some() {
        return Err(Some(REPLACING));
    }
    let built = match item.kind {
        ItemKind::Unchanged => {
            // A built file stands in for the Source Set's only while it is as built, and its SHA-256 is known to check
            // it by before they are deleted.
            let checked = folder.scan.as_ref().is_some_and(|s| {
                s.files.iter().any(|f| {
                    f.target == item.target
                        && matches!(f.outcome, FileOutcome::Unchanged | FileOutcome::Refreshed)
                })
            });
            let recorded = inputs
                .built
                .get(&item.target)
                .is_some_and(|b| b.sha256.is_some());
            if !checked || !recorded {
                return Err(Some(UNCHECKED));
            }
            true
        }
        _ => false,
    };
    let chosen = match sources.choose(&item.game, &item.roms) {
        Ok(chosen) => chosen,
        // Built here, but no longer in the Source Set: nothing of it is the Source Set's to place.
        Err(_) => return Err(None),
    };
    let from: Vec<PlanSource> = chosen
        .iter()
        .zip(&item.roms)
        .map(|(c, rom)| PlanSource {
            rom: rom.name.clone(),
            path: c.entry.path.clone(),
            member: c.entry.member.clone(),
            finding: Some(c.entry.finding),
            evidence: c.entry.evidence.as_ref().map(|e| e.level),
            file: c.file.clone(),
            identity: Some(c.identity),
            // Every source of a placement is read by descriptor, only while it is still the file the plan found.
            ctime_ns: Some(c.ctime_ns),
        })
        .collect();
    let mut originals: Vec<PlacedOriginal> = Vec::new();
    for source in &from {
        if originals.iter().any(|o| o.path == source.path) {
            continue;
        }
        if split_placed(&source.path).is_some() {
            return Err(None);
        }
        let (file, identity, ctime_ns) = sources
            .file(chosen_path(&chosen, &source.path))
            .ok_or(None)?;
        originals.push(PlacedOriginal {
            path: source.path.clone(),
            file,
            identity: FileIdentity {
                dev: identity.dev,
                ino: identity.ino,
            },
            ctime_ns,
        });
    }
    // The file the folder stores is the Source Set's file itself: an exact TorrentZip, or a loose ROM as the DAT
    // lists it.
    let whole = match (
        item.kind,
        originals.as_slice(),
        from.as_slice(),
        item.roms.as_slice(),
    ) {
        (ItemKind::Copy, [_], ..) => true,
        (ItemKind::Repack, [original], [source], [rom]) => {
            item.loose && source.member.is_none() && original.file.size == rom.size
        }
        _ => false,
    };
    Ok(Found {
        built,
        whole,
        originals,
        sources: from,
    })
}

/// The path as `chosen` holds it, borrowed for as long as [`Sources`] caches what it found there.
fn chosen_path<'a>(chosen: &[super::Candidate<'a>], path: &str) -> &'a str {
    chosen
        .iter()
        .find(|c| c.entry.path == path)
        .map(|c| c.entry.path.as_str())
        .expect("a source of the item is one of its chosen entries")
}
