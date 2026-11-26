//! Stale Source Sets: the stat walk, a low-priority read-only Job that compares each file's stamp with the latest
//! Audit's by the rule an Incremental Audit uses to reuse a read, and records what looks changed. A file whose stamp
//! moved only in its change time, as a change of permissions or owner moves it, is unconfirmed rather than changed
//! (ADR 0031): the next Incremental Audit reads it again, as it reads any file whose stamp moved.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::jobs::{check_no_active, finish_running, job_by_id};
use super::sets::{resolve, set_by_name};
use super::{Error, Result, Store, now};
use stage_select_api::{FollowedRevision, Job, JobKind, JobState, StatWalk};
use stage_select_engine::payload::Checksums;
use stage_select_engine::{FileStamp, HeaderRule, StatEntry};

/// Why a restart Interrupted a stat walk, which is never queued again: the next one finds the same.
pub(crate) const WALK_INTERRUPTED: &str = "the service stopped while the check for changes was running; nothing in \
     the source set was changed. Check it again";
/// Why a stat walk stopped early: a Repair or Build wanted the Source Set or the job slot.
const WALK_YIELDED: &str = "stopped to let a repair or build run; nothing in the source set was changed. Check it \
     again once it ends";
/// How often a running stat walk asks whether a Repair or Build waits for it.
const YIELD_POLL: Duration = Duration::from_millis(500);

/// The stamps the latest Audit saw, by path relative to the Source Set.
struct Baseline {
    audit: i64,
    /// Every path the Audit saw, with its stamp where it recorded one.
    stamps: BTreeMap<String, Option<FileStamp>>,
    /// The files a re-read found holding other bytes than the Audit read, whatever their stamp says now.
    differs: BTreeSet<String>,
    /// When the Audit started, in nanoseconds since the Unix epoch: a file it saw without a stamp looks changed if
    /// its modification or status change time is later.
    started_ns: i128,
}

/// What a stat walk counted.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    changed: u64,
    added: u64,
    removed: u64,
    unconfirmed: u64,
}

/// How a file the Audit saw compares with it now.
#[derive(Debug, PartialEq, Eq)]
enum Compared {
    /// As the Audit saw it, or with nothing to compare.
    Same,
    /// Its device, inode, size and modification time hold; only its change time moved.
    Unconfirmed,
    /// Changed, added to or gone from what the Audit stamped.
    Changed,
}

impl Counts {
    fn of(baseline: &Baseline, walked: &[StatEntry]) -> Counts {
        let mut counts = Counts::default();
        let mut seen = BTreeSet::new();
        for entry in walked {
            let (rel, stamp) = match entry {
                StatEntry::File { rel, stamp } => (rel, Some(stamp.as_ref())),
                StatEntry::Unreadable { rel } => (rel, None),
            };
            seen.insert(rel.as_str());
            match baseline.stamps.get(rel) {
                None => counts.added += 1,
                Some(before) => {
                    let compared = match (before, stamp) {
                        (Some(_), Some(Some(_))) if baseline.differs.contains(rel) => {
                            Compared::Changed
                        }
                        (Some(before), Some(Some(now))) if before == now => Compared::Same,
                        (Some(before), Some(Some(now))) if only_ctime_moved(before, now) => {
                            Compared::Unconfirmed
                        }
                        (Some(_), Some(Some(_))) => Compared::Changed,
                        // No stamp shows what held, so either time moving is a change.
                        (None, Some(Some(now)))
                            if now.mtime_ns > baseline.started_ns
                                || now.ctime_ns > baseline.started_ns =>
                        {
                            Compared::Changed
                        }
                        (None, Some(Some(_))) => Compared::Same,
                        // A file the audit stamped that can no longer be reached: a dangling symlink, or one that
                        // now leaves the root.
                        (Some(_), None) => Compared::Changed,
                        // Unreadable to the audit too, or a platform with no stamps.
                        (None, None) | (_, Some(None)) => Compared::Same,
                    };
                    match compared {
                        Compared::Same => {}
                        Compared::Unconfirmed => counts.unconfirmed += 1,
                        Compared::Changed => counts.changed += 1,
                    }
                }
            }
        }
        counts.removed = baseline
            .stamps
            .keys()
            .filter(|path| !seen.contains(path.as_str()))
            .count() as u64;
        counts
    }
}

