//! Plans made in the background: started, then read every so often until made, with how far each has got. A plan of a
//! large source set or library folder takes a while, so its pending status names the step under way and the meter
//! beside it fills as it goes.

use leptos::prelude::*;
use serde::Serialize;
use stage_select_api::planning::{PlanProgress, PlanStep, Planning, PlanningKind};

use crate::api;

/// How often a plan being made is read again, in milliseconds.
const POLL_MS: i32 = 300;

/// What a plan of `kind` is doing at `step`, for its pending status.
pub fn step_status(kind: PlanningKind, step: PlanStep) -> &'static str {
    match (kind, step) {
        (PlanningKind::Repair, PlanStep::Reading) => "Reading the latest audit…",
        (PlanningKind::Build, PlanStep::Reading) => "Reading the selection…",
        (PlanningKind::Repair, PlanStep::Walking) => "Looking at the files in the source set…",
        (PlanningKind::Build, PlanStep::Walking) => "Looking at the files in the library folder…",
        (PlanningKind::Repair, PlanStep::Planning) => "Planning each game…",
        (PlanningKind::Build, PlanStep::Planning) => "Planning each selected game's files…",
        (_, PlanStep::Finishing) => "Finishing the plan…",
    }
}

/// How far a plan of one kind being made has got, once the Service has said.
#[derive(Clone, Copy)]
pub struct MakingPlan {
    kind: PlanningKind,
    at: RwSignal<Option<PlanProgress>>,
}

impl MakingPlan {
    pub fn new(kind: PlanningKind) -> Self {
        MakingPlan {
            kind,
            at: RwSignal::new(None),
        }
    }

    /// Forgets how far the last plan got, for one about to start.
    pub fn forget(self) {
        self.at.set(None);
    }

    /// The step under way, or `before` until the Service has said.
    pub fn status(self, before: impl Into<Signal<String>>) -> Signal<String> {
        let before = before.into();
        Signal::derive(move || match self.at.get() {
            Some(at) => step_status(self.kind, at.step).to_owned(),
            None => before.get(),
        })
    }

    /// How far the plan has got as a whole, in percent, once the Service has said.
    pub fn percent(self) -> Signal<Option<u8>> {
        Signal::derive(move || self.at.get().map(|at| at.percent()))
    }

    /// Starts making a plan at `start` from `body`, and reads how far it has got until it is made: its id, or why it
    /// wasn't.
    pub async fn make<B: Serialize>(self, start: &str, body: &B) -> Result<i64, String> {
        self.forget();
        let mut planning: Planning = api::post(start, body).await?;
        loop {
            self.at.set(Some(planning.progress));
            if let Some(ended) = ended(&planning) {
                return ended;
            }
            api::sleep(POLL_MS).await;
            planning = api::get(&format!("/plannings/{}", planning.id)).await?;
        }
    }
}

/// The plan made, or why it wasn't, once making it has ended.
fn ended(planning: &Planning) -> Option<Result<i64, String>> {
    match (planning.plan, &planning.error) {
        (Some(plan), _) => Some(Ok(plan)),
        (None, Some(error)) => Some(Err(error.clone())),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_step_names_what_it_looks_at_in_ordinary_words() {
        for kind in [PlanningKind::Repair, PlanningKind::Build] {
            for step in [
                PlanStep::Reading,
                PlanStep::Walking,
                PlanStep::Planning,
                PlanStep::Finishing,
            ] {
                let status = step_status(kind, step);
                assert!(status.ends_with('…'), "{status}");
                assert!(!status.to_lowercase().contains("retool"), "{status}");
                assert!(!status.contains("Source Set") && !status.contains("Library Folder"));
            }
        }
        assert_eq!(
            step_status(PlanningKind::Build, PlanStep::Walking),
            "Looking at the files in the library folder…"
        );
    }

    #[test]
    fn a_plan_ends_once_made_or_failed() {
        let mut planning = Planning {
            id: 1,
            kind: PlanningKind::Repair,
            source_set: Some("Alpha".into()),
            library_folder: None,
            progress: PlanProgress::default(),
            plan: None,
            error: None,
        };
        assert_eq!(ended(&planning), None);
        planning.error = Some("no audit".into());
        assert_eq!(ended(&planning), Some(Err("no audit".into())));
        planning.plan = Some(4);
        assert_eq!(ended(&planning), Some(Ok(4)));
    }
}
