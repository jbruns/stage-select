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

#202 tracks the tests that break these rules today.

## Each layer tests its own part

- The store layer owns behaviour.
- API tests own the HTTP mapping: status, JSON shape, authentication.
- CLI tests own arguments and output text.

An upper-layer test that re-checks store behaviour is cut to its mapping when someone next touches it. There is no
cleanup pass.
