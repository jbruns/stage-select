//! Single Copy (ADR 0023): which of a plan's items place their Game in the Library Folder holding its Source Set's
//! Placed Games, and how. Only a Game the folder stores as one file, TorrentZip or loose, alone in its release, is
//! placed; and only from Source Set files that hold nothing but it, each matched on its payload, so that consuming
//! them loses nothing. Every other Game is copied as before, and the plan's notes say why.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

use title_select_api::build::{
    FileIdentity, FileOutcome, ItemKind, PlaceHow, PlacedOriginal, PlanItem, PlanPlace, PlanSource,
};
use title_select_api::report::{EvidenceLevel, Finding};
use title_select_api::storage::{release_of, split_placed};

use super::{Folder, Inputs, Sources};
use crate::config::Config;

/// Why a Game a plan could place is copied instead.
const CHD: &str = "it is stored as CHD here, and a CHD is copied, not placed";
const RELEASE: &str = "it goes in place with other files of its release, and only a game stored as one file is placed";
const SHARED: &str = "its files in the source set hold more than this game, so they stay there";
const UNMATCHED: &str = "the latest audit didn't match each of its files in the source set exactly, by their \
                         contents";
const REPLACING: &str = "a file of it built here earlier is replaced or converted first";
const UNCHECKED: &str =
    "its file here wasn't found as it was built, so it can't stand in for its source set's files";

/// Makes each of `items` that can place its Game here a placement, under Single Copy, when this folder holds the
/// Source Set's Placed Games; and says in `notes` what placing does, and why any Game is copied instead.
pub(super) fn place(
    config: &Config,
    inputs: &Inputs,
    folder: Option<&Folder>,
    sources: &mut Sources,
    items: &mut [PlanItem],
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
    let (Some(folder), Some((set_dir, _))) = (folder, sources.set_dir.clone()) else {
        return;
    };
    // A rename moves a file only within one filesystem; a test may say they are two.
    let same_fs = match (std::fs::metadata(&set_dir), folder.dir.identity()) {
        (Ok(set), Ok(here)) => set.dev() == here.dev && !inputs.copy_across,
        _ => false,
    };
    let mut releases: BTreeMap<String, usize> = BTreeMap::new();
    for item in items.iter().filter(|i| i.addition.is_none()) {
        *releases.entry(release_of(&item.game)).or_default() += 1;
    }
    let mut copied: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut renamed, mut copies, mut converted, mut built) = (0, 0, 0, 0);
    let (mut freed, mut written) = (0u64, 0u64);
    for item in items.iter_mut() {
        if !matches!(
            item.kind,
            ItemKind::Copy | ItemKind::Repack | ItemKind::Unchanged
        ) || item.addition.is_some()
            || item.generated.is_some()
            || item.roms.is_empty()
            || inputs.placed.iter().any(|p| p.game == item.game)
        {
            continue;
        }
        let placement = match releases.get(&release_of(&item.game)) {
            _ if item.chd => Err(Some(CHD)),
            // A file in a folder of its own is part of a layout, which goes in place whole.
            _ if item.target.contains('/') => Err(Some(RELEASE)),
            Some(1) => placement(inputs, folder, sources, item, same_fs),
            _ => Err(Some(RELEASE)),
        };
        let (place, from) = match placement {
            Ok(found) => found,
            Err(Some(why)) => {
                *copied.entry(why).or_default() += 1;
                continue;
            }
            Err(None) => continue,
        };
        match place.how {
            PlaceHow::Rename => renamed += 1,
            PlaceHow::Copy => copies += 1,
            PlaceHow::Convert => converted += 1,
            PlaceHow::Built => built += 1,
        }
        freed += place.freed;
        written += place.written;
        item.kind = ItemKind::Place;
        item.sources = from;
        item.place = Some(place);
        item.changes_source_set = true;
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
            "single copy places {} here as {} only copy, taking {} out of source set {set}: {}. It frees about {} \
             there, an estimate, as block cloning may already share some of it with other files, and writes {} \
             here",
            games(placed),
            if placed == 1 { "its" } else { "their" },
            if placed == 1 { "it" } else { "them" },
            ways.join(", "),
            bytes(freed),
            bytes(written)
        ));
    }
    for (why, n) in copied {
        notes.push(match n {
            1 => format!("1 game is copied rather than placed: {why}"),
            n => format!("{n} games are copied rather than placed, each because {why}"),
        });
    }
}

fn games(n: usize) -> String {
    match n {
        1 => "1 game".into(),
        n => format!("{n} games"),
    }
}

fn bytes(n: u64) -> String {
    match n {
        1 => "1 byte".into(),
        n => format!("{n} bytes"),
    }
}

/// How `item`'s Game is placed, and the sources it is written from, one per ROM; or why it is copied instead, if it
/// says, or none when it is not the Source Set's to place.
fn placement(
    inputs: &Inputs,
    folder: &Folder,
    sources: &mut Sources,
    item: &PlanItem,
    same_fs: bool,
) -> Result<(PlanPlace, Vec<PlanSource>), Option<&'static str>> {
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
        // Each file consumed holds this Game alone, every Entry of it matched on its payload and taken: nothing else
        // in it is lost.
        let entries = sources
            .by_path
            .get(source.path.as_str())
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
                && e.roms.iter().all(|r| r.game == item.game)
                && from
                    .iter()
                    .any(|s| s.path == e.path && s.member == e.member)
        });
        if entries.is_empty() || !all_ours {
            return Err(Some(SHARED));
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
    let freed = originals.iter().map(|o| o.file.size).sum();
    let how = match (built, whole, same_fs) {
        (true, ..) => PlaceHow::Built,
        (false, true, true) => PlaceHow::Rename,
        (false, true, false) => PlaceHow::Copy,
        (false, false, _) => PlaceHow::Convert,
    };
    let written = match how {
        PlaceHow::Rename | PlaceHow::Built => 0,
        PlaceHow::Copy => freed,
        PlaceHow::Convert => item.roms.iter().map(|r| r.size).sum(),
    };
    Ok((
        PlanPlace {
            how,
            originals,
            freed,
            written,
        },
        from,
    ))
}

/// The path as `chosen` holds it, borrowed for as long as [`Sources`] caches what it found there.
fn chosen_path<'a>(chosen: &[super::Candidate<'a>], path: &str) -> &'a str {
    chosen
        .iter()
        .find(|c| c.entry.path == path)
        .map(|c| c.entry.path.as_str())
        .expect("a source of the item is one of its chosen entries")
}
