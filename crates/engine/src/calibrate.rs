//! Calibration's sweeps (ADR 0022): how hash throughput on the Host, and read throughput from a Root, grow with the
//! number of workers, and the knee where adding more stops paying.
//!
//! A sweep runs steps of 1, 2, 4… workers. Each step stops at a time limit or a byte limit, whichever comes first, and
//! records the throughput it reached. The knee is the smallest count within 10% of the best throughput, never one past
//! a drop. A Root's sweep stops at a drop: more readers than that only contend.
//!
//! Every read is read-only, and each file is read at most once in a run, then dropped from the page cache so that
//! no later step reads it from memory.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use title_select_api::calibration::{CurvePoint, Knee};

use crate::meter::{JobMeter, Reading, Tally};
use crate::payload::Hasher;

/// The most payload readers a Root's sweep tries.
pub const MAX_READERS: usize = 32;

/// The throughput within which a smaller count is as good as the best: 10%.
const KNEE_SLACK: f64 = 0.9;

const CHUNK: usize = 1 << 16;

/// The synthetic payload each hash worker checksums over and over.
const HOST_PAYLOAD: usize = 16 << 20;

/// When each step stops, and how much a Root needs for a measurement worth having.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub step_time: Duration,
    pub step_bytes: u64,
    /// Less than this to read and a Root is too small to measure; a step that ran out of files before reading a
    /// quarter of it measured too little to keep.
    pub min_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            step_time: Duration::from_secs(10),
            step_bytes: 2 << 30,
            min_bytes: 1 << 30,
        }
    }
}

impl Limits {
    /// The most a sweep of `steps` steps can read: what's worth finding in a Root before reading it.
    pub fn sweep_bytes(&self, steps: usize) -> u64 {
        self.step_bytes.saturating_mul(steps as u64)
    }
}

/// The time a step takes, measured from a fixed start. Tests drive a fake one from a throughput model.
pub trait Clock: Sync {
    fn elapsed(&self) -> Duration;
}

impl Clock for Instant {
    fn elapsed(&self) -> Duration {
        Instant::elapsed(self)
    }
}

/// The worker counts a sweep up to `max` tries: 1, 2, 4… and `max` itself.
pub fn counts(max: usize) -> Vec<usize> {
    let max = max.max(1);
    let mut counts: Vec<usize> = std::iter::successors(Some(1usize), |n| n.checked_mul(2))
        .take_while(|&n| n < max)
        .collect();
    counts.push(max);
    counts
}

/// Why a step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepEnd {
    Time,
    Bytes,
    /// Its workers ran out of files to read.
    Exhausted,
    Cancelled,
}

/// What a step's workers share: the bytes they've got through, and whether to go on.
pub struct Budget<'a> {
    limits: Limits,
    clock: &'a dyn Clock,
    start: Duration,
    bytes: AtomicU64,
    cancel: &'a AtomicBool,
}

impl Budget<'_> {
    /// Whether the workers should take on more: not cancelled, and neither limit reached.
    pub fn going(&self) -> bool {
        !self.cancel.load(Ordering::Relaxed)
            && self.bytes.load(Ordering::Relaxed) < self.limits.step_bytes
            && self.clock.elapsed().saturating_sub(self.start) < self.limits.step_time
    }

    /// Counts `n` bytes got through.
    pub fn add(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }
}

/// One measured step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub point: CurvePoint,
    pub end: StepEnd,
}

/// Runs `workers` threads of `work` together until the step's limits, a cancel, or the work runs out, and measures
/// their throughput. Each `work` call takes on work while [`Budget::going`] says so, counting it with
/// [`Budget::add`], and returns when it stops or has nothing left.
pub fn step(
    workers: usize,
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    work: &(dyn Fn(&Budget) + Sync),
) -> Step {
    let budget = Budget {
        limits,
        clock,
        start: clock.elapsed(),
        bytes: AtomicU64::new(0),
        cancel,
    };
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| work(&budget));
        }
    });
    let took = clock.elapsed().saturating_sub(budget.start);
    let bytes = budget.bytes.load(Ordering::Relaxed);
    let end = if cancel.load(Ordering::Relaxed) {
        StepEnd::Cancelled
    } else if bytes >= limits.step_bytes {
        StepEnd::Bytes
    } else if took >= limits.step_time {
        StepEnd::Time
    } else {
        StepEnd::Exhausted
    };
    Step {
        point: CurvePoint {
            workers,
            bytes,
            duration_ms: took.as_millis() as u64,
            bytes_per_s: bytes as f64 / took.as_secs_f64().max(1e-9),
        },
        end,
    }
}

