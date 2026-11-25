//! A repair or build plan being made in the background, and how far it has got (#362).

use serde::{Deserialize, Serialize};

/// What making a plan is doing now, in the order it does it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStep {
    /// Reading what the plan starts from: a repair's Audit report and DAT, a build's Selection and DAT.
    #[default]
    Reading,
    /// Looking at the files there now: a repair's Source Set folder, a build's Library Folder.
    Walking,
    /// Planning each Game: a repair's DAT Games, a build's selected Games' files.
    Planning,
    /// Settling what the plan does as a whole, and recording it.
    Finishing,
}

impl PlanStep {
    /// Where the step starts and ends on the way to a whole plan, in percent. Planning each Game takes most of the
    /// time (#361), so it takes most of the way.
    fn span(self) -> (u64, u64) {
        match self {
            PlanStep::Reading => (0, 5),
            PlanStep::Walking => (5, 20),
            PlanStep::Planning => (20, 95),
            PlanStep::Finishing => (95, 100),
        }
    }
}

/// How far a plan being made has got: its step, and how much of the step is done, when the step counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanProgress {
    pub step: PlanStep,
    /// What the step has done: files walked, Games or files planned.
    pub done: u64,
    /// What the step will do, as far as is known; 0 for a step that doesn't count.
    pub total: u64,
}

impl PlanProgress {
    /// How far the plan has got as a whole, in percent: the step's share of the way, filled as far as it has done.
    /// It never goes back while the steps go forward and each step's count rises.
    pub fn percent(&self) -> u8 {
        let (from, to) = self.step.span();
        let within = match self.total {
            0 => 0,
            // Widened, so that a large count can't overflow the product.
            total => ((to - from) as u128 * self.done.min(total) as u128 / total as u128) as u64,
        };
        (from + within) as u8
    }
}

/// What kind of plan is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningKind {
    /// A repair plan, read at `/repairs/{plan}` once made.
    Repair,
    /// A build plan, read at `/plans/{plan}` once made.
    Build,
}

/// A plan being made in the background, or made: `POST /source-sets/{name}/repair-plannings` and
/// `POST /libraries/{id}/plannings` start one, and `GET /plannings/{id}` reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planning {
    pub id: u64,
    pub kind: PlanningKind,
    /// The Source Set a repair plan is of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_set: Option<String>,
    /// The Library Folder a build plan builds into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_folder: Option<i64>,
    pub progress: PlanProgress,
    /// The plan made, once it is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<i64>,
    /// Why no plan was made, once making it failed: what the synchronous request would have answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Planning {
    /// Whether making the plan has ended, made or failed.
    pub fn ended(&self) -> bool {
        self.plan.is_some() || self.error.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(step: PlanStep, done: u64, total: u64) -> u8 {
        PlanProgress { step, done, total }.percent()
    }

    #[test]
    fn the_percentage_rises_through_each_step_in_turn() {
        assert_eq!(at(PlanStep::Reading, 0, 0), 0);
        assert_eq!(at(PlanStep::Walking, 0, 1_000), 5);
        assert_eq!(at(PlanStep::Walking, 500, 1_000), 12);
        assert_eq!(at(PlanStep::Walking, 1_000, 1_000), 20);
        assert_eq!(at(PlanStep::Planning, 0, 14_000), 20);
        assert_eq!(at(PlanStep::Planning, 7_000, 14_000), 57);
        assert_eq!(at(PlanStep::Planning, 14_000, 14_000), 95);
        assert_eq!(at(PlanStep::Finishing, 0, 0), 95);
    }

    #[test]
    fn a_count_past_its_estimated_total_stays_within_its_step() {
        // A repair estimates the files it walks from its Audit, and the folder may hold more now.
        assert_eq!(at(PlanStep::Walking, 1_500, 1_000), 20);
        assert_eq!(at(PlanStep::Walking, 3, 0), 5);
        assert_eq!(at(PlanStep::Planning, u64::MAX, u64::MAX), 95);
    }
}
