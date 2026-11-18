# 22. Ceilings are calibrated, and one Pace scales them

Date: 2026-11-17

## Status

Accepted

## Context

Since #139, the `[jobs]` ceilings in the configuration file default to the CPU count, and the Pace (`gentle`,
`default` or `unthrottled`) is a share of each. A CPU count says nothing about storage. Roots on one Host may sit on
a seek-bound HDD pool or on an SSD, and on a seek-bound pool more readers can lower throughput. Title Select runs
unprivileged in a container (ADR 0006), so it can't see disks, pools or I/O wait: it sees only its CPUs and its
Roots. #20 asks for byte-rate caps for each Pace, a display of throughput, and defaults that fit the Host.

## Decision

- **Ceilings are measured, not assumed.** A Calibration is a Job that runs only when the Operator asks, from the
  workbench or `title-select calibrate`. It runs alone, holding other Jobs queued, and can be cancelled. It sweeps
  worker counts 1, 2, 4… and takes the knee: the smallest count within 10% of the best throughput, never one past
  a drop.
  - For the Host, it sweeps hash workers over synthetic data in memory.
  - For each Root, it sweeps payload readers over real files that Title Select hasn't read recently, read-only, and
    records the throughput at the knee.
  - Each step stops at 10 s or 2 GiB. A Root with too little unread data is reported as too small to measure.
- **Writes are never measured.** A Calibration writes nothing. A write byte-rate Ceiling exists only when the
  Operator sets one for a Root. Observed write throughput is displayed and may be offered as a starting value.
- **Ceilings belong to the Host or to a Root.** Concurrent Jobs, hash workers and 7z archive workers are Ceilings
  for the Host. Readers, a read byte rate and an optional write byte rate are Ceilings for each Root. Concurrent
  Jobs and 7z archive workers aren't measured: Jobs compete for the same Roots, and 7z decoding is limited by memory
  (`max_dictionary_mib`), not throughput.
- **Each Ceiling comes from the first of these sources that has a value:** the configuration file (which locks it in
  the workbench), then the Operator's value in the workbench, then the Calibration, then the CPU count. A
  Root that hasn't been calibrated has no read byte-rate Ceiling.
- **One Pace scales every Ceiling, live.** The Pace stays a single choice of three: `gentle` ¼, `default` ½ and
  `unthrottled` all, rounded down and never below 1 worker; `unthrottled` sets no byte cap. Worker counts change at
  each worker's next file. Byte caps are token buckets that the Pace's share of each Root is read into about every
  100 ms, shared by every Job reading or writing that Root, so a Pace change slows even a single large file within a
  second.
- **Calibration is never re-run automatically.** A Calibration becomes Stale when the CPU count changes, a Root is
  registered after it ran, or a Root's path changes. Its results keep applying until the Operator calibrates again.

## Considered Options

- **Hand-calibrated defaults documented for one Host** (the original #20 plan). Rejected: they'd suit only that
  Host, contrary to ADR 0006.
- **Calibrating automatically at first start.** Rejected: it would load a live Host, possibly busy with other work,
  without the Operator asking.
- **Synthetic writes to a writable Root.** Rejected: Title Select writes only through approved Change Plans.
- **A separate Pace for each Root, or a continuous dial.** Rejected: the Operator wants one simple control that
  reaches running Jobs.
- **One byte cap for the whole Host.** Rejected: Roots on one Host can differ by an order of magnitude.

## Consequences

- Byte caps limit Title Select's own reads and writes. They don't guarantee bandwidth on a pool that other software
  shares.
- Ceilings move from the configuration file alone into the store, where a backup of the store keeps them. Keys set in
  the configuration file keep their meaning and pin their value.
- `[ingest] archive_workers` becomes a Ceiling, which the Pace scales.
