//! The store's integration tests, one module per area, in one test binary so that they link once.
//! `fetch_proxy.rs` stays a binary of its own, as it sets the process's environment.

#[path = "../../../engine/tests/common/mod.rs"]
mod common;
#[path = "../../../engine/tests/common/sevenz.rs"]
mod sevenz_fixtures;

mod additions;
mod arcade;
mod arcade_build;
mod beneath;
mod builds;
mod calibration;
mod chd;
mod coverage;
mod dat_change;
mod dat_deletion;
mod discs;
mod disks;
mod fetch;
mod headers;
mod ingest;
mod lifecycle;
mod placements;
mod repair;
mod returns;
mod romm;
mod romm_additions;
mod roots;
mod set_layout;
mod single_copy;
mod stale;
mod store;
mod support_files;

use stage_select_api::roots::RootSpec;
use stage_select_store::Store;

/// Registers each of `roots` in `store`, creating its folder first if it is missing.
pub(crate) fn register(store: &Store, roots: &[RootSpec]) {
    for root in roots {
        std::fs::create_dir_all(&root.path).unwrap();
        store.register_root(root, "test").unwrap();
    }
}

/// Lets Stage Select change the registered Source Set `set`, as its switch does.
pub(crate) fn let_change(store: &Store, set: &str) {
    store.set_repairs_may_change(set, true, "test").unwrap();
}

/// Replaces the registered Root of the same name with `root`.
pub(crate) fn reregister(store: &Store, root: RootSpec) {
    std::fs::create_dir_all(&root.path).unwrap();
    store.edit_root(&root.name, &root, "test").unwrap();
}

/// Makes the Root `root` writable, as a `library` Root, or not, as a `sources` one, straight in the closed store at
/// `dir`, past the rules an edit keeps: as a store restored from a backup taken before an edit might hold it.
pub(crate) fn force_writable(dir: &std::path::Path, root: &str, writable: bool) {
    let conn = rusqlite::Connection::open(dir.join(stage_select_store::DATABASE)).unwrap();
    let kind = if writable { "library" } else { "sources" };
    let changed = conn
        .execute(
            "UPDATE roots SET writable = ?2, kind = ?3 WHERE name = ?1",
            rusqlite::params![root, writable, kind],
        )
        .unwrap();
    assert_eq!(changed, 1, "no root {root}");
}
