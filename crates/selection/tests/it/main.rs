//! The Selection engine's integration tests, one module per area, in one test binary so that they link
//! once. `fixtures.rs` stays a binary of its own, as CI leaves it out of the debug run and runs it alone in release.

#[path = "../support/mod.rs"]
mod support;

mod arcade;
mod conformance;
mod discs;
mod fetch;
mod selection;
mod synthetic;
