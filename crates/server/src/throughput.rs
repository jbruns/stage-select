//! Turns the store's throughput meter into rates: sampled a few times a second over a short sliding window, for each
//! Running Job, each Root and in total, with the Service's CPU use, and published on `/events` while Jobs run and
//! until their rates decay, then once at rest.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use title_select_api::settings::Pace;
use title_select_api::{JobEvent, JobLoad, Rates, RootRates, Throughput};
use title_select_engine::meter::{Sample, Window};
use title_select_store::Store;

use crate::jobs::Jobs;

/// How often the meter is sampled and, while Jobs run, rates published.
const SAMPLE_EVERY: Duration = Duration::from_millis(500);
/// The span rates are averaged over.
const WINDOW: Duration = Duration::from_secs(3);

/// The latest rates of each Running Job, for its progress events.
#[derive(Default)]
pub struct Latest(Mutex<HashMap<i64, Rates>>);

impl Latest {
    pub fn of(&self, job: i64) -> Option<Rates> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&job)
            .copied()
    }

    fn set(&self, rates: HashMap<i64, Rates>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = rates;
    }
}

/// Every sliding window, kept from one sample to the next.
#[derive(Default)]
struct Windows {
    total: Option<Window>,
    roots: HashMap<String, Window>,
    jobs: HashMap<i64, Window>,
}

fn window() -> Window {
    Window::new(WINDOW)
}

impl Windows {
    /// Pushes `sample`, taken `at`, and returns the rates it gives; a Job no longer metered is forgotten.
    fn push(&mut self, at: Instant, sample: &Sample) -> (Rates, Vec<RootRates>, Vec<(i64, Rates)>) {
        let total = self.total.get_or_insert_with(window).push(at, sample.total);
        let roots = sample
            .roots
            .iter()
            .map(|(root, counted)| RootRates {
                root: root.clone(),
                rates: self
                    .roots
                    .entry(root.clone())
                    .or_insert_with(window)
                    .push(at, *counted),
            })
            .collect();
        self.jobs
            .retain(|id, _| sample.jobs.iter().any(|j| j.id == *id));
        let jobs = sample
            .jobs
            .iter()
            .map(|j| {
                let rates = self
                    .jobs
                    .entry(j.id)
                    .or_insert_with(window)
                    .push(at, j.counted);
                (j.id, rates)
            })
            .collect();
        (total, roots, jobs)
    }
}

/// Samples the meter of `store` until the Service stops, publishing rates to `jobs` while anything moves.
pub async fn run(store: Arc<Store>, jobs: Arc<Jobs>) {
    let mut publisher = Publisher::default();
    let mut cpu = Cpu::default();
    let mut tick = tokio::time::interval(SAMPLE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let at = Instant::now();
        let sample = store.meter().sample();
        let cpu = cpu.sample(at);
        let (rates, event) = publisher.next(at, &sample, cpu, store.pace());
        jobs.latest.set(rates);
        if let Some(event) = event {
            jobs.publish(JobEvent::Throughput(Box::new(event)));
        }
    }
}

/// Turns each sample into rates and decides what is published: every sample while a Job is metered or any rate has
/// yet to decay to nothing, then one event at rest, then nothing until something moves again.
#[derive(Default)]
struct Publisher {
    windows: Windows,
    /// Whether the last sample was published as moving, so the next at rest is published too.
    moving: bool,
}

impl Publisher {
    /// The rates of each Job `sample` meters, and the event to publish, if any.
    fn next(
        &mut self,
        at: Instant,
        sample: &Sample,
        cpu: Option<f64>,
        pace: Pace,
    ) -> (HashMap<i64, Rates>, Option<Throughput>) {
        let (total, roots, rates) = self.windows.push(at, sample);
        let moving =
            !sample.jobs.is_empty() || moves(&total) || roots.iter().any(|r| moves(&r.rates));
        let latest = rates.iter().copied().collect();
        let was_moving = std::mem::replace(&mut self.moving, moving);
        if !moving {
            // At rest: no rate, no Job, and no CPU use, which would otherwise stay at what it was last.
            let rest = was_moving.then(|| Throughput {
                total: Rates::default(),
                roots: Vec::new(),
                jobs: Vec::new(),
                pace,
                cpu: None,
            });
            return (latest, rest);
        }
        let jobs = sample
            .jobs
            .iter()
            .zip(&rates)
            .map(|(j, (_, rates))| JobLoad {
                job: j.id,
                rates: *rates,
                readers: j.readers,
                hashers: j.hashers,
                allowance: j.allowance.unwrap_or_else(|| pace.workers(j.ceilings)),
                chdman: j.chdman,
            })
            .collect();
        let event = Throughput {
            total,
            roots,
            jobs,
            pace,
            cpu,
        };
        (latest, Some(event))
    }
}

/// Whether `rates` count anything moving.
fn moves(rates: &Rates) -> bool {
    rates.read_bytes_per_s > 0.0 || rates.write_bytes_per_s > 0.0 || rates.files_per_s > 0.0
}