/// A sweep's curve and knee, or that it was cancelled before it finished.
#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    pub curve: Vec<CurvePoint>,
    pub knee: Option<Knee>,
    pub cancelled: bool,
}

/// Runs `step` for each count in turn. It stops at a cancel, when the work runs out, and, when `stop_at_drop`, at the
/// first step slower than the one before. A step that ran out of work before reading `keep_short` bytes measured too
/// little and is left off the curve.
pub fn sweep(
    counts: &[usize],
    stop_at_drop: bool,
    keep_short: u64,
    mut step: impl FnMut(usize) -> Step,
) -> Sweep {
    let mut curve: Vec<CurvePoint> = Vec::new();
    let mut cancelled = false;
    for &count in counts {
        let Step { point, end } = step(count);
        match end {
            StepEnd::Cancelled => {
                cancelled = true;
                break;
            }
            StepEnd::Exhausted => {
                if point.bytes >= keep_short && point.bytes > 0 {
                    curve.push(point);
                }
                break;
            }
            StepEnd::Time | StepEnd::Bytes => {
                let dropped = curve
                    .last()
                    .is_some_and(|last| point.bytes_per_s < last.bytes_per_s);
                curve.push(point);
                if dropped && stop_at_drop {
                    break;
                }
            }
        }
    }
    Sweep {
        knee: knee(&curve),
        curve,
        cancelled,
    }
}

/// The smallest count within 10% of the best throughput, looking no further than the step before the first drop.
pub fn knee(curve: &[CurvePoint]) -> Option<Knee> {
    let before_drop = curve
        .windows(2)
        .position(|w| w[1].bytes_per_s < w[0].bytes_per_s)
        .map_or(curve.len(), |i| i + 1);
    let curve = &curve[..before_drop];
    let best = curve.iter().map(|p| p.bytes_per_s).fold(0.0, f64::max);
    curve
        .iter()
        .find(|p| p.bytes_per_s >= best * KNEE_SLACK)
        .map(|p| Knee {
            workers: p.workers,
            bytes_per_s: p.bytes_per_s,
        })
}

/// Sweeps hash workers 1, 2, 4… up to `cpus`, each checksumming synthetic data in memory with the Audit's hasher.
pub fn host(
    cpus: usize,
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    job: Option<&JobMeter>,
) -> Sweep {
    let payload = synthetic(HOST_PAYLOAD);
    let work = |budget: &Budget| {
        let _active = job.map(JobMeter::hashing);
        while budget.going() {
            let mut hasher = Hasher::new();
            for chunk in payload.chunks(CHUNK) {
                if !budget.going() {
                    break;
                }
                hasher.update(chunk);
                budget.add(chunk.len() as u64);
            }
            std::hint::black_box(hasher.finish());
        }
    };
    sweep(&counts(cpus), false, 0, |n| {
        step(n, limits, clock, cancel, &work)
    })
}

/// Bytes that don't compress or repeat within a chunk, so that hashing them costs what hashing a real payload does.
fn synthetic(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// A file a Root's sweep may read, and its size when found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
}

