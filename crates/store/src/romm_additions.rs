//! RomM's "Additions" collection (#142, ADR 0020): one public static collection holding every Addition built into a
//! Library Folder that maps to a RomM platform, kept in step after each scan a Build Job requested finishes. It is
//! tracked by RomM's id for it, never by its name, made again when it is gone, kept public, and deleted when RomM is
//! turned off or its address changes, until RomM confirms it is gone. Keeping it in step is a warning on the Job when it goes wrong, never the Job's failure.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, Weak};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Deserialize;
use title_select_api::romm::{RommAdditions, RommAdditionsOutcome, RommScanOutcome};
use title_select_api::{Job, JobKind};

use crate::config::Config;
use crate::fetch::Method;
use crate::romm::{Romm, RommClient, RommError, ScanStop, ScanTiming};
use crate::{Result, Store, now};

/// The collection's name in RomM.
pub const NAME: &str = "Additions";
const DESCRIPTION: &str = "Every addition Title Select built into a library folder RomM scans. Title Select keeps \
                           it in step after each build, so a change you make to it here is undone.";

/// One change to the collection at a time, across every Store in the process: a sync, or deleting it once RomM is
/// turned off, so a deletion never races a sync that would make it again.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Counts one sync waiting its turn until dropped.
struct Waiting<'a>(&'a AtomicUsize);

impl Waiting<'_> {
    fn new(count: &AtomicUsize) -> Waiting<'_> {
        count.fetch_add(1, Ordering::SeqCst);
        Waiting(count)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Store {
    /// How many of this store's syncs of the Additions collection wait their turn now.
    #[doc(hidden)]
    pub fn romm_syncs_waiting(&self) -> usize {
        self.romm_syncs_waiting.load(Ordering::SeqCst)
    }
}

/// The fields of RomM's collection that Title Select reads.
#[derive(Deserialize)]
struct Collection {
    id: i64,
    name: String,
    #[serde(default)]
    rom_ids: BTreeSet<i64>,
    #[serde(default)]
    is_public: bool,
}

/// The field of RomM's ROM that Title Select reads.
#[derive(Deserialize)]
struct Rom {
    id: i64,
}

/// An Addition built into a Library Folder RomM maps.
struct Built {
    name: String,
    sha1: Option<String>,
}

/// The collection's sync recorded on a Job, read from its column `at`.
pub(crate) fn from_column(
    row: &rusqlite::Row,
    at: usize,
) -> rusqlite::Result<Option<RommAdditions>> {
    let Some(json) = row.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    serde_json::from_str(&json).map(Some).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(at, rusqlite::types::Type::Text, e.into())
    })
}

/// The collection tracked at RomM's address `base_url`, if any.
fn tracked(tx: &rusqlite::Connection, base_url: &str) -> rusqlite::Result<Option<i64>> {
    tx.query_row(
        "SELECT collection_id FROM romm_additions_collection WHERE base_url = ?1",
        [base_url],
        |r| r.get(0),
    )
    .optional()
}

/// Records the collection a settings update leaves to delete: the one tracked at the RomM `was`, once the update
/// turned RomM off or changed its address, with `was`'s address and token. It stops being tracked in the update's own
/// transaction, and replaces any deletion still pending. Returns whether it left one.
pub(crate) fn release(tx: &Transaction, was: &Romm, now: &Romm) -> Result<bool> {
    if !was.is_on() || (now.is_on() && now.base_url == was.base_url) {
        return Ok(false);
    }
    let left: Option<(String, i64)> = tx
        .query_row(
            "DELETE FROM romm_additions_collection RETURNING base_url, collection_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((base_url, collection_id)) = left else {
        return Ok(false);
    };
    let (Some(at), Some(token)) = (&was.base_url, &was.token) else {
        return Ok(false);
    };
    if *at != base_url {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO romm_released_collection (id, base_url, token, collection_id) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET base_url = ?1, token = ?2, collection_id = ?3",
        params![base_url, token, collection_id],
    )?;
    Ok(true)
}

