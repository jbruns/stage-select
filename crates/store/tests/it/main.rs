//! The store's integration tests, one module per area, in one test binary so that they link once.
//! `fetch_proxy.rs` stays a binary of its own, as it sets the process's environment.

#[path = "../../../engine/tests/common/mod.rs"]
mod common;
#[path = "../../../engine/tests/common/sevenz.rs"]
mod sevenz_fixtures;

mod additions;
mod builds;
mod calibration;
mod chd;
mod coverage;
mod dat_deletion;
mod discs;
mod fetch;
mod headers;
mod ingest;
mod lifecycle;
mod placements;
mod repair;
mod returns;
mod romm;
mod romm_additions;
mod single_copy;
mod stale;
mod store;
