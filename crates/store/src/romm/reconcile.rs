//! Comparing RomM's view of a Library Folder with the files its Builds made, once a scan the Build requested finished
//! (#143). Title Select never writes identity into RomM (ADR 0020): it reads the platform's ROMs and their files, and
//! shows where RomM disagrees. A file in the folder no Build made isn't compared: Drift covers it.
//!
//! RomM 5.4.1 (research 0136): `GET /api/platforms` names each platform's internal id beside its `fs_slug`;
//! `GET /api/roms?platform_ids=` pages a platform's ROMs with `limit` and `offset`, each with its `files[]` when
//! `with_files=true`. A ROM is a file or a folder directly in the platform folder, its `fs_name`; each of its files
//! has a `full_path` under the ROM's `fs_path`, and a `sha1_hash`, empty when RomM didn't hash it. A zip's own hash is
//! RomM's composite of its members, so a zip is compared by its `archive_members`; a CHD by the SHA-1 its header
//! embeds, `chd_sha1_hash`. `is_unidentified` is a ROM no metadata provider matched, and `name` the name one did.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Weak;

use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use title_select_api::build::ExpectedRom;
use title_select_api::romm::{
    RommFlag, RommReconcileCounts, RommReconcileRow, RommReconciliation, RommRescan,
    RommScanOutcome,
};
use unicode_normalization::UnicodeNormalization;

use super::{RommClient, RommError, ScanStop, ScanTiming, Task};
use crate::config::Config;
use crate::plans::Built;
use crate::{Error, Result, Store, now};

/// How many ROMs each request for a platform's ROMs asks RomM for.
pub const PAGE: usize = 250;
/// The largest page of ROMs read from RomM: each ROM comes with its metadata and every file.
const PAGE_LIMIT: u64 = 64 << 20;

#[derive(Deserialize)]
struct Platform {
    id: i64,
    fs_slug: String,
}

#[derive(Deserialize)]
struct Page {
    items: Vec<RommRom>,
    #[serde(default)]
    total: Option<u64>,
}

#[derive(Deserialize)]
struct RommRom {
    id: i64,
    fs_path: String,
    fs_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_unidentified: bool,
    #[serde(default)]
    missing_from_fs: bool,
    #[serde(default)]
    files: Vec<RommFile>,
}

#[derive(Deserialize)]
struct RommFile {
    full_path: String,
    #[serde(default)]
    sha1_hash: Option<String>,
    #[serde(default)]
    chd_sha1_hash: Option<String>,
    #[serde(default)]
    archive_members: Option<Vec<RommMember>>,
}

#[derive(Deserialize)]
struct RommMember {
    name: String,
    #[serde(default)]
    sha1_hash: Option<String>,
}

/// A hash RomM computed, or `None` where it computed none: RomM leaves a hash empty on a platform it doesn't hash.
fn hashed(sha1: &Option<String>) -> Option<String> {
    sha1.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
}

/// A title as it is compared: without its tags, in lowercase, with accents, punctuation and spacing set aside, `&`
/// read as "and", and a trailing article of each ` - ` part (`Legend of Zelda, The`) moved to its front.
fn title_key(title: &str) -> String {
    let mut bare = String::new();
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            c if depth == 0 => bare.push(c),
            _ => {}
        }
    }
    let parts: Vec<String> = bare
        .split(" - ")
        .map(|part| {
            let part = part.trim();
            for article in ["The", "A", "An"] {
                let suffix = format!(", {article}");
                let cut = part.len().saturating_sub(suffix.len());
                if cut > 0
                    && part.is_char_boundary(cut)
                    && part[cut..].eq_ignore_ascii_case(&suffix)
                {
                    return format!("{article} {}", &part[..cut]);
                }
            }
            part.to_owned()
        })
        .collect();
    let joined = parts.join(" ").replace('&', " and ");
    let mut key = String::new();
    for c in joined
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        if c.is_alphanumeric() {
            key.push(c);
        } else if !key.is_empty() && !key.ends_with(' ') {
            key.push(' ');
        }
    }
    key.trim_end().to_owned()
}

