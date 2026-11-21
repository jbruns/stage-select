//! The engine's integration tests, one module per area, in one test binary so that they link once.
//! `sevenz_memory.rs` stays a binary of its own, as it installs a counting global allocator.

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/sevenz.rs"]
mod sevenz_fixtures;

mod audit;
mod dat;
mod evidence;
mod family;
mod fixdat;
mod headers;
mod sevenz;
mod torrentzip;
