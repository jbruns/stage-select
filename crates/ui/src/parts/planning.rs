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

/// What a plan stops being read with once the page that started it is gone. The Service goes on making it.
const LEFT: &str = "You left the page that started this plan; it is still being made.";

/// The plans started in turn, to tell the latest, whose progress shows, from those it superseded. A plan takes its
/// turn when you ask for it, not when the Service starts making it, so one that first waits for a selection never
/// takes over from one asked for after it.
#[derive(Debug, Default)]
struct Turns {
    seq: u64,
}

/// One plan asked for, to check against the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn(u64);

impl Turns {
    /// Starts a turn, superseding every one before.
    fn take(&mut self) -> Turn {
        self.seq += 1;
        Turn(self.seq)
    }

    fn is_latest(&self, turn: Turn) -> bool {
        turn.0 == self.seq
    }
}

/// How far the latest plan of one kind being made has got, once the Service has said. Its progress lives as long as
/// the owner it was made under: a page's stops being read when you leave the page, an app-wide one never does.
#[derive(Clone, Copy)]
pub struct MakingPlan {
    kind: PlanningKind,
    at: RwSignal<Option<PlanProgress>>,
    turns: StoredValue<Turns>,
}

impl MakingPlan {
    pub fn new(kind: PlanningKind) -> Self {
        MakingPlan {
            kind,
            at: RwSignal::new(None),
            turns: StoredValue::new(Turns::default()),
        }
    }

    /// Takes the turn of a plan just asked for, and forgets how far the last one got: what a plan asked for before
    /// says no longer shows. Once the owner is gone the turn is never the latest.
    pub fn take_turn(self) -> Turn {
        let turn = self.turns.try_update_value(Turns::take).unwrap_or(Turn(0));
        self.at.try_set(None);
        turn
    }

    /// Whether `turn` is the latest plan asked for, whose progress shows; None once the owner is gone.
    pub fn is_latest(self, turn: Turn) -> Option<bool> {
        self.turns.try_with_value(|t| t.is_latest(turn))
    }

    /// Shows `progress` of the plan of `turn` if it is the latest; None once the owner is gone.
    fn show(self, turn: Turn, progress: PlanProgress) -> Option<()> {
        if self.is_latest(turn)? {
            self.at.try_set(Some(progress));
        }
        Some(())
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
    /// wasn't. Its progress shows until another plan starts; reading stops once the owner is gone.
    pub async fn make<B: Serialize>(self, start: &str, body: &B) -> Result<i64, String> {
        let turn = self.take_turn();
        self.make_in(turn, start, body).await
    }

    /// [`MakingPlan::make`] for the plan asked for in `turn`, taken when it was asked for.
    pub async fn make_in<B: Serialize>(
        self,
        turn: Turn,
        start: &str,
        body: &B,
    ) -> Result<i64, String> {
        self.is_latest(turn).ok_or(LEFT)?;
        let mut planning: Planning = api::post(start, body).await?;
        loop {
            self.show(turn, planning.progress).ok_or(LEFT)?;
            if let Some(ended) = ended(&planning) {
                if ended.is_ok() {
                    // The plan is recorded only now, not when it was started, so what counts statuses reads again.
                    api::writes().update(|n| *n += 1);
                }
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
    fn only_the_latest_plan_started_shows_its_progress() {
        let mut turns = Turns::default();
        let a = turns.take();
        assert!(turns.is_latest(a));
        let b = turns.take();
        assert!(!turns.is_latest(a), "a plan started before no longer shows");
        assert!(turns.is_latest(b));
    }

    #[test]
    fn a_plan_asked_for_earlier_never_takes_over_once_it_starts_being_made() {
        let owner = Owner::new();
        owner.with(|| {
            let making = MakingPlan::new(PlanningKind::Build);
            let at = |step| PlanProgress {
                step,
                done: 0,
                total: 0,
            };
            // A waits for a selection while B, asked for after it, is made.
            let a = making.take_turn();
            let b = making.take_turn();
            making.show(b, at(PlanStep::Walking)).unwrap();

            // A's selection is ready and it starts being made: it keeps its turn and shows nothing.
            making.show(a, at(PlanStep::Reading)).unwrap();
            assert_eq!(making.at.get_untracked(), Some(at(PlanStep::Walking)));
            assert_eq!(making.is_latest(a), Some(false));
            making.show(b, at(PlanStep::Planning)).unwrap();
            assert_eq!(making.at.get_untracked(), Some(at(PlanStep::Planning)));
        });
    }

    #[test]
    fn a_plan_read_from_a_page_stops_being_read_once_the_page_is_gone() {
        let owner = Owner::new();
        let making = owner.with(|| MakingPlan::new(PlanningKind::Repair));
        assert!(making.turns.try_with_value(|_| ()).is_some());
        drop(owner);
        let turn = making.take_turn();
        assert_eq!(making.is_latest(turn), None);
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