/// Whether the DAT Game `game` and the name RomM matched, `romm`, are the same title once tags, case, punctuation and
/// a trailing article are set aside.
fn same_title(game: &str, romm: &str) -> bool {
    title_key(game) == title_key(romm)
}

/// The SHA-1 of the file at `path` as it is now, if it is still the file the Build recorded: one changed since is
/// Drift's to report.
fn sha1_on_disk(path: &Path, built: &Built) -> Option<String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).ok()?;
    if crate::plans::stat(&meta) != built.file {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = title_select_engine::payload::Hasher::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(hasher.finish().sha1)
}

/// What the file built at `path` should hash to as a whole, when that is known: the DAT's SHA-1 of a ROM built loose
/// as it is, or else its SHA-1 on disk, for a generated file (an `.m3u` or SBI) or a ROM written with a Header.
fn expected_sha1(path: &Path, built: &Built) -> Option<String> {
    match &built.roms[..] {
        [
            ExpectedRom {
                sha1: Some(sha1),
                header: None,
                ..
            },
        ] => Some(sha1.to_ascii_lowercase()),
        _ => sha1_on_disk(path, built),
    }
}

/// How RomM's `file` disagrees with the file built at `path`, if it does. A hash RomM didn't compute is no
/// disagreement.
fn hash_disagreement(path: &Path, built: &Built, file: &RommFile) -> Option<String> {
    if built.chd {
        let (theirs, ours) = (hashed(&file.chd_sha1_hash)?, built.chd_sha1.as_deref()?);
        return (theirs != ours.to_ascii_lowercase()).then(|| {
            format!("RomM reads the SHA-1 in its CHD header as {theirs}; it was built as {ours}.")
        });
    }
    if built.loose || built.roms.is_empty() {
        let theirs = hashed(&file.sha1_hash)?;
        let ours = expected_sha1(path, built)?;
        return (theirs != ours)
            .then(|| format!("RomM's SHA-1 for it is {theirs}; it was built as {ours}."));
    }
    let Some(members) = file.archive_members.as_ref().filter(|m| !m.is_empty()) else {
        return hashed(&file.sha1_hash).map(|_| {
            "RomM couldn't read inside it, so it hashed the archive as a whole.".to_owned()
        });
    };
    let mut wrong = Vec::new();
    for rom in &built.roms {
        match members.iter().find(|m| m.name == rom.name) {
            None => wrong.push(format!("RomM doesn't list {} in it.", rom.name)),
            Some(member) => {
                if let (Some(theirs), Some(ours)) = (hashed(&member.sha1_hash), rom.sha1.as_deref())
                    && theirs != ours.to_ascii_lowercase()
                {
                    wrong.push(format!(
                        "RomM's SHA-1 for {} in it is {theirs}; it was built as {ours}.",
                        rom.name
                    ));
                }
            }
        }
    }
    (!wrong.is_empty()).then(|| wrong.join(" "))
}

