//! How far a plan being made has got, counted as it is made (#362). Each item planned is one relaxed atomic add, and
//! only a change of step takes a lock, so counting costs the planner next to nothing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use stage_select_api::planning::{PlanProgress, PlanStep};

/// Counts a plan's progress as it is made; read from another thread while it is.
#[derive(Debug, Default)]
pub struct PlanMeter {
    /// The step under way and its total. Changed once per step.
    step: Mutex<(PlanStep, u64)>,
    /// What the step has done so far.
    done: AtomicU64,
    /// Each step left behind, as it ended.
    ended: Mutex<Vec<PlanProgress>>,
}

impl PlanMeter {
    /// Starts `step`, which will do `total`, as far as is known (0 for a step that doesn't count).
    pub(crate) fn start(&self, step: PlanStep, total: u64) {
        let mut at = self.step.lock().unwrap_or_else(|e| e.into_inner());
        let before = PlanProgress {
            step: at.0,
            done: self.done.swap(0, Ordering::Relaxed),
            total: at.1,
        };
        self.ended
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(before);
        *at = (step, total);
    }

    /// Counts one more done in the step under way.
    pub(crate) fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    /// Sets what the step under way has done to `done`, for a step that counts as it goes.
    pub(crate) fn count(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }

    /// How far the plan has got now. A step that has done more than it expected to shows its total as what it has
    /// done.
    pub fn progress(&self) -> PlanProgress {
        let at = self.step.lock().unwrap_or_else(|e| e.into_inner());
        let done = self.done.load(Ordering::Relaxed);
        PlanProgress {
            step: at.0,
            done,
            total: at.1.max(done),
        }
    }

    /// Each step the plan has left behind, in order, as it ended.
    pub fn ended(&self) -> Vec<PlanProgress> {
        self.ended.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