/// Whether `now` is the file `before` stamped, by its device, inode, size and modification time, with only its change
/// time moved.
pub(crate) fn only_ctime_moved(before: &FileStamp, now: &FileStamp) -> bool {
    before.ctime_ns != now.ctime_ns
        && FileStamp {
            ctime_ns: now.ctime_ns,
            ..*before
        } == *now
}

/// The Source Set's latest finished Audit, if any.
fn latest_audit(c: &Connection, set: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM audits WHERE source_set_id = ?1 AND status = 'finished' ORDER BY finish_seq DESC LIMIT 1",
        [set],
        |r| r.get(0),
    )
    .optional()?)
}

fn baseline(c: &Connection, audit: i64) -> Result<Baseline> {
    let started: String = c.query_row(
        "SELECT started_at FROM audits WHERE id = ?1",
        [audit],
        |r| r.get(0),
    )?;
    let started_ns = started
        .parse::<jiff::Timestamp>()
        .map_err(|e| Error::Io(format!("audit {audit}: {e}")))?
        .as_nanosecond();
    let mut stamps = BTreeMap::new();
    let mut entries = c.prepare("SELECT DISTINCT path FROM audit_entries WHERE audit_id = ?1")?;
    for path in entries.query_map([audit], |r| r.get::<_, String>(0))? {
        stamps.insert(path?, None);
    }
    // A read's stamp wins over a claim's: audit_stamps last.
    for table in ["audit_claim_stamps", "audit_stamps"] {
        let mut rows = c.prepare(&format!(
            "SELECT path, dev, ino, size, mtime_ns, ctime_ns FROM {table} WHERE audit_id = ?1"
        ))?;
        for row in rows.query_map([audit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                FileStamp {
                    dev: r.get::<_, i64>(1)? as u64,
                    ino: r.get::<_, i64>(2)? as u64,
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get::<_, i64>(4)? as i128,
                    ctime_ns: r.get::<_, i64>(5)? as i128,
                },
            ))
        })? {
            let (path, stamp) = row?;
            stamps.insert(path, Some(stamp));
        }
    }
    let differs = c
        .prepare("SELECT path FROM audit_stamps WHERE audit_id = ?1 AND differs = 1")?
        .query_map([audit], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    Ok(Baseline {
        audit,
        stamps,
        differs,
        started_ns,
    })
}

/// Whether a Repair or Build wants what a stat walk of Source Set `set` holds: one of the Source Set Queued or
/// Running, one of it that ran since `since`, or any Queued, which waits for a job slot.
fn yield_to(c: &Connection, set: i64, since: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind IN ('build', 'repair') AND (
             state = 'queued'
             OR (source_set_id = ?1 AND (state = 'running' OR finished_at >= ?2))))",
        params![set, since],
        |r| r.get(0),
    )?)
}

/// Records what the walk Job `job` of Source Set `set` found against `audit`.
fn record(tx: &Transaction, set: i64, audit: i64, job: i64, counts: &Counts) -> Result<()> {
    let watch = crate::observe::StaleWatch::start(tx)?;
    let at = now();
    let latest: Option<(i64, i64, i64, i64, i64, i64)> = tx
        .query_row(
            "SELECT id, audit_id, changed, added, removed, unconfirmed FROM stat_walks
             WHERE source_set_id = ?1 ORDER BY id DESC LIMIT 1",
            [set],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    let same = |(_, a, c, ad, r, u): (i64, i64, i64, i64, i64, i64)| {
        a == audit
            && c as u64 == counts.changed
            && ad as u64 == counts.added
            && r as u64 == counts.removed
            && u as u64 == counts.unconfirmed
    };
    if let Some(latest) = latest.filter(|l| same(*l)) {
        tx.execute(
            "UPDATE stat_walks SET checked_at = ?2, job_id = ?3 WHERE id = ?1",
            params![latest.0, at, job],
        )?;
        return watch.finish(tx);
    }
    tx.execute(
        "DELETE FROM stat_walks WHERE source_set_id = ?1 AND changed + added + removed + unconfirmed = 0",
        [set],
    )?;
    tx.execute(
        "INSERT INTO stat_walks (source_set_id, audit_id, job_id, found_at, checked_at, changed, added, removed,
                                 unconfirmed)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8)",
        params![
            set,
            audit,
            job,
            at,
            counts.changed as i64,
            counts.added as i64,
            counts.removed as i64,
            counts.unconfirmed as i64
        ],
    )?;
    watch.finish(tx)
}