/// Compares `built`, the files the Builds made in the folder at `dir`, with RomM's `roms` of its platform.
fn compare(
    dir: &Path,
    built: &BTreeMap<String, Built>,
    roms: &[RommRom],
) -> (RommReconcileCounts, Vec<RommReconcileRow>) {
    let roms: Vec<&RommRom> = roms.iter().filter(|r| !r.missing_from_fs).collect();
    let by_name: HashMap<&str, &RommRom> = roms.iter().map(|r| (r.fs_name.as_str(), *r)).collect();
    let mut files: HashMap<&str, &RommFile> = HashMap::new();
    for rom in &roms {
        let prefix = format!("{}/", rom.fs_path.trim_end_matches('/'));
        for file in &rom.files {
            if let Some(path) = file.full_path.strip_prefix(&prefix) {
                files.insert(path, file);
            }
        }
    }
    let mut counts = RommReconcileCounts::default();
    let mut rows = Vec::new();
    for (target, built) in built {
        let top = target.split('/').next().unwrap_or(target);
        let rom = by_name.get(top).copied();
        let mut flags = Vec::new();
        let mut detail = None;
        if let Some(rom) = rom {
            if rom.is_unidentified {
                flags.push(RommFlag::Unidentified);
            } else if let Some(name) = rom.name.as_deref()
                && !same_title(&built.game, name)
            {
                flags.push(RommFlag::TitleMismatch);
            }
        }
        match files.get(target.as_str()) {
            None => {
                flags.push(RommFlag::HashMismatch);
                detail = Some("RomM doesn't list this file. Scan the platform in RomM.".to_owned());
            }
            Some(file) => {
                if let Some(why) = hash_disagreement(&dir.join(target), built, file) {
                    flags.push(RommFlag::HashMismatch);
                    detail = Some(why);
                }
            }
        }
        counts.files += 1;
        for flag in &flags {
            match flag {
                RommFlag::Unidentified => counts.unidentified += 1,
                RommFlag::TitleMismatch => counts.title_mismatch += 1,
                RommFlag::HashMismatch => counts.hash_mismatch += 1,
            }
        }
        if flags.is_empty() {
            counts.clean += 1;
            continue;
        }
        rows.push(RommReconcileRow {
            target: target.clone(),
            game: built.game.clone(),
            flags,
            rom_id: rom.map(|r| r.id),
            romm_name: rom.and_then(|r| r.name.clone()),
            detail,
        });
    }
    (counts, rows)
}

/// Why reading RomM stopped: the Store is gone, Title Select is stopping, or RomM's settings changed meanwhile, and
/// nothing is recorded; or a request failed, and that is.
enum Halt {
    Abandon,
    Failed(String),
}

