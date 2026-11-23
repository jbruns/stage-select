# 30. The rename to Stage Select is a clean break, and the compatibility promise restarts at 0.13.0

Date: 2026-11-22

## Status

Accepted. Amends ADR 0028.

## Context

ADR 0028 made M7's release the baseline. 0.12.0 was published as that release, and its notes promised that a store
made by it opens in every later release. The application was then renamed from Title Select to Stage Select (M8).
The old name is in everything ADR 0028 promises, and in more that it doesn't mention: the store's `application_id`
("TSel") and file name, the `.title-select/` state folder in every managed folder, the environment variables, the
metric names, the `schema` strings of the CLI's JSON and the `title_select_version` field of Selection pins. When the
rename was decided, hours after 0.12.0, only the Operator's own deployment had run it, and the Operator chose to
start it from an empty store.

## Decision

- **The rename is a clean break.** Every name maps straight across to its Stage Select form. 0.13.0 reads no earlier
  name: no old environment variable, state folder, store file name, local storage key or `schema` string.
- **Nothing made before 0.13.0 is carried.** The store takes a new `application_id`. A store stamped by an earlier
  release is refused, and the Operator is told to start a new store. A folder marked by an earlier
  release is not this store's, and nothing in it is adopted.
- **The promise 0.12.0 published is withdrawn, once.** ADR 0028's promise restarts at 0.13.0 on the same terms:
  append-only migrations, a release's warning before a removal, a new version for a breaking machine format, and
  release notes that list both. 0.13.0's notes say that the earlier promise was withdrawn and list every changed
  name.
- **Names in machine formats change with it**, including a Selection's outcome values, so that no old word has to
  be kept after 0.13.0.
- **The new name goes everywhere in the repository**, earlier ADRs included. Git history keeps what they said.

## Considered Options

- **Keep the store's promise only**: leave `application_id` as it is, so a 0.12.0 store opens, and break every other
  name. Rejected: the store would carry the old name for good, for a deployment that starts over anyway.
- **Honour ADR 0028 in full**: read every old name for one minor release, with warnings. Rejected: it adds a shim on
  every surface M7 had just cleared, for no known deployment.
- **Rename the on-disk names before M7's acceptance**: rejected because 0.12.0 was already published.

## Consequences

- A compatibility promise was published and withdrawn one release later. That is a cost to the promise's
  credibility, paid once and stated in the release notes; from 0.13.0 it must hold.
- M7's acceptance on the Host is superseded by M8's: the Host starts from an empty store and its Source Sets under
  the new names. Library Folders built by an earlier release are left on disk, unmanaged.
- Monitoring, scripts and deployment configuration that name the old binary, image, variables or metrics must be
  changed by hand at 0.13.0.