/// Records, as the history of each Following Source Set, the newest revision of its DAT where it is newer than its
/// latest Audit's: what makes it Stale now. Called as a revision is imported, an Audit finishes or you follow.
pub(crate) fn note_followed_revisions(tx: &Transaction) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO followed_revisions (source_set_id, dat_revision_id, found_at)
         SELECT s.id, n.id, ?1 FROM source_sets s
         JOIN audits a ON a.id = (SELECT id FROM audits WHERE source_set_id = s.id AND status = 'finished'
                                  ORDER BY finish_seq DESC LIMIT 1)
         JOIN dat_revisions n ON n.id = (SELECT id FROM dat_revisions
                                         WHERE dat_id = s.dat_id AND deleted_at IS NULL
                                         ORDER BY header_order DESC LIMIT 1)
         JOIN dat_revisions v ON v.id = a.dat_revision_id
         WHERE s.follow AND v.dat_id = s.dat_id AND n.header_order > v.header_order AND s.unregistered_at IS NULL",
        [now()],
    )?;
    Ok(())
}

/// What [`Store::confirm_unchanged`] found of the files it read again.
#[derive(Debug, Default)]
pub(crate) struct Reread {
    /// Each file holding what the Audit read, with its new stamp.
    pub(crate) confirmed: Vec<(String, FileStamp)>,
    /// Each file a re-read found holding other bytes, now or before.
    pub(crate) changed: Vec<String>,
}

/// Records that a re-read found Source Set `set`'s file `path`, its change time `ctime_ns`, holding other bytes than
/// Audit `audit` read: its stamp is marked, so that a stat walk counts it changed, its read is forgotten, and the latest
/// walk against the Audit counts it changed, or a walk is recorded that does, so the Source Set is Stale now. That walk
/// counted it unconfirmed, and counts it so no longer, only where it ran after its change time moved.
fn record_differs(
    tx: &Transaction,
    (set, audit): (i64, i64),
    path: &str,
    ctime_ns: i128,
) -> Result<()> {
    let watch = crate::observe::StaleWatch::start(tx)?;
    tx.execute(
        "UPDATE audit_stamps SET differs = 1 WHERE audit_id = ?1 AND path = ?2 AND unreadable = 0",
        params![audit, path],
    )?;
    tx.execute(
        "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
        params![set, path],
    )?;
    let at = now();
    let latest: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, checked_at FROM stat_walks WHERE source_set_id = ?1 AND audit_id = ?2
             AND id = (SELECT MAX(id) FROM stat_walks WHERE source_set_id = ?1)",
            params![set, audit],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match latest {
        Some((walk, checked)) => {
            let saw_it = checked
                .parse::<jiff::Timestamp>()
                .is_ok_and(|at| at.as_nanosecond() >= ctime_ns);
            tx.execute(
                "UPDATE stat_walks SET changed = changed + 1, unconfirmed = MAX(unconfirmed - ?3, 0),
                                       found_at = ?2, checked_at = ?2
                 WHERE id = ?1",
                params![walk, at, i64::from(saw_it)],
            )?
        }
        None => tx.execute(
            "INSERT INTO stat_walks (source_set_id, audit_id, job_id, found_at, checked_at, changed, added, removed,
                                     unconfirmed)
             VALUES (?1, ?2, NULL, ?3, ?3, 1, 0, 0, 0)",
            params![set, audit, at],
        )?,
    };
    watch.finish(tx)
}