impl Store {
    /// Compares RomM's view of the Library Folder `folder` with the files its Builds made, and keeps it as the
    /// folder's latest reconciliation, replacing the one before; `job` is the Job whose RomM scan led to it, if one
    /// did. It reads RomM's ROMs of the folder's platform `page` at a time. A request RomM refuses or doesn't answer
    /// is kept as a reconciliation that says why. Returns `None`, recording nothing, when RomM is off or the folder
    /// isn't a platform RomM scans; or when, before it ends, the Store is gone, `stop` is signalled, RomM is turned
    /// off or its address or root changes.
    ///
    /// Like the scan, it holds the Store only to read and record, never while it waits on RomM, and each request
    /// reads RomM's settings as they are then.
    pub fn reconcile_romm(
        store: &Weak<Store>,
        config: &Config,
        folder: i64,
        job: Option<i64>,
        page: usize,
        timing: &ScanTiming,
        stop: &ScanStop,
    ) -> Result<Option<RommReconciliation>> {
        let page = page.max(1);
        let (was, fs_slug, dir) = {
            let Some(store) = store.upgrade() else {
                return Ok(None);
            };
            let romm = store.settings()?.romm;
            if !romm.is_on() {
                return Ok(None);
            }
            let (root, path) = store.folder_place(folder)?;
            let platform = romm.platform(config, &root, &path);
            let (Some(fs_slug), Some(root)) = (platform.fs_slug(), config.root(&root)) else {
                return Ok(None);
            };
            (romm, fs_slug.to_owned(), root.path.join(&path))
        };
        let client = || -> std::result::Result<RommClient, Halt> {
            if stop.is_stopped() {
                return Err(Halt::Abandon);
            }
            let settings = store.upgrade().ok_or(Halt::Abandon)?.settings();
            let now = settings
                .map_err(|e| Halt::Failed(format!("Your RomM settings can't be read: {e}")))?
                .romm;
            if !now.is_on() || now.base_url != was.base_url || now.root != was.root {
                return Err(Halt::Abandon);
            }
            RommClient::of(&now)
                .expect("RomM that is on has an address")
                .map(|c| c.with_timeout(timing.request).with_body_limit(PAGE_LIMIT))
                .map_err(|e| Halt::Failed(e.to_string()))
        };
        let read = || -> std::result::Result<Vec<RommRom>, Halt> {
            let failed = |e: RommError| Halt::Failed(e.to_string());
            let platforms: Vec<Platform> = client()?.get("/api/platforms").map_err(failed)?;
            let Some(platform) = platforms.iter().find(|p| p.fs_slug == fs_slug) else {
                return Err(Halt::Failed(format!(
                    "RomM has no platform {fs_slug}. Scan your library in RomM so it finds the folder"
                )));
            };
            let mut roms = Vec::new();
            loop {
                let path = format!(
                    "/api/roms?platform_ids={}&limit={page}&offset={}&with_files=true&with_char_index=false\
                     &with_filter_values=false&with_rom_id_index=false&order_by=id&order_dir=asc",
                    platform.id,
                    roms.len()
                );
                let got: Page = client()?.get(&path).map_err(failed)?;
                let ended = got.items.len() < page
                    || got
                        .total
                        .is_some_and(|total| (roms.len() + got.items.len()) as u64 >= total);
                roms.extend(got.items);
                if ended {
                    return Ok(roms);
                }
            }
        };
        let read = read();
        let Some(store) = store.upgrade() else {
            return Ok(None);
        };
        let mut reconciliation = RommReconciliation {
            folder_id: folder,
            job_id: job,
            fs_slug: fs_slug.clone(),
            reconciled_at: now(),
            error: None,
            counts: RommReconcileCounts::default(),
            rows: Vec::new(),
        };
        match read {
            _ if stop.is_stopped() => return Ok(None),
            Err(Halt::Abandon) => return Ok(None),
            Err(Halt::Failed(why)) => {
                reconciliation.error = Some(format!(
                    "{why}, so RomM's view of {fs_slug} wasn't compared with your library folder."
                ))
            }
            Ok(roms) => {
                let built = store.read(|c| crate::plans::read_built(c, folder))?;
                (reconciliation.counts, reconciliation.rows) = compare(&dir, &built, &roms);
            }
        }
        store.record_romm_reconciliation(&reconciliation)?;
        Ok(Some(reconciliation))
    }

    /// Compares RomM's view of the Library Folder the ended Build Job `job` changed, once the scan it requested
    /// finished (see [`Store::reconcile_romm`]). Any other Job, or scan, leads to none.
    pub fn reconcile_romm_scan(
        store: &Weak<Store>,
        config: &Config,
        job: &title_select_api::Job,
        timing: &ScanTiming,
        stop: &ScanStop,
    ) -> Result<Option<RommReconciliation>> {
        let finished = job
            .romm_scan
            .as_ref()
            .is_some_and(|s| s.outcome == RommScanOutcome::Finished);
        match job.library_folder {
            Some(folder) if finished => {
                Store::reconcile_romm(store, config, folder, Some(job.id), PAGE, timing, stop)
            }
            _ => Ok(None),
        }
    }

