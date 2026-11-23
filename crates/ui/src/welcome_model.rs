//! The first-run steps, from an empty store to a first Audit and a Library Folder: which are done, which can be done
//! now, and what each waits on. Every step is worked out from what the store holds, so none is remembered: once a
//! Root exists the home screen no longer leads here.

use stage_select_api::fetch::{FetchProviderStatus, FetchStatus};
use stage_select_api::roots::RootKind;
use stage_select_api::{Root, SourceSetSummary};

use crate::registration::holds_source_sets;

/// A first-run step, in the order you take them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Root,
    Dat,
    SourceSet,
    Audit,
    Library,
}

impl Step {
    pub const ALL: [Step; 5] = [
        Step::Root,
        Step::Dat,
        Step::SourceSet,
        Step::Audit,
        Step::Library,
    ];

    /// The step's heading: the action it asks of you.
    pub fn title(self) -> &'static str {
        match self {
            Step::Root => "Register a root",
            Step::Dat => "Import a DAT",
            Step::SourceSet => "Register a source set",
            Step::Audit => "Audit a source set",
            Step::Library => "Make a library folder",
        }
    }

    /// Where you can take the step later, once you've skipped it.
    pub fn later(self) -> &'static str {
        match self {
            Step::Root => "Settings, then Roots",
            Step::Dat => "DATs",
            Step::SourceSet | Step::Audit => "Source sets",
            Step::Library => "Library folders",
        }
    }
}

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Done,
    /// You can take it now.
    Ready,
    /// It needs an earlier step first; the reason says which.
    Waiting(&'static str),
}

/// What the store holds that the steps depend on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    pub roots: usize,
    /// Read-only general Roots: where Source Sets are registered.
    pub read_only_roots: usize,
    /// Writable Roots but Ingest Folders: where Library Folders are made.
    pub writable_roots: usize,
    pub dats: usize,
    pub source_sets: usize,
    /// Source Sets with a finished Audit.
    pub audited: usize,
    pub libraries: usize,
}

impl Facts {
    pub fn of(roots: &[Root], dats: usize, sets: &[SourceSetSummary], libraries: usize) -> Facts {
        Facts {
            roots: roots.len(),
            read_only_roots: roots.iter().filter(|r| holds_source_sets(r)).count(),
            writable_roots: roots
                .iter()
                .filter(|r| r.writable && r.kind != RootKind::Ingest)
                .count(),
            dats,
            source_sets: sets.len(),
            audited: sets.iter().filter(|s| s.latest.is_some()).count(),
            libraries,
        }
    }

    pub fn state(&self, step: Step) -> State {
        let done = match step {
            Step::Root => self.roots > 0,
            Step::Dat => self.dats > 0,
            Step::SourceSet => self.source_sets > 0,
            Step::Audit => self.audited > 0,
            Step::Library => self.libraries > 0,
        };
        if done {
            return State::Done;
        }
        let waiting = match step {
            Step::Root | Step::Dat => None,
            Step::SourceSet if self.read_only_roots == 0 => {
                Some("Register a read-only root first.")
            }
            Step::SourceSet if self.dats == 0 => Some("Import a DAT first."),
            Step::SourceSet => None,
            Step::Audit if self.source_sets == 0 => Some("Register a source set first."),
            Step::Audit => None,
            Step::Library if self.writable_roots == 0 => Some("Register a writable root first."),
            Step::Library if self.source_sets == 0 => Some("Register a source set first."),
            Step::Library => None,
        };
        waiting.map_or(State::Ready, State::Waiting)
    }
}

/// What the page shows of its loads of what the store holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadState {
    /// A load is under way and none has worked yet.
    pub pending: bool,
    /// Why the latest load failed, once no load is under way; the page offers to try again.
    pub problem: Option<String>,
    /// Whether the steps show: from the first load that worked, kept through a reload that fails.
    pub steps: bool,
}

impl LoadState {
    /// `in_flight` while the latest load asked for hasn't come back; `latest_error` is the error of the latest that
    /// did, if it failed.
    pub fn of(in_flight: bool, loaded_once: bool, latest_error: Option<&str>) -> LoadState {
        LoadState {
            pending: in_flight && !loaded_once,
            problem: latest_error.filter(|_| !in_flight).map(str::to_owned),
            steps: loaded_once,
        }
    }
}

/// What fetching can do for the DAT step, besides importing a DAT, which always works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetching {
    /// No Source Set follows a DAT a DAT Provider fetches, so fetching has nothing to fetch: on an empty store, and
    /// until fetching's status loads. Your first DAT is one you import.
    NothingToFetch,
    /// A Source Set follows a DAT a DAT Provider fetches, but fetching from it is off: turn it on under Operations.
    Off,
    /// Fetching is on for a DAT a Source Set follows: the DATs screen shows where it stands and fetches now.
    On,
}

/// What fetching can do, from where it stands; `None` while that hasn't loaded.
pub fn fetching(status: Option<&FetchStatus>) -> Fetching {
    let fetchable: Vec<&FetchProviderStatus> = status
        .iter()
        .flat_map(|s| &s.providers)
        .filter(|p| {
            p.systems
                .iter()
                .any(|s| s.system.is_some() && !s.source_sets.is_empty())
        })
        .collect();
    if fetchable.is_empty() {
        Fetching::NothingToFetch
    } else if fetchable.iter().any(|p| p.enabled) {
        Fetching::On
    } else {
        Fetching::Off
    }
}

