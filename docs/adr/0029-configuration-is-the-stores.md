# 29. Configuration is the store's; only what bootstraps the Service lives outside it

Date: 2026-11-20

## Status

Accepted. Amends ADR 0008 and ADR 0022. Amended by #351: a Root's kind says what it holds, and fixes whether it is
writable (see "Amendment: Root kinds").

## Context

`stage-select.toml` declares the Roots, auth, the Host's and each Root's Ceilings (where a value set there locks it in
the workbench, ADR 0022), the 7z archive limits and the path to chdman. Everything else is already a setting in the
store, edited in the workbench. The file is optional, but without it there are no Roots, so in practice the Operator
has to write it, deploy it next to the container and restart the Service for every change. Most Operators won't keep
a deployed file the way this project's own deployment does.

## Decision

- **Outside the store is only what's needed before the store opens, or what the workbench can't safely change.** That
  is the store path, the listen address, the workbench's files, the clone lists, chdman, and auth. Each is a flag with
  a matching `STAGE_SELECT_*` environment variable. Auth is `STAGE_SELECT_IDENTITY_HEADER`,
  `STAGE_SELECT_TRUSTED_PROXIES` (comma-separated CIDRs), `STAGE_SELECT_DEV_IDENTITY` and the existing
  `STAGE_SELECT_OPERATOR_TOKEN_FILE`, with the same refusals at startup as now: no way to authenticate, or a dev
  identity off loopback. There is no configuration file: `--config`, `STAGE_SELECT_CONFIG` and `stage-select.toml`
  are removed.
- **Roots are registered in the workbench**, and changes take effect without a restart. Each change records who made it.
  - A Root's name is fixed once it's registered, because records refer to it.
  - Its kind is `sources`, `library`, `ingest` or `additions`, with at most one `additions` Root. It replaces the Root
    that's special because it's named `additions`. The kind fixes whether the Root is writable: a `sources` Root is
    read-only but for the Source Sets each switch lets Stage Select change; every other kind is writable.
  - Registering a Root checks that its path exists, and that it can be written when its kind is writable.
  - A Root can be unregistered only when nothing is registered in it, and its kind changes only then.
  - Its path or its kind can't be changed while a Job runs on it, nor its path while an approved, unapplied plan
    writes to it.
- **The container's mounts are the trust boundary** (ADR 0006). The workbench can register any folder the container
  can see. Mounting read-only what Stage Select must never write is how the Operator bounds it.
- **Every Ceiling and the 7z archive limits are workbench settings.** A Ceiling comes from the Operator's value, then the
  Calibration, then the CPU count. There's no file tier, and nothing is locked. `max_dictionary_mib` is shown with the
  memory it costs.

## Considered Options

- **Keeping Roots in the file as the bound on the workbench**: rejected; the mounts already bound what Stage Select
  can reach, and the file makes a first run need hand-written configuration.
- **Allowed base paths in the environment, with Roots registered beneath them**: rejected as a second boundary that
  adds a concept for little gain over the mounts.
- **Auth in the store**: rejected; the workbench mustn't be able to change who can reach the workbench, which risks
  both locking the Operator out and widening access.
- **A file holding only `[auth]`**: rejected; environment variables already carry the rest of bootstrap, and suit a
  container.

## Consequences

- A first run has no Roots, so the workbench leads the Operator through registering one, then Source Sets and Library
  Folders.
- ADR 0008: an Ingest Folder is a Root of kind `ingest`, writable, registered in the workbench, not declared in
  `stage-select.toml`.
- ADR 0022: a Ceiling's sources lose the configuration file; the workbench value is first.

## Amendment: Root kinds

The kind `general` held Source Sets when read-only and Library Folders when writable, a double meaning the Operator
couldn't see when registering a Root: a writable Root meant for Source Sets refused them. It is split into `sources` and
`library`, and the kind alone decides writability; there is no separate writable setting. Migration 0007 maps each
`general` Root to `sources` if it was read-only, or `library` if writable. A Root's `writable_paths` change only through
each Source Set's switch.

This breaks ADR 0030's additive promise for the Root API and CLI, once, as an exception the Operator decided as its only
client: `general` is refused like any unknown kind, and the register and edit request bodies lose `writable` and
`writable_paths`, which responses keep as derived fields. The CLI loses `--writable`, `--read-only`, `--writable-path`
and `--no-writable-paths`. The release notes list the change.
