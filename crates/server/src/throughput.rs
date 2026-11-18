//! Turns the store's throughput meter into rates: sampled a few times a second over a short sliding window, for each
//! Running Job, each Root and in total, with the Service's CPU use, and published on `/events` while Jobs run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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

/// Samples the meter of `store` until the Service stops, publishing rates to `jobs` while any Job is metered and once
/// more, at rest, after the last ends.
pub async fn run(store: Arc<Store>, jobs: Arc<Jobs>) {
    let mut windows = Windows::default();
    let mut cpu = Cpu::default();
    let mut was_busy = false;
    let mut tick = tokio::time::interval(SAMPLE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let at = Instant::now();
        let sample = store.meter().sample();
        let (total, roots, rates) = windows.push(at, &sample);
        let cpu = cpu.sample(at);
        jobs.latest.set(rates.iter().copied().collect());
        let busy = !sample.jobs.is_empty();
        if !busy && !was_busy {
            continue;
        }
        was_busy = busy;
        let pace = store.pace();
        let loads = sample
            .jobs
            .iter()
            .zip(&rates)
            .map(|(j, (_, rates))| JobLoad {
                job: j.id,
                rates: *rates,
                readers: j.readers,
                hashers: j.hashers,
                allowance: pace.workers(j.ceilings),
                chdman: j.chdman,
            })
            .collect();
        jobs.publish(JobEvent::Throughput(Box::new(Throughput {
            total,
            roots,
            jobs: loads,
            pace,
            cpu,
        })));
    }
}

/// The Service's CPU use between two samples, read from `/proc/self/stat`.
#[derive(Default)]
struct Cpu {
    last: Option<(Instant, u64)>,
}

/// The clock ticks per second `/proc` counts CPU time in: `USER_HZ`, 100 on every Linux architecture.
const USER_HZ: f64 = 100.0;

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
        share(last, (at, ticks), cpus)
    }
}

/// The share of `cpus` CPUs used between two readings of CPU ticks.
fn share((from, before): (Instant, u64), (to, after): (Instant, u64), cpus: usize) -> Option<f64> {
    let span = to.duration_since(from).as_secs_f64();
    (span > 0.0).then(|| after.saturating_sub(before) as f64 / USER_HZ / span / cpus as f64)
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
        assert_eq!(share((at, 1000), (later, 1200), 2), Some(0.5));
        assert_eq!(share((at, 1000), (at, 1200), 2), None);
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