impl Store {
    /// Reads again each file at the paths `moved` gives, in the folder `folder` of Source Set `set`, whose stamp moved
    /// only in its change time since Audit `audit` recorded it, as a change of permissions or owner moves it (ADR
    /// 0031). Each one still holding exactly what the read the Audit recorded found, under the Header Rule it was read
    /// under, is recorded with its new stamp, as that Audit's and its read's, and is confirmed. Each one holding other
    /// bytes is recorded as changed: the stamp is marked so that a stat walk counts it changed, the read is forgotten,
    /// and the latest walk against the Audit counts it changed rather than unconfirmed, so the Source Set is Stale. A
    /// file read as a Disk, one read through a symbolic link, or one that can't be read, is left for the next Audit.
    pub(crate) fn confirm_unchanged(
        &self,
        (set, audit): (i64, i64),
        folder: &std::path::Path,
        moved: &[(&str, FileStamp)],
    ) -> Result<Reread> {
        let mut out = Reread::default();
        for (path, recorded) in moved {
            let found = self.read(|c| {
                let differs: bool = c
                    .query_row(
                        "SELECT differs FROM audit_stamps WHERE audit_id = ?1 AND path = ?2 AND unreadable = 0",
                        params![audit, path],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(false);
                if differs {
                    return Ok(Err(()));
                }
                let row: Option<(FileStamp, String, Option<String>, Option<String>)> = c
                    .query_row(
                        "SELECT dev, ino, size, mtime_ns, ctime_ns, members, header_rule, disk_sha1
                         FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
                        params![set, path],
                        |r| {
                            Ok((
                                FileStamp {
                                    dev: r.get::<_, i64>(0)? as u64,
                                    ino: r.get::<_, i64>(1)? as u64,
                                    size: r.get::<_, i64>(2)? as u64,
                                    mtime_ns: r.get::<_, i64>(3)? as i128,
                                    ctime_ns: r.get::<_, i64>(4)? as i128,
                                },
                                r.get(5)?,
                                r.get(6)?,
                                r.get(7)?,
                            ))
                        },
                    )
                    .optional()?;
                Ok(Ok(row.and_then(|(stamp, members, rule, disk)| {
                    let rule = match rule {
                        Some(rule) => Some(HeaderRule::parse(&rule)?),
                        None => None,
                    };
                    (stamp == *recorded && disk.is_none())
                        .then(|| {
                            serde_json::from_str::<Vec<(Option<String>, Checksums)>>(&members).ok()
                        })
                        .flatten()
                        .map(|members| (members, rule))
                })))
            })?;
            let (members, header) = match found {
                Err(()) => {
                    out.changed.push(path.to_string());
                    continue;
                }
                Ok(None) => continue,
                Ok(Some(found)) => found,
            };
            // Planned for the Operator, by no Job: no Job's meter counts it.
            let Some(read) = stage_select_engine::read_again(
                &folder.join(path),
                folder,
                header,
                &Default::default(),
            ) else {
                continue;
            };
            if !only_ctime_moved(recorded, &read.stamp) || read.disk.is_some() {
                continue;
            }
            if read.members != members {
                self.write(|tx| record_differs(tx, (set, audit), path, read.stamp.ctime_ns))?;
                out.changed.push(path.to_string());
                continue;
            }
            let ctime = read.stamp.ctime_ns as i64;
            self.write(|tx| {
                tx.execute(
                    "UPDATE payload_reads SET ctime_ns = ?3, read_at = ?4 WHERE source_set_id = ?1 AND path = ?2",
                    params![set, path, ctime, read.read_at.to_string()],
                )?;
                tx.execute(
                    "UPDATE audit_stamps SET ctime_ns = ?3 WHERE audit_id = ?1 AND path = ?2 AND unreadable = 0",
                    params![audit, path, ctime],
                )?;
                Ok(())
            })?;
            out.confirmed.push((path.to_string(), read.stamp));
        }
        Ok(out)
    }

    /// The DAT Revisions that made the Source Set `name` Stale because it Follows its DAT, newest first.
    pub fn followed_revisions(&self, name: &str) -> Result<Vec<FollowedRevision>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut rows = c.prepare(
                "SELECT r.sha256, r.version, r.imported_at, f.found_at FROM followed_revisions f
                 JOIN dat_revisions r ON r.id = f.dat_revision_id
                 WHERE f.source_set_id = ?1 ORDER BY r.header_order DESC, r.id DESC",
            )?;
            let found = rows
                .query_map([set.id], |r| {
                    Ok(FollowedRevision {
                        source_set: name.to_owned(),
                        sha256: r.get(0)?,
                        version: r.get(1)?,
                        imported_at: r.get(2)?,
                        found_at: r.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(found)
        })
    }

    /// Queues a stat walk of the Source Set `name`, started by `actor`. Refused before its first Audit, which it
    /// compares with, and while one is Queued or Running. A Repair or Build of it never waits for one.
    pub fn enqueue_stat_walk(&self, name: &str, actor: &str) -> Result<Job> {
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            if latest_audit(tx, set.id)?.is_none() {
                return Err(Error::Refused(format!(
                    "source set {name} has never been audited; there is nothing to compare its files with"
                )));
            }
            check_no_active(tx, set.id, name, JobKind::StatWalk)?;
            tx.execute(
                "INSERT INTO jobs (state, kind, source_set_id, actor, readers, hashers, created_at)
                 VALUES ('queued', 'stat_walk', ?1, ?2, 1, 1, ?3)",
                params![set.id, actor, now()],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Runs the Running stat walk Job `id`: stats every file of its Source Set, reads no payload and writes nothing
    /// there. It stops, recording nothing and ending Interrupted, once a Repair or Build wants the Source Set or the
    /// job slot. Returns the Job as it ends.
    pub fn run_stat_walk_job(&self, id: i64) -> Result<Job> {
        // What it reads and writes is counted for it, even where it isn't handed its meter.
        let _metered = self.meter.job(id).on_this_thread();
        let job = self.job(id)?;
        if job.kind != JobKind::StatWalk {
            return Err(Error::Refused(format!("job {id} is not a stat walk")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let since = job.started_at.clone().unwrap_or_else(now);
        let outcome = self.stat_walk(&job.source_set, &since);
        self.write(|tx| {
            let (state, error) = match &outcome {
                Ok(Some((set, audit, counts))) => {
                    if yield_to(tx, *set, &since)? {
                        (JobState::Interrupted, Some(WALK_YIELDED.to_owned()))
                    } else {
                        record(tx, *set, *audit, id, counts)?;
                        (JobState::Succeeded, None)
                    }
                }
                Ok(None) => (JobState::Interrupted, Some(WALK_YIELDED.to_owned())),
                Err(err) => (JobState::Failed, Some(err.to_string())),
            };
            match finish_running(tx, &self.meter, id, state, error.as_deref()) {
                Err(Error::Io(_)) | Ok(()) => Ok(()),
                Err(err) => Err(err),
            }
        })?;
        self.job(id)
    }

    /// Walks Source Set `name`, returning its id, the Audit it compared with and what it counted; none if it
    /// yielded.
    fn stat_walk(&self, name: &str, since: &str) -> Result<Option<(i64, i64, Counts)>> {
        let roots = &*self.roots();
        let (set, baseline) = self.read(|c| {
            let set = set_by_name(c, name)?;
            let audit = latest_audit(c, set.id)?.ok_or_else(|| {
                Error::Refused(format!("source set {name} has never been audited"))
            })?;
            Ok((set, baseline(c, audit)?))
        })?;
        let resolved = resolve(roots, &set.record.root, &set.record.path)?;
        let polled = Cell::new(Instant::now());
        let stop = Cell::new(false);
        let keep_going = || {
            if !stop.get() && polled.get().elapsed() >= YIELD_POLL {
                polled.set(Instant::now());
                // An error asking counts as a reason to stop: the walk is only ever a hint.
                stop.set(self.read(|c| yield_to(c, set.id, since)).unwrap_or(true));
            }
            !stop.get()
        };
        let Some(mut walked) = stage_select_engine::stat_walk(
            &resolved.path,
            Some(&resolved.canonical_root),
            &keep_going,
        )?
        else {
            return Ok(None);
        };
        // Its Placed Games' files are its own too, where they are placed.
        for (folder, placed, files) in self.placed_folders(set.id)? {
            walked.extend(stage_select_engine::stat_placed(
                &stage_select_engine::Placed {
                    id: folder,
                    root: &placed.canonical_root,
                    folder: &placed.rel,
                    files,
                    pools: None,
                    tally: Default::default(),
                    // A stat walk reads no file.
                    chds: Vec::new(),
                    chd: stage_select_engine::ChdAudit {
                        chdman: Err("a stat walk runs no chdman".into()),
                        limits: self.settings()?.chd_limits(),
                        tmp: self.dir().join(crate::TMP),
                    },
                },
            ));
        }
        Ok(Some((
            set.id,
            baseline.audit,
            Counts::of(&baseline, &walked),
        )))
    }

    /// The stat walks of the Source Set `name` that found files changed, added, removed or unconfirmed, newest first.
    pub fn stat_walks(&self, name: &str) -> Result<Vec<StatWalk>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut rows = c.prepare(
                "SELECT id, audit_id, found_at, checked_at, changed, added, removed, unconfirmed FROM stat_walks
                 WHERE source_set_id = ?1 AND changed + added + removed + unconfirmed > 0 ORDER BY id DESC",
            )?;
            let walks = rows
                .query_map([set.id], |r| {
                    Ok(StatWalk {
                        id: r.get(0)?,
                        source_set: name.to_owned(),
                        audit: r.get(1)?,
                        found_at: r.get(2)?,
                        checked_at: r.get(3)?,
                        changed: r.get::<_, i64>(4)? as u64,
                        added: r.get::<_, i64>(5)? as u64,
                        removed: r.get::<_, i64>(6)? as u64,
                        unconfirmed: r.get::<_, i64>(7)? as u64,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(walks)
        })
    }

    /// Queues a stat walk of every audited Source Set that has none Queued or Running and was last checked, or else
    /// last audited, longer than `every` ago, so a restart doesn't check everything again. A check counts from when
    /// it finished, however long it waited to run. Returns the Jobs queued.
    pub fn enqueue_stat_walks(&self, actor: &str, every: Duration) -> Result<Vec<Job>> {
        let due: Vec<(String, String)> = self.read(|c| {
            let mut rows = c.prepare(
                "SELECT s.name, coalesce(
                     (SELECT max(j.finished_at) FROM jobs j WHERE j.source_set_id = s.id AND j.kind = 'stat_walk'),
                     (SELECT max(w.checked_at) FROM stat_walks w WHERE w.source_set_id = s.id),
                     (SELECT max(a.finished_at) FROM audits a WHERE a.source_set_id = s.id AND a.status = 'finished'))
                 FROM source_sets s
                 WHERE s.unregistered_at IS NULL
                 AND EXISTS (SELECT 1 FROM audits a WHERE a.source_set_id = s.id AND a.status = 'finished')
                 AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.source_set_id = s.id AND j.kind = 'stat_walk'
                                 AND j.state IN ('queued', 'running'))
                 ORDER BY s.name",
            )?;
            Ok(rows
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?)
        })?;
        let now = jiff::Timestamp::now();
        let every = jiff::SignedDuration::try_from(every).unwrap_or(jiff::SignedDuration::MAX);
        let mut queued = Vec::new();
        for (name, last) in due {
            let last = last
                .parse::<jiff::Timestamp>()
                .map_err(|e| Error::Io(format!("source set {name}: {e}")))?;
            if now.duration_since(last) < every {
                continue;
            }
            match self.enqueue_stat_walk(&name, actor) {
                Ok(job) => queued.push(job),
                Err(Error::JobActive { .. } | Error::Refused(_) | Error::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(queued)
    }
}