/// What became of the collection left to delete. It never names the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Released {
    /// None was left to delete.
    Nothing,
    /// RomM deleted the collection of this id, or it was already gone.
    Deleted(i64),
    /// RomM didn't delete it; it is tried again at the next start and before the next sync.
    Failed {
        collection_id: i64,
        error: RommError,
    },
}

/// Deletes the collection left to delete, if any, holding the Store only to read and clear the record. Called with
/// [`ONE_AT_A_TIME`] held.
fn delete_released(store: &Weak<Store>, timeout: Option<std::time::Duration>) -> Result<Released> {
    let Some(pending) = store.upgrade() else {
        return Ok(Released::Nothing);
    };
    // One tracked again at the same address is in use, so it is no longer to delete.
    let left: Option<(String, String, i64)> = pending.write(|tx| {
        let left: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT base_url, token, collection_id FROM romm_released_collection",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((base_url, _, collection_id)) = &left else {
            return Ok(None);
        };
        if tracked(tx, base_url)? == Some(*collection_id) {
            tx.execute("DELETE FROM romm_released_collection", [])?;
            return Ok(None);
        }
        Ok(left)
    })?;
    drop(pending);
    let Some((base_url, token, collection_id)) = left else {
        return Ok(Released::Nothing);
    };
    let sent = RommClient::new(&base_url, Some(&token)).and_then(|client| {
        let client = match timeout {
            Some(timeout) => client.with_timeout(timeout),
            None => client,
        };
        match client.send(
            Method::Delete,
            &format!("/api/collections/{collection_id}"),
            None,
        ) {
            Ok(_) | Err(RommError::Status { status: 404, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    });
    if let Err(error) = sent {
        return Ok(Released::Failed {
            collection_id,
            error,
        });
    }
    if let Some(store) = store.upgrade() {
        store.write(|tx| {
            tx.execute(
                "DELETE FROM romm_released_collection WHERE base_url = ?1 AND collection_id = ?2",
                params![base_url, collection_id],
            )?;
            Ok(())
        })?;
    }
    Ok(Released::Deleted(collection_id))
}

/// A form RomM reads as `multipart/form-data`, and its content type.
fn multipart(fields: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = format!(
        "title-select-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let mut body = String::new();
    for (name, value) in fields {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    (
        format!("multipart/form-data; boundary={boundary}"),
        body.into_bytes(),
    )
}

/// How a sync ends early: recorded as `outcome` with `detail`, or not recorded once the Store is gone.
type Early = Option<(RommAdditionsOutcome, String)>;

/// What to say of a sync that stopped because of `why`.
fn not_synced(why: &str) -> String {
    format!(
        "{why}, so RomM's \"{NAME}\" collection may not hold every addition RomM has indexed. The next build that \
         RomM scans brings it in step."
    )
}

/// Keeps the collection in step with `built` through the RomM `was`, sending each request through the RomM settings
/// as they are then, and stopping once RomM is off, its address or root is no longer `was`'s, or `stop`. Returns
/// the collection's id and members, how many were added and removed, and the names of the Additions RomM hasn't
/// indexed.
fn sync(
    store: &Weak<Store>,
    was: &Romm,
    built: &[Built],
    timing: &ScanTiming,
    stop: &ScanStop,
    result: &mut RommAdditions,
) -> std::result::Result<(), Early> {
    let failed = |e: RommError| Some((RommAdditionsOutcome::Error, not_synced(&e.to_string())));
    let client = || -> std::result::Result<RommClient, Early> {
        if stop.is_stopped() {
            return Err(Some((
                RommAdditionsOutcome::Stopped,
                not_synced("Title Select stopped"),
            )));
        }
        let now = match store.upgrade().ok_or(None)?.settings() {
            Ok(settings) => settings.romm,
            Err(e) => {
                return Err(Some((
                    RommAdditionsOutcome::Error,
                    not_synced(&format!("Your RomM settings can't be read: {e}")),
                )));
            }
        };
        let why = if !now.is_on() {
            Some("You turned RomM off meanwhile")
        } else if now.base_url != was.base_url {
            Some("You changed RomM's address meanwhile")
        } else if now.root != was.root {
            Some("You changed the root RomM mounts as its library meanwhile")
        } else {
            None
        };
        if let Some(why) = why {
            return Err(Some((RommAdditionsOutcome::Stopped, not_synced(why))));
        }
        match RommClient::of(&now).expect("RomM that is on has an address") {
            Ok(client) => Ok(client.with_timeout(timing.request)),
            Err(e) => Err(failed(e)),
        }
    };

    // Without `collections.write` no collection is touched; the check itself adds nothing to a collection RomM
    // never makes.
    let mut missing = Vec::new();
    for scope in ["roms.read", "collections.write"] {
        if client()?.lacks(scope).map_err(failed)? {
            missing.push(scope);
        }
    }
    if !missing.contains(&"collections.write")
        && client()?.lacks("collections.read").map_err(failed)?
    {
        missing.push("collections.read");
    }
    if !missing.is_empty() {
        let scopes = missing
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" and ");
        let what = if missing.len() == 1 {
            "scope"
        } else {
            "scopes"
        };
        return Err(Some((
            RommAdditionsOutcome::Error,
            not_synced(&format!(
                "Your RomM token lacks the {scopes} {what}. Create a token with every scope Title Select needs in \
                 RomM and save it in your RomM settings"
            )),
        )));
    }

    let mut want = BTreeSet::new();
    for addition in built {
        let found = match &addition.sha1 {
            Some(sha1) => {
                match client()?.get::<Rom>(&format!("/api/roms/by-hash?sha1_hash={sha1}")) {
                    Ok(rom) => Some(rom.id),
                    Err(RommError::Status { status: 404, .. }) => None,
                    Err(e) => return Err(failed(e)),
                }
            }
            None => None,
        };
        match found {
            Some(id) => {
                want.insert(id);
            }
            None => result.unresolved.push(addition.name.clone()),
        }
    }

    let base_url = was
        .base_url
        .as_deref()
        .expect("RomM that is on has an address");
    let known = {
        let store = store.upgrade().ok_or(None)?;
        store
            .read(|c| Ok(tracked(c, base_url)?))
            .map_err(|e| Some((RommAdditionsOutcome::Error, not_synced(&e.to_string()))))?
    };
    let found = match known {
        Some(id) => match client()?.get::<Collection>(&format!("/api/collections/{id}")) {
            Ok(collection) => Some(collection),
            Err(RommError::Status { status: 404, .. }) => None,
            Err(e) => return Err(failed(e)),
        },
        None => None,
    };
    let collection = match found {
        Some(collection) => collection,
        None => {
            let (content_type, body) = multipart(&[("name", NAME), ("description", DESCRIPTION)]);
            let path = "/api/collections?is_public=true";
            let (made, created) = match client()?.send_body(
                Method::Post,
                path,
                Some((&content_type, &body)),
            ) {
                Ok(bytes) => (
                    RommClient::read::<Collection>(path, &bytes).map_err(failed)?,
                    true,
                ),
                // The token's user already has a collection of that name, which Title Select no longer tracks.
                // RomM lists other users' public collections too, so only one the token may change is its user's:
                // adding no ROM to it changes nothing.
                Err(RommError::Status { status: 409, .. }) => {
                    let all: Vec<Collection> = client()?.get("/api/collections").map_err(failed)?;
                    let mut owned = Vec::new();
                    for collection in all.into_iter().filter(|c| c.name == NAME) {
                        match client()?.send(
                            Method::Post,
                            &format!("/api/collections/{}/roms", collection.id),
                            Some(&serde_json::json!({ "rom_ids": [] })),
                        ) {
                            Ok(_) => owned.push(collection),
                            Err(RommError::Forbidden { .. }) => {}
                            Err(e) => return Err(failed(e)),
                        }
                    }
                    let mut named = owned.into_iter();
                    match (named.next(), named.next()) {
                        (Some(one), None) => (one, false),
                        _ => {
                            return Err(Some((
                                RommAdditionsOutcome::Error,
                                not_synced(&format!(
                                    "RomM says your token's user already has a collection named \"{NAME}\", but \
                                     Title Select can't find exactly one. Rename or delete it in RomM"
                                )),
                            )));
                        }
                    }
                }
                Err(e) => return Err(failed(e)),
            };
            let store = store.upgrade().ok_or(None)?;
            let kept = store
                .write(|tx| {
                    let now = crate::settings::load(tx)?.romm;
                    if !now.is_on() || now.base_url != was.base_url {
                        return Ok(false);
                    }
                    tx.execute(
                        "INSERT INTO romm_additions_collection (id, base_url, collection_id) VALUES (1, ?1, ?2)
                         ON CONFLICT (id) DO UPDATE SET base_url = ?1, collection_id = ?2",
                        params![base_url, made.id],
                    )?;
                    // Adopted again, it is no longer to delete.
                    tx.execute(
                        "DELETE FROM romm_released_collection WHERE base_url = ?1 AND collection_id = ?2",
                        params![base_url, made.id],
                    )?;
                    Ok(true)
                })
                .map_err(|e| Some((RommAdditionsOutcome::Error, not_synced(&e.to_string()))))?;
            if !kept {
                // RomM was turned off or moved before the collection was tracked: no settings update will delete it.
                if created {
                    let _ = RommClient::of(was)
                        .expect("RomM that was on has an address")
                        .and_then(|c| {
                            c.with_timeout(timing.request).send(
                                Method::Delete,
                                &format!("/api/collections/{}", made.id),
                                None,
                            )
                        });
                }
                return Err(Some((
                    RommAdditionsOutcome::Stopped,
                    not_synced("You turned RomM off or changed its address meanwhile"),
                )));
            }
            made
        }
    };
    result.collection_id = Some(collection.id);
    if !collection.is_public {
        client()?
            .send(
                Method::Put,
                &format!("/api/collections/{}/visibility", collection.id),
                Some(&serde_json::json!({ "is_public": true })),
            )
            .map_err(failed)?;
    }

    let roms = format!("/api/collections/{}/roms", collection.id);
    let add: Vec<i64> = want.difference(&collection.rom_ids).copied().collect();
    let remove: Vec<i64> = collection.rom_ids.difference(&want).copied().collect();
    if !add.is_empty() {
        client()?
            .send(
                Method::Post,
                &roms,
                Some(&serde_json::json!({ "rom_ids": add })),
            )
            .map_err(failed)?;
        result.added = add.len() as u64;
    }
    if !remove.is_empty() {
        client()?
            .send(
                Method::Delete,
                &roms,
                Some(&serde_json::json!({ "rom_ids": remove })),
            )
            .map_err(failed)?;
        result.removed = remove.len() as u64;
    }
    result.members = want.len() as u64;
    Ok(())
}

impl Store {
    /// Each Addition built into a registered Library Folder that maps to a RomM platform, by the RomM `romm`, with
    /// the SHA-1 of its one ROM, by name.
    fn built_additions(&self, config: &Config, romm: &Romm) -> Result<Vec<Built>> {
        let rows: Vec<(i64, String, Option<String>, String, String)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT a.id, a.name, json_extract(a.rom, '$.sha1'), f.root, f.path
                 FROM built_files b
                 JOIN additions a ON a.id = b.addition_id
                 JOIN library_folders f ON f.id = b.folder_id
                 WHERE f.unregistered_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        let mut built = BTreeMap::new();
        for (id, name, sha1, root, path) in rows {
            if romm.platform(config, &root, &path).fs_slug().is_some() {
                built.insert(
                    id,
                    Built {
                        name,
                        sha1: sha1.map(|s| s.to_ascii_lowercase()),
                    },
                );
            }
        }
        let mut built: Vec<Built> = built.into_values().collect();
        built.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(built)
    }

    fn record_romm_additions(&self, job: i64, additions: &RommAdditions) -> Result<Job> {
        self.write(|tx| {
            tx.execute(
                "UPDATE romm_scans SET additions = ?2 WHERE job_id = ?1",
                params![
                    job,
                    serde_json::to_string(additions).expect("JSON serializes")
                ],
            )?;
            tx.execute("UPDATE jobs SET version = version + 1 WHERE id = ?1", [job])?;
            crate::jobs::job_by_id(tx, job)
        })
    }

    /// Deletes in RomM the collection a settings update left to delete, if any, through the address and token it was
    /// made with, and stops recording it once RomM deleted it or it was already gone. One RomM didn't delete stays
    /// recorded, to try again. Holds the Store only to read and clear the record.
    pub fn delete_released_romm_collection(store: &Weak<Store>) -> Result<Released> {
        let _one = one_at_a_time();
        delete_released(store, None)
    }

    /// Keeps RomM's "Additions" collection in step once the scan the Build Job `job` requested has finished: it
    /// holds exactly the Additions built into Library Folders RomM maps that RomM has indexed, found by SHA-1. An
    /// Addition RomM hasn't indexed is listed as unresolved. The collection is made when it isn't tracked or is gone.
    /// Records the outcome on the Job's RomM scan, tells `changed` of the Job, and returns it; returns `None` when
    /// the Job owes no sync, as when its scan didn't finish, or once the Store is gone. Whatever happens, the Job's
    /// own state stays as it ended.
    ///
    /// Like [`Store::request_romm_scan`], it holds the Store only to read and record, reads the RomM settings before
    /// each request, and stops once RomM is turned off, its address or root changes, or `stop`.
    pub fn sync_romm_additions(
        store: &Weak<Store>,
        config: &Config,
        job: i64,
        timing: &ScanTiming,
        stop: &ScanStop,
        changed: &mut dyn FnMut(&Job),
    ) -> Result<Option<Job>> {
        let waiting = store.upgrade().map(|s| s.romm_syncs_waiting.clone());
        let _one = {
            let _waiting = waiting.as_deref().map(Waiting::new);
            one_at_a_time()
        };
        // A deletion RomM refused is tried again first; it never stops the sync.
        let _ = delete_released(store, Some(timing.request));
        // What is built is read only now it is this sync's turn, so it never undoes a later one.
        let (romm, built) = {
            let Some(store) = store.upgrade() else {
                return Ok(None);
            };
            let ended = store.job(job)?;
            let owed = ended.kind == JobKind::Build
                && ended.romm_scan.as_ref().is_some_and(|scan| {
                    scan.outcome == RommScanOutcome::Finished && scan.additions.is_none()
                });
            let romm = store.settings()?.romm;
            if !owed || !romm.is_on() {
                return Ok(None);
            }
            let built = store.built_additions(config, &romm)?;
            (romm, built)
        };
        let mut result = RommAdditions {
            outcome: RommAdditionsOutcome::Synced,
            collection_id: None,
            members: 0,
            added: 0,
            removed: 0,
            unresolved: Vec::new(),
            detail: None,
            synced_at: String::new(),
        };
        if let Err(early) = sync(store, &romm, &built, timing, stop, &mut result) {
            let Some((outcome, detail)) = early else {
                return Ok(None);
            };
            result.outcome = outcome;
            result.detail = Some(detail);
        }
        result.synced_at = now();
        let Some(store) = store.upgrade() else {
            return Ok(None);
        };
        let job = store.record_romm_additions(job, &result)?;
        changed(&job);
        Ok(Some(job))
    }
}
