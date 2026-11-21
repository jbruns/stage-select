# Testing

How the tests run, locally and in CI, and the rules every test follows.

## Running the tests

`cargo test --locked` runs every test after `tools/clone-lists/fetch` has unpacked the clone
lists the Selection tests read. A debug build checks only some of the fixture goldens.

CI runs the same suite through [cargo-nextest](https://nexte.st), one process per test, with the `ci` profile in
`.config/nextest.toml` (install it with `cargo install cargo-nextest --locked`; CI pins its version in
`.github/workflows/ci.yaml`):

```sh
cargo nextest run --profile ci --locked -E 'not binary_id(title-select-selection::fixtures)'
cargo nextest run --profile ci --release --locked --package title-select-selection --test fixtures
```

The debug run leaves out the `fixtures` binary: its goldens are a subset of the ones the release run checks, and the
release run checks every golden (ADR 0015). `ONLY_DAT=<file name>` narrows the fixtures test to one DAT.

nextest runs no doctests, and there are none. The first doctest brings `cargo test --doc --locked` back: into CI after
the debug nextest run, and into the Build and test block in `AGENTS.md`.

The CHD tests need chdman. They skip without it, except where `CI` is set, where a missing chdman is a failure;
`TITLE_SELECT_CHDMAN` names the binary when it isn't on `PATH`.

## `tools/check`

`tools/check` is how agents build and test locally; GitHub CI's full run gates every merge. `--dry-run` prints its
commands without running them.

- **Default:** the crates changed since the merge-base with `origin/main` (committed, staged, unstaged and untracked),
  and every workspace crate that depends on them, dev-dependencies included. It runs fmt, clippy and the debug nextest
  run for those crates, both wasm clippy runs when api or ui is among them, and the release fixtures run when selection
  is. It prints what it chose and why first.
- **Paths outside `crates/`:** `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/` and `.config/` select
  every crate. `third_party/` and the clone-list and curated-list fetch scripts select selection, which compiles their
  pins in and whose tests read the fixtures; `docs/brand-kit/` selects ui, which compiles its tokens in. Anything else,
  such as docs, selects nothing.
- **`--crate X [-- ARGS]`:** clippy and nextest for X alone, X a folder under `crates/` or a package name, with ARGS
  as nextest's filter. Repeat `--crate` for several crates.
- **`--ci`:** exactly the CI commands above.

It fetches the clone lists and the curated list before nextest runs; both fetches do nothing when already current.

Builds go to `/tmp/ts-target-<worktree folder>` unless `CARGO_TARGET_DIR` is set, so agents in different worktrees never
wait on one cargo lock, and later rounds in one worktree build incrementally. When `sccache` is on `PATH` and
`RUSTC_WRAPPER` is unset, compiles go through it, with `SCCACHE_CACHE_SIZE` defaulting to `30G`, so the build folders
share dependency builds. CI uses neither.

The dev and test profiles build line tables only (`debug = "line-tables-only"` in `Cargo.toml`): a panic still names
its file and line, for a fraction of full debug info's disk. `CARGO_PROFILE_DEV_DEBUG=true` builds full debug info for
a debugger.

`tools/clean-builds` lists the T3 worktrees and `/tmp/ts-*` folders left by finished work: a worktree whose branch's
PR is merged or closed, or whose commits `origin/main` holds, and a folder whose worktree is gone or done. It keeps
anything with changes or modified in the last 60 minutes. `--yes` removes what it lists.

## Where integration tests go

Each crate's integration tests build into one test binary, `tests/it/`, as each binary links the crate and its
dependencies again. A new integration test goes in a module under `tests/it/`, declared in `tests/it/main.rs`, not in a
new file under `tests/`. Under `cargo test` the modules share one process, so a test that changes process-wide state
gets a binary of its own: the engine's `sevenz_memory` installs a global allocator, and the store's `fetch_proxy` sets
the environment. Selection's `fixtures` is a binary of its own too, as CI runs it alone in release.

## No retries

CI runs nextest with no retries. A flaky test is a failure until it is fixed at its cause; never add retries or
re-run CI until it passes.

## Order and amounts, never elapsed time

Tests assert order and amounts, never elapsed time.

- Ordering goes through hooks: channels, barriers, `cap::lockstep::Lockstep`, the `Fake` clocks.
- Backoffs and throttles take an injected clock, or a shorter value under test.
- Wall-clock time appears only as a deadlock guard, of 30 s or more.
- A test that only checks speed is deleted.
- Sleeping past a filesystem's mtime granularity is fine.

## Each layer tests its own part

- The store layer owns behaviour.
- API tests own the HTTP mapping: status, JSON shape, authentication.
- CLI tests own arguments and output text.

An upper-layer test that re-checks store behaviour is cut to its mapping when someone next touches it. There is no
cleanup pass.