/// The regular files among `paths`, in order, until their sizes add up to `enough`. A path that can't be statted, or
/// isn't a regular file (a symlink included), is passed over.
pub fn candidates(paths: impl IntoIterator<Item = PathBuf>, enough: u64) -> Vec<Candidate> {
    let mut found = Vec::new();
    let mut total = 0u64;
    for path in paths {
        if total >= enough {
            break;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 {
            continue;
        }
        total += meta.len();
        found.push(Candidate {
            path,
            size: meta.len(),
        });
    }
    found
}

/// The files of a Root with no Audits, walked in name order without following links and passing over Title Select's
/// own entries, until their sizes add up to `enough`. Errors only when the Root itself can't be read.
pub fn walk(root: &Path, enough: u64) -> std::io::Result<Vec<Candidate>> {
    std::fs::read_dir(root)?;
    let mut paths = walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    let mut files = Vec::new();
    let mut total = 0u64;
    while total < enough {
        let Some(item) = paths.next() else { break };
        let Ok(item) = item else { continue };
        if item.depth() > 0 && crate::is_own_entry(&item.file_name().to_string_lossy()) {
            if item.file_type().is_dir() {
                paths.skip_current_dir();
            }
            continue;
        }
        if !item.file_type().is_file() {
            continue;
        }
        let Ok(meta) = item.metadata() else { continue };
        if meta.len() == 0 {
            continue;
        }
        total += meta.len();
        files.push(Candidate {
            path: item.into_path(),
            size: meta.len(),
        });
    }
    Ok(files)
}

/// How a Root's sweep went.
#[derive(Debug, Clone, PartialEq)]
pub enum RootSweep {
    /// Its files add up to less than [`Limits::min_bytes`].
    TooSmall,
    Swept(Sweep),
}

/// Sweeps payload readers 1, 2, 4… up to [`MAX_READERS`] over `files`, read-only, in order. Each file is read at most
/// once across the sweep, so each step reads files no earlier step read, and each is dropped from the page cache
/// before and after. A file that can't be opened or read is passed over.
pub fn root(
    files: &[Candidate],
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    tally: &Tally,
) -> RootSweep {
    let available: u64 = files.iter().map(|f| f.size).sum();
    if available < limits.min_bytes {
        return RootSweep::TooSmall;
    }
    let next = AtomicUsize::new(0);
    let work = |budget: &Budget| {
        let _active = tally.job().map(JobMeter::reading);
        let mut buf = vec![0; CHUNK];
        while budget.going() {
            let Some(file) = files.get(next.fetch_add(1, Ordering::Relaxed)) else {
                return;
            };
            let Ok(opened) = File::open(&file.path) else {
                continue;
            };
            uncache(&opened);
            let mut reading = Reading::new(&opened, tally);
            while budget.going() {
                match reading.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => budget.add(n as u64),
                }
            }
            tally.file_read();
            uncache(&opened);
        }
    };
    RootSweep::Swept(sweep(
        &counts(MAX_READERS),
        true,
        limits.min_bytes / 4,
        |n| step(n, limits, clock, cancel, &work),
    ))
}

