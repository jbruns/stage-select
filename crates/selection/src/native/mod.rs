//! The built-in Selection engine: a port of the pinned, patched Retool's Selection to Rust.
#![allow(dead_code)]

mod choose;
mod clone_lists;
mod dat;
mod filters;
mod json;
mod node;
mod overrides;
mod pipeline;
mod process;
mod py;
mod pyre;
mod pyset;
mod refs;
mod regexes;
mod select;
mod settings;
mod titletools;
mod trace;
mod userre;

pub(crate) use pipeline::run;

/// Reads one of the Operator's `/pattern` lines as Python's `re` would, or says why the engine can't.
pub(crate) fn user_regex(pattern: &str) -> Result<(), String> {
    UserRegex::new(pattern).map(|_| ())
}

/// One of the Operator's `/pattern` lines, compiled as Python's `re` reads it without case.
pub(crate) struct UserRegex(pyre::Re);

impl UserRegex {
    pub fn new(pattern: &str) -> Result<UserRegex, String> {
        pyre::Re::user(pattern).map(UserRegex)
    }

    pub fn search(&self, text: &str) -> bool {
        self.0.search(text)
    }
}

/// The system `dat` is for, as its header names it, or `None` for a DAT the engine can't read.
pub(crate) fn system_name(dat: &std::path::Path, internal_config: &[u8]) -> Option<String> {
    let text = process::read_text(dat).ok()?;
    let settings = settings::Settings::new(
        internal_config,
        &stage_select_api::selection::SelectionProfile::default(),
        "",
    )
    .ok()?;
    let header = dat::read_header(dat, &text, &settings).ok()?;
    Some(header.search_name).filter(|name| !name.is_empty())
}

/// Reads Games' regions from their names under the engine's internal configuration.
pub(crate) struct GameRegions(settings::Settings);

impl GameRegions {
    pub fn new(internal_config: &[u8]) -> Result<Self, crate::Error> {
        settings::Settings::new(
            internal_config,
            &stage_select_api::selection::SelectionProfile::default(),
            "",
        )
        .map(GameRegions)
    }

    /// As `DatNode` reads them: region tags don't depend on the Profile, and invalid characters are replaced first.
    pub fn of(&self, name: &str) -> Vec<String> {
        let name = titletools::replace_invalid_characters(name, false);
        let tag = titletools::regions(&name, &self.0, titletools::Method::Get);
        titletools::region_list(&tag, &self.0)
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// When a run of the built-in engine must stop: the caller cancelled it, or it ran out of time.
#[derive(Debug, Clone, Default)]
pub(crate) struct Limits {
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<(Instant, Duration)>,
}

/// The `Crash` messages that stand for a stop rather than an error.
const CANCELLED: &str = "\0cancelled";
const TIMED_OUT: &str = "\0timed out";

#[cfg(test)]
thread_local! {
    /// How many times this thread checked whether its run must stop.
    pub(crate) static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Limits {
    /// Fails when the run must stop.
    pub fn check(&self) -> py::PyResult<()> {
        #[cfg(test)]
        CHECKS.with(|checks| checks.set(checks.get() + 1));
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(py::Crash(CANCELLED.into()));
        }
        if self.deadline.is_some_and(|(at, _)| Instant::now() >= at) {
            return Err(py::Crash(TIMED_OUT.into()));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;