/// A name for a Root at `path`: its last folder's name.
pub fn root_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use stage_select_api::fetch::{FetchProvider, FetchSystem};

    use super::*;

    fn root(name: &str, kind: RootKind, writable: bool) -> Root {
        Root {
            name: name.into(),
            path: format!("/mnt/{name}"),
            kind,
            writable,
            writable_paths: Vec::new(),
            readers: None,
            read_mib_per_s: None,
            write_mib_per_s: None,
            updated_by: "Operator token".into(),
            updated_at: "2026-11-22T10:00:00Z".into(),
        }
    }

    fn states(facts: Facts) -> Vec<State> {
        Step::ALL.iter().map(|&s| facts.state(s)).collect()
    }

    #[test]
    fn from_an_empty_store_only_a_root_and_a_dat_can_be_done_first() {
        let facts = Facts::of(&[], 0, &[], 0);
        assert_eq!(
            states(facts),
            [
                State::Ready,
                State::Ready,
                State::Waiting("Register a read-only root first."),
                State::Waiting("Register a source set first."),
                State::Waiting("Register a writable root first."),
            ]
        );
    }

    #[test]
    fn a_source_set_needs_a_read_only_general_root_and_a_library_folder_a_writable_one_but_an_ingest_folder()
     {
        let ingest = [root("in", RootKind::Ingest, true)];
        let facts = Facts::of(&ingest, 1, &[], 0);
        assert_eq!(facts.state(Step::Root), State::Done);
        assert_eq!(
            facts.state(Step::SourceSet),
            State::Waiting("Register a read-only root first.")
        );
        assert_eq!(
            facts.state(Step::Library),
            State::Waiting("Register a writable root first.")
        );

        let both = [
            root("sets", RootKind::General, false),
            root("play", RootKind::General, true),
        ];
        let facts = Facts::of(&both, 0, &[], 0);
        assert_eq!(
            facts.state(Step::SourceSet),
            State::Waiting("Import a DAT first.")
        );
        assert_eq!(
            facts.state(Step::Library),
            State::Waiting("Register a source set first.")
        );
        let facts = Facts {
            dats: 1,
            source_sets: 1,
            ..facts
        };
        assert_eq!(
            states(facts),
            [
                State::Done,
                State::Done,
                State::Done,
                State::Ready,
                State::Ready
            ]
        );
        assert_eq!(
            Facts {
                audited: 1,
                libraries: 1,
                ..facts
            }
            .state(Step::Audit),
            State::Done
        );
    }

    #[test]
    fn a_first_load_that_fails_shows_its_problem_instead_of_loading_for_ever() {
        let state = |pending, problem: Option<&str>, steps| LoadState {
            pending,
            problem: problem.map(str::to_owned),
            steps,
        };
        assert_eq!(LoadState::of(true, false, None), state(true, None, false));
        assert_eq!(
            LoadState::of(false, false, Some("HTTP 500")),
            state(false, Some("HTTP 500"), false)
        );
        // Trying again: the old problem gives way to the load under way.
        assert_eq!(
            LoadState::of(true, false, Some("HTTP 500")),
            state(true, None, false)
        );
        // A reload that fails keeps the steps, under its problem, and one under way doesn't dim them.
        assert_eq!(
            LoadState::of(false, true, Some("HTTP 500")),
            state(false, Some("HTTP 500"), true)
        );
        assert_eq!(LoadState::of(true, true, None), state(false, None, true));
    }

    fn provider(enabled: bool, systems: Vec<FetchSystem>) -> FetchProviderStatus {
        FetchProviderStatus {
            provider: FetchProvider::Redump,
            enabled,
            running: false,
            release: None,
            systems,
            support: None,
        }
    }

    fn followed(system: Option<&str>) -> FetchSystem {
        FetchSystem {
            system: system.map(str::to_owned),
            dat: "Redump/Sony - PlayStation".into(),
            source_sets: vec!["PSX".into()],
            artifacts: Vec::new(),
        }
    }

    fn fetching_with(providers: Vec<FetchProviderStatus>) -> Fetching {
        fetching(Some(&FetchStatus { providers }))
    }

    #[test]
    fn on_an_empty_store_fetching_has_nothing_to_fetch_whether_on_or_off() {
        assert_eq!(fetching(None), Fetching::NothingToFetch);
        assert_eq!(
            fetching_with(vec![provider(false, Vec::new())]),
            Fetching::NothingToFetch
        );
        assert_eq!(
            fetching_with(vec![provider(true, Vec::new())]),
            Fetching::NothingToFetch
        );
        // A followed DAT the provider has no system for can't be fetched either.
        assert_eq!(
            fetching_with(vec![provider(true, vec![followed(None)])]),
            Fetching::NothingToFetch
        );
    }

    #[test]
    fn a_followed_dat_is_fetched_once_its_provider_is_turned_on() {
        assert_eq!(
            fetching_with(vec![provider(false, vec![followed(Some("psx"))])]),
            Fetching::Off
        );
        assert_eq!(
            fetching_with(vec![
                provider(false, vec![followed(Some("psx"))]),
                provider(true, Vec::new()),
            ]),
            Fetching::Off
        );
        assert_eq!(
            fetching_with(vec![
                provider(false, Vec::new()),
                provider(true, vec![followed(Some("psx"))]),
            ]),
            Fetching::On
        );
    }

    #[test]
    fn a_suggested_folder_is_named_for_its_last_folder() {
        assert_eq!(root_name("/mnt/roms"), "roms");
        assert_eq!(root_name("/data/"), "data");
    }
}