    fn folder_place(&self, folder: i64) -> Result<(String, String)> {
        self.read(|c| {
            c.query_row(
                "SELECT root, path FROM library_folders WHERE id = ?1",
                [folder],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("library folder {folder} doesn't exist")))
        })
    }

    fn record_romm_reconciliation(&self, r: &RommReconciliation) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO romm_reconciliations (folder_id, job_id, fs_slug, reconciled_at, error, files, clean,
                                                   unidentified, title_mismatch, hash_mismatch, rows)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (folder_id) DO UPDATE SET
                     job_id = ?2, fs_slug = ?3, reconciled_at = ?4, error = ?5, files = ?6, clean = ?7,
                     unidentified = ?8, title_mismatch = ?9, hash_mismatch = ?10, rows = ?11",
                params![
                    r.folder_id,
                    r.job_id,
                    r.fs_slug,
                    r.reconciled_at,
                    r.error,
                    r.counts.files as i64,
                    r.counts.clean as i64,
                    r.counts.unidentified as i64,
                    r.counts.title_mismatch as i64,
                    r.counts.hash_mismatch as i64,
                    serde_json::to_string(&r.rows).expect("JSON serializes"),
                ],
            )?;
            Ok(())
        })
    }

    /// The latest reconciliation of the Library Folder `folder` with RomM, if it has one.
    pub fn romm_reconciliation(&self, folder: i64) -> Result<Option<RommReconciliation>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT folder_id, job_id, fs_slug, reconciled_at, error, files, clean, unidentified,
                        title_mismatch, hash_mismatch, rows
                 FROM romm_reconciliations WHERE folder_id = ?1",
                [folder],
                |r| {
                    let count = |i: usize| r.get::<_, i64>(i).map(|n| n as u64);
                    Ok(RommReconciliation {
                        folder_id: r.get(0)?,
                        job_id: r.get(1)?,
                        fs_slug: r.get(2)?,
                        reconciled_at: r.get(3)?,
                        error: r.get(4)?,
                        counts: RommReconcileCounts {
                            files: count(5)?,
                            clean: count(6)?,
                            unidentified: count(7)?,
                            title_mismatch: count(8)?,
                            hash_mismatch: count(9)?,
                        },
                        rows: crate::plans::json(r, 10)?,
                    })
                },
            )
            .optional()?)
        })
    }

    /// Asks RomM to scan again the ROM `rom_id`, one the Library Folder `folder`'s latest reconciliation flagged: an
    /// unidentified ROM is scanned for a match (`unmatched`), any other is scanned again whole (`update`). Refused
    /// while RomM is off; a request RomM refuses or doesn't answer is refused saying why.
    pub fn request_romm_rescan(&self, folder: i64, rom_id: i64) -> Result<RommRescan> {
        let romm = self.settings()?.romm;
        if !romm.is_on() {
            return Err(Error::Refused(
                "RomM is off. Turn it on in your settings to ask it to scan again.".into(),
            ));
        }
        let latest = self.romm_reconciliation(folder)?;
        let rows: Vec<_> = latest
            .iter()
            .flat_map(|r| &r.rows)
            .filter(|r| r.rom_id == Some(rom_id))
            .collect();
        if rows.is_empty() {
            return Err(Error::Refused(format!(
                "RomM's ROM {rom_id} isn't one the latest comparison of this library folder flagged, so RomM wasn't \
                 asked to scan it again."
            )));
        }
        let scan_type = match rows
            .iter()
            .any(|r| r.flags.contains(&RommFlag::Unidentified))
        {
            true => "unmatched",
            false => "update",
        };
        let client = RommClient::of(&romm)
            .expect("RomM that is on has an address")
            .map_err(|e| Error::Refused(e.to_string()))?;
        let body = serde_json::json!({"type": scan_type, "roms_ids": [rom_id]});
        match client.post::<_, Task>("/api/tasks/scan", &body) {
            Ok(task) => Ok(RommRescan {
                rom_id,
                scan_type: scan_type.into(),
                task_id: task.task_id,
            }),
            Err(RommError::Status { status: 503, .. }) => Err(Error::Refused(
                "RomM has no scan worker running, so it wasn't asked to scan again. Start RomM's worker, then try \
                 again."
                    .into(),
            )),
            Err(e) => Err(Error::Refused(format!(
                "{e}, so RomM wasn't asked to scan again."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_key_sets_tags_case_punctuation_and_articles_aside() {
        for (title, key) in [
            ("Pong (USA)", "pong"),
            (
                "Legend of Zelda, The - A Link to the Past (USA)",
                "the legend of zelda a link to the past",
            ),
            ("Pokémon - Edición Roja (Spain) [b]", "pokemon edicion roja"),
            ("Tom & Jerry", "tom and jerry"),
            ("Saga (USA) (Disc 2)", "saga"),
        ] {
            assert_eq!(title_key(title), key, "{title}");
        }
    }
}