/// The Service's CPU use between two samples, read from `/proc/self/stat`.
#[derive(Default)]
struct Cpu {
    last: Option<(Instant, u64)>,
}

impl Cpu {
    /// The share of the CPUs the Service may use it used since the last sample; none on the first, or where
    /// `/proc/self/stat` can't be read.
    fn sample(&mut self, at: Instant) -> Option<f64> {
        let ticks = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| cpu_ticks(&stat));
        let Some(ticks) = ticks else {
            self.last = None;
            return None;
        };
        let last = self.last.replace((at, ticks))?;
        let cpus = std::thread::available_parallelism().map_or(1, usize::from);
        share(last, (at, ticks), cpus, clock_ticks())
    }
}

/// The clock ticks per second `/proc` counts CPU time in, as `sysconf(_SC_CLK_TCK)` gives it.
fn clock_ticks() -> u64 {
    rustix::param::clock_ticks_per_second()
}

/// The share of `cpus` CPUs used between two readings of CPU ticks, counted `hz` a second.
fn share(
    (from, before): (Instant, u64),
    (to, after): (Instant, u64),
    cpus: usize,
    hz: u64,
) -> Option<f64> {
    let span = to.duration_since(from).as_secs_f64();
    (span > 0.0 && hz > 0)
        .then(|| after.saturating_sub(before) as f64 / hz as f64 / span / cpus as f64)
}

/// The user and system CPU ticks `/proc/self/stat` records: its 14th and 15th fields, counted after the command
/// name, which is in parentheses and may hold spaces and parentheses itself.
fn cpu_ticks(stat: &str) -> Option<u64> {
    let (_, after) = stat.rsplit_once(')')?;
    // `after` starts at the 3rd field, the state.
    let mut fields = after.split_whitespace().skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some(user + system)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(read: u64, jobs: usize) -> Sample {
        let counted = title_select_engine::meter::Counted {
            read,
            ..Default::default()
        };
        Sample {
            total: counted,
            roots: vec![("roms".into(), counted)],
            jobs: (0..jobs)
                .map(|_| title_select_engine::meter::JobSample {
                    id: 1,
                    counted,
                    readers: 1,
                    hashers: 1,
                    chdman: false,
                    ceilings: title_select_engine::ONE_WORKER_EACH,
                    allowance: None,
                })
                .collect(),
        }
    }

    #[test]
    fn rates_are_published_until_they_decay_then_once_at_rest_with_no_cpu_use() {
        let mut publisher = Publisher::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let pace = Pace::default();
        let mut next = |ms, read, jobs| {
            publisher
                .next(at(ms), &sample(read, jobs), Some(0.25), pace)
                .1
        };

        assert_eq!(
            next(0, 0, 0),
            None,
            "nothing moved since the Service started"
        );
        let running = next(500, 1000, 1).expect("a Job runs");
        assert_eq!(running.jobs.len(), 1);
        // The Job ends; what it read is still within the window.
        let decaying = next(1000, 1000, 0).expect("its rates have yet to decay");
        assert!(decaying.total.read_bytes_per_s > 0.0);
        assert!(decaying.jobs.is_empty());
        assert_eq!(decaying.cpu, Some(0.25));
        // Past the window, nothing has moved.
        let rest = next(5000, 1000, 0).expect("one event at rest");
        assert_eq!(rest.total, Rates::default());
        assert!(rest.roots.is_empty() && rest.jobs.is_empty());
        assert_eq!(rest.cpu, None);
        assert_eq!(next(5500, 1000, 0), None, "nothing more while at rest");
        assert!(next(6000, 2000, 0).is_some(), "until something moves again");
    }

    #[test]
    fn cpu_ticks_are_user_and_system_time_even_when_the_command_name_holds_parentheses() {
        let stat = "4242 (title (select) x) S 1 4242 4242 0 -1 4194560 1500 0 0 0 731 269 0 0 20 0 9 0 \
                    123 456 789";
        assert_eq!(cpu_ticks(stat), Some(1000));
        assert_eq!(cpu_ticks("4242 (cut short) S 1 2"), None);
        assert_eq!(cpu_ticks("no parentheses"), None);
    }

    #[test]
    fn cpu_use_is_a_share_of_the_cpus_the_service_may_use() {
        let at = Instant::now();
        let later = at + Duration::from_secs(2);
        // 200 ticks over 2 s is one CPU busy: half of two.
        assert_eq!(share((at, 1000), (later, 1200), 2, 100), Some(0.5));
        assert_eq!(share((at, 1000), (at, 1200), 2, 100), None);
        assert!(clock_ticks() > 0);
    }

    #[test]
    fn cpu_use_is_none_where_proc_isnt_there() {
        let mut cpu = Cpu::default();
        let at = Instant::now();
        let first = cpu.sample(at);
        let second = cpu.sample(at + Duration::from_millis(500));
        assert_eq!(first, None, "the first sample has nothing to compare with");
        match std::path::Path::new("/proc/self/stat").exists() {
            true => assert!(second.is_some()),
            false => assert_eq!(second, None),
        }
    }
}
