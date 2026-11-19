//! The server's integration tests, one module per area, in one test binary so that they link once.

#[path = "../../../engine/tests/common/mod.rs"]
mod common;

mod api;
mod notify;