/// Drops `file`'s pages from the page cache, so that what was read isn't read again from memory, nor was read from
/// memory in the first place. Where the platform has no such advice, it does nothing.
fn uncache(file: &File) {
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(file, 0, None, rustix::fs::Advice::DontNeed);
    #[cfg(not(target_os = "linux"))]
    let _ = file;
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    /// A clock that only moves when the fake work says so.
    #[derive(Default)]
    struct Fake(AtomicU64);

    impl Fake {
        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_nanos() as u64, Ordering::Relaxed);
        }
    }

    impl Clock for Fake {
        fn elapsed(&self) -> Duration {
            Duration::from_nanos(self.0.load(Ordering::Relaxed))
        }
    }

    /// Work whose `workers` together get through `rate(workers)` bytes a second, a MiB at a time.
    fn modelled<'a>(
        clock: &'a Fake,
        workers: usize,
        rate: &'a (dyn Fn(usize) -> f64 + Sync),
    ) -> impl Fn(&Budget) + Sync + 'a {
        move |budget: &Budget| {
            while budget.going() {
                clock.advance(Duration::from_secs_f64(MIB as f64 / rate(workers)));
                budget.add(MIB);
            }
        }
    }

    fn point(workers: usize, mib_per_s: f64) -> CurvePoint {
        CurvePoint {
            workers,
            bytes: 0,
            duration_ms: 0,
            bytes_per_s: mib_per_s,
        }
    }

    fn timed(point: CurvePoint) -> Step {
        Step {
            point,
            end: StepEnd::Time,
        }
    }

    #[test]
    fn counts_double_up_to_the_most() {
        assert_eq!(counts(32), [1, 2, 4, 8, 16, 32]);
        assert_eq!(counts(12), [1, 2, 4, 8, 12]);
        assert_eq!(counts(1), [1]);
        assert_eq!(counts(0), [1]);
    }

    #[test]
    fn the_knee_is_the_smallest_count_within_ten_percent_of_the_best() {
        let curve = [
            point(1, 100.0),
            point(2, 180.0),
            point(4, 300.0),
            point(8, 320.0),
            point(16, 330.0),
        ];
        let knee = knee(&curve).unwrap();
        assert_eq!(knee.workers, 4, "300 is within 10% of 330; 180 isn't");
        assert_eq!(knee.bytes_per_s, 300.0);
    }

    #[test]
    fn the_knee_is_never_one_past_a_drop() {
        // The best is after the drop, but a count past a drop is never the knee.
        let curve = [
            point(1, 100.0),
            point(2, 200.0),
            point(4, 150.0),
            point(8, 400.0),
        ];
        assert_eq!(knee(&curve).unwrap().workers, 2);
        assert_eq!(knee(&[]), None);
    }

    #[test]
    fn a_root_sweep_stops_at_a_drop_and_the_host_sweep_does_not() {
        let model =
            |n: usize| [100.0, 200.0, 150.0, 400.0, 500.0, 600.0][n.trailing_zeros() as usize];
        let mut tried = Vec::new();
        let swept = sweep(&counts(32), true, 0, |n| {
            tried.push(n);
            timed(point(n, model(n)))
        });
        assert_eq!(tried, [1, 2, 4], "the step that dropped is the last");
        assert_eq!(swept.curve.len(), 3);
        assert_eq!(swept.knee.unwrap().workers, 2);
        assert!(!swept.cancelled);

        let swept = sweep(&counts(32), false, 0, |n| timed(point(n, model(n))));
        assert_eq!(swept.curve.len(), 6);
        assert_eq!(swept.knee.unwrap().workers, 2);
    }

    #[test]
    fn a_step_stops_at_its_time_limit_when_that_comes_first() {
        let clock = Fake::default();
        let rate = |_| 100.0 * MIB as f64;
        let step = step(
            1,
            Limits::default(),
            &clock,
            &AtomicBool::new(false),
            &modelled(&clock, 1, &rate),
        );
        assert_eq!(step.end, StepEnd::Time);
        assert_eq!(step.point.bytes, 1000 * MIB, "10 s at 100 MiB/s");
        assert_eq!(step.point.duration_ms, 10_000);
        assert!((step.point.bytes_per_s - 100.0 * MIB as f64).abs() < 1.0);
    }

    #[test]
    fn a_step_stops_at_its_byte_limit_when_that_comes_first() {
        let clock = Fake::default();
        let rate = |workers: usize| workers as f64 * 1024.0 * MIB as f64;
        let step = step(
            4,
            Limits::default(),
            &clock,
            &AtomicBool::new(false),
            &modelled(&clock, 4, &rate),
        );
        assert_eq!(step.end, StepEnd::Bytes);
        // Each worker may take one more MiB between the check and the count.
        assert!((2048 * MIB..2048 * MIB + 4 * MIB).contains(&step.point.bytes));
        assert!(step.point.duration_ms <= 1000, "2 GiB at 4 GiB/s");
    }

    #[test]
    fn a_cancelled_step_ends_the_sweep_with_what_it_finished() {
        let cancel = AtomicBool::new(false);
        let swept = sweep(&counts(8), false, 0, |n| {
            if n == 4 {
                cancel.store(true, Ordering::Relaxed);
            }
            let clock = Fake::default();
            let rate = |n: usize| n as f64 * MIB as f64;
            step(
                n,
                Limits::default(),
                &clock,
                &cancel,
                &modelled(&clock, n, &rate),
            )
        });
        assert!(swept.cancelled);
        assert_eq!(
            swept.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn a_short_step_that_ran_out_of_files_is_left_off() {
        let swept = sweep(&counts(8), true, 10, |n| Step {
            point: CurvePoint {
                bytes: if n == 1 { 100 } else { 5 },
                ..point(n, n as f64)
            },
            end: if n == 1 {
                StepEnd::Bytes
            } else {
                StepEnd::Exhausted
            },
        });
        assert_eq!(swept.curve.len(), 1);
        assert!(!swept.cancelled);
    }

    fn files(dir: &Path, sizes: &[usize]) -> Vec<Candidate> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                let path = dir.join(format!("{i:03}.bin"));
                std::fs::write(&path, vec![i as u8; size]).unwrap();
                Candidate {
                    path,
                    size: size as u64,
                }
            })
            .collect()
    }

    #[test]
    fn a_root_with_less_than_the_minimum_is_too_small_to_measure() {
        let dir = tempfile::tempdir().unwrap();
        let files = files(dir.path(), &[1000, 1000]);
        let limits = Limits {
            min_bytes: 2001,
            ..Limits::default()
        };
        let swept = root(
            &files,
            limits,
            &Instant::now(),
            &AtomicBool::new(false),
            &Tally::default(),
        );
        assert_eq!(swept, RootSweep::TooSmall);
    }

    #[test]
    fn a_root_sweep_reads_each_file_at_most_once() {
        let dir = tempfile::tempdir().unwrap();
        let files = files(dir.path(), &[4096; 64]);
        let limits = Limits {
            step_time: Duration::from_secs(60),
            step_bytes: 8 * 4096,
            min_bytes: 4096,
        };
        let RootSweep::Swept(swept) = root(
            &files,
            limits,
            &Instant::now(),
            &AtomicBool::new(false),
            &Tally::default(),
        ) else {
            panic!("big enough");
        };
        let read: u64 = swept.curve.iter().map(|p| p.bytes).sum();
        assert!(
            read <= 64 * 4096,
            "read {read} bytes of {}: a file read twice",
            64 * 4096
        );
    }

    #[test]
    fn candidates_keep_order_and_stop_once_there_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        let made = files(dir.path(), &[10, 0, 10, 10]);
        let mut paths: Vec<PathBuf> = made.iter().map(|c| c.path.clone()).collect();
        paths.insert(0, dir.path().join("missing"));
        let found = candidates(paths, 20);
        assert_eq!(
            found.iter().map(|c| &c.path).collect::<Vec<_>>(),
            [&made[0].path, &made[2].path],
            "missing and empty files are passed over"
        );
    }

    #[test]
    fn a_walk_passes_over_own_entries_and_stops_once_there_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/.title-select")).unwrap();
        std::fs::write(dir.path().join("a/.title-select/kept"), [0; 100]).unwrap();
        std::fs::write(dir.path().join("a/1.zip"), [0; 10]).unwrap();
        std::fs::write(dir.path().join("b.zip"), [0; 10]).unwrap();
        std::fs::write(dir.path().join("c.zip"), [0; 10]).unwrap();
        let walked = walk(dir.path(), 20).unwrap();
        assert_eq!(
            walked.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
            [dir.path().join("a/1.zip"), dir.path().join("b.zip")]
        );
        assert!(walk(&dir.path().join("missing"), 20).is_err());
    }

    #[test]
    fn the_host_sweep_measures_every_count_up_to_the_cpus() {
        let limits = Limits {
            step_time: Duration::from_secs(5),
            step_bytes: 4 * MIB,
            min_bytes: 0,
        };
        let swept = host(3, limits, &Instant::now(), &AtomicBool::new(false), None);
        assert_eq!(
            swept.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(swept.curve.iter().all(|p| p.bytes >= 4 * MIB));
        assert!(swept.knee.is_some());
    }
}
