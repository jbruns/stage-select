# RomM integration points

Findings for #136, following research 0005 (#5). Researched 2026-11-14 from RomM's source at the latest stable
release and its docs. This was read-only: no container was run, and no Host data was touched.

Pinned: **RomM 5.4.1** ([release](https://github.com/rommapp/romm/releases/tag/5.4.1)). The only newer release is
the pre-release 5.5.0-alpha.1 (2026-11-11). Below, `R` is `https://github.com/rommapp/romm/blob/5.4.1/backend/`. `D`
is `https://github.com/rommapp/docs/blob/main/docs/`, cited at main because that repo has no tags.

## Recommendation

Coordinate through RomM's API, never through its library. Title Select requests platform-scoped scans after a Build
changes a Library Folder, maintains one "Additions" collection, and reads RomM back to show disagreements. It writes no
identity into RomM. RomM keeps metadata and artwork, deployed with Hasheous off and ScreenScraper and libretro on.
Two Title Select layouts must change first, because RomM misreads them (§4). Decisions: ADR 0019, ADR 0020.

| Point | Direction | Decision |
| --- | --- | --- |
| Scan request after a Build or cleanup | Title Select → RomM | Yes (#141) |
| Additions collection | Title Select → RomM | Yes (#142) |
| Per-Profile collections | Title Select → RomM | No: they repeat the library |
| Provider ids or "verified" from our Audits | Title Select → RomM | No: we hold no provider ids; impersonating Hasheous is too brittle |
| Reconciliation of RomM's matches with the Build | RomM → Title Select | Yes (#143) |
| Artwork and metadata in the workbench | RomM → Title Select | Deferred |
| ES-DE `gamelist.xml` and media in Library Folders | Title Select → library | Deferred |
| Pulling files from RomM into an Ingest Folder | RomM → Title Select | No: circular, and Title Select never acquires |

## 1. Scans

- **Request.** `POST /api/tasks/scan` needs `tasks.run` and returns 202 with a task id
  ([R endpoints/tasks.py L400-427](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/tasks.py#L400-L427)).
  - The body takes `type`, `platform_fs_slugs`, `platforms`, `roms_ids` and `apis`, and rejects unknown keys
    ([R endpoints/sockets/scan.py L1527-1554](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/sockets/scan.py#L1527-L1554)).
  - It returns 409 while a library scan is queued or running, and 503 without a scan worker
    ([L1557-1571](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/sockets/scan.py#L1557-L1571)).
- **What each type picks up:**
  - **New files:** `quick`, `complete` and `hashes`; `update` and `unmatched` only touch existing ROMs
    ([scan.py L395-425](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/sockets/scan.py#L395-L425)).
  - **Removed files:** every platform scan marks them `missing_from_fs`
    ([L1055-1064](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/sockets/scan.py#L1055-L1064)).
  - **Changed files:** `quick` rehashes a file whose size or mtime changed
    ([R handler/rom_files.py L75-120](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/rom_files.py#L75-L120)).
    Builds stamp mtimes, so `quick` is enough.
- **Completion.** Poll `GET /api/tasks/{id}` for RQ status, `finished` or `failed`
  ([tasks.py L139-160](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/tasks.py#L139-L160)).
  - Socket.io `scan:done` authenticates by session cookie, not by token
    ([D developers/websockets.md L17-33](https://github.com/rommapp/docs/blob/main/docs/developers/websockets.md#L17-L33)).
  - RomM also sends signed webhooks (`scan_completed`, `X-RomM-Signature`) to the starting user's channels
    ([D using/notifications.md L39-87](https://github.com/rommapp/docs/blob/main/docs/using/notifications.md#L39-L87)).
    Polling is simpler and needs no inbound route.
- **Leave RomM's own triggers off.** The watcher is `watchfiles` on the library. Its docs call it flaky over SMB/NFS
  and warn against it while another tool writes the library
  ([D administration/scanning-and-watcher.md L69-84](https://github.com/rommapp/docs/blob/main/docs/administration/scanning-and-watcher.md#L69-L84)).
  Scheduled rescans are off by default
  ([R config/\_\_init\_\_.py L284-297](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L284-L297)).

## 2. Credentials

- **Tokens.** Client API Tokens are `Bearer rmm_…`. A token's scopes are a subset of its owner's, and expiry is
  optional ([R endpoints/client_tokens.py L60-88](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/client_tokens.py#L60-L88),
  [R utils/client_tokens.py L25-57](https://github.com/rommapp/romm/blob/5.4.1/backend/utils/client_tokens.py#L25-L57)).
- **A non-admin owner works.** A non-admin holds `tasks.run` when its group grants Tasks write
  ([R handler/auth/permissions.py L184-216](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/auth/permissions.py#L184-L216)).
- **Scopes needed:** `roms.read`, `platforms.read`, `collections.read`, `collections.write` and `tasks.run`
  ([R handler/auth/constants.py L13-35](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/auth/constants.py#L13-L35)).
- **OIDC.** An OIDC access token can't call the API, so you create a Client API Token while signed in
  ([D developers/api-authentication.md L112](https://github.com/rommapp/docs/blob/main/docs/developers/api-authentication.md#L112)).
- **5.5.0-alpha.1** caps token scopes at the caller's and binds paired tokens to devices (release notes).

## 3. Reading and writing data

- **Reading.**
  - `GET /api/roms` pages with `limit` up to 10000 and `offset`, filtering by platform, `matched`, `verified` and
    others ([R handler/database/rom_filters.py L118-192](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/database/rom_filters.py#L118-L192)).
  - A ROM carries `fs_name`, `fs_path`, provider ids, parsed regions and tags, and `files[]` with per-file CRC32, MD5
    and SHA-1 ([R endpoints/responses/rom.py L253-272, L349-451](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/responses/rom.py#L349-L451)).
  - `GET /api/roms/by-hash?sha1_hash=` resolves a file
    ([R endpoints/roms/\_\_init\_\_.py L1199-1235](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/roms/__init__.py#L1199-L1235)).
- **Collections.** Static collections are created with `POST /api/collections` and their members edited with
  `POST`/`DELETE /api/collections/{id}/roms` (`collections.write`). Smart collections store `RomFilterParams`, which
  can't express "chosen by a Selection Profile"
  ([R endpoints/collections.py](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/collections.py#L114)).
- **ROM edits.** `PUT /api/roms/{id}` (`roms.write`) can set provider ids, names and covers
  ([L429-500](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/roms/__init__.py#L429-L500)). Title Select
  doesn't use it: it holds no provider ids, and a renamed `fs_name` would write into the library.
- **Identity.**
  - RomM's `verified` comes only from Hasheous or RetroAchievements
    ([D getting-started/metadata-providers.md L140](https://github.com/rommapp/docs/blob/main/docs/getting-started/metadata-providers.md#L140)).
  - RomM won't import DATs: "We don't plan to include DAT handling within the app itself"
    ([rommapp/romm#4079](https://github.com/rommapp/romm/issues/4079)).
  - Hasheous skips `.m3u` files
    ([R handler/metadata/hasheous_handler.py L361-366](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/metadata/hasheous_handler.py#L361-L366)).
  - *Inference:* a CHD's hash appears in no Redump DAT, so CHD libraries rely on name matching.
- **Writes into the library.** Uploads, `gamelist.xml`/Pegasus exports and conversions all write into the library, so
  they fail under `:ro` and stay off ([D reference/exports.md L10-62](https://github.com/rommapp/docs/blob/main/docs/reference/exports.md#L10-L62)).
  `gamelist.xml` is read as a provider ([D metadata-providers.md L184-287](https://github.com/rommapp/docs/blob/main/docs/getting-started/metadata-providers.md#L184-L287)).

## 4. Layouts RomM misreads

- **The marker file is indexed as a ROM.** A dotfile is listed as a single-file ROM unless named in
  `exclude.roms.single_file.names`; the defaults are an explicit list, not a dotfile rule
  ([R utils/filesystem.py L28-38](https://github.com/rommapp/romm/blob/5.4.1/backend/utils/filesystem.py#L28-L38),
  [R config/config_manager.py L267-286](https://github.com/rommapp/romm/blob/5.4.1/backend/config/config_manager.py#L267-L286)).
  So the `.title-select` marker file is indexed as a ROM.
- **Dot-folders are skipped.** Dot-folders inside a platform folder are skipped
  ([R handler/filesystem/roms_handler.py L530-544](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/filesystem/roms_handler.py#L530-L544)).
  Fix: all folder state moves under `.title-select/` (#137).
- **`.multidisc/` discs are never indexed.** The `.m3u` becomes a standalone ROM whose download is the playlist alone,
  and RomM doesn't follow an `.m3u` to files outside its own game folder
  ([R utils/m3u.py L49-70](https://github.com/rommapp/romm/blob/5.4.1/backend/utils/m3u.py#L49-L70)).
  A folder such as `<game>.cue/` is one multi-file game
  ([roms_handler.py L1212-1217](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/filesystem/roms_handler.py#L1212-L1217)).
  Fix: `<release>.m3u/` (ADR 0019, #138).
- **Arcade slugs change in 5.5.** 5.5.0-alpha.1 splits arcade into `mame`, `fbneo`, `fba` and board platforms, which
  matters for arcade Library Folder names (release notes).

## 5. Precedent

RomM's igir guide is the closest organizer pattern. Its DAT-sorted output is mounted `:ro` and scanned, with no API
coordination ([D ecosystem/igir.md L174-185](https://github.com/rommapp/docs/blob/main/docs/ecosystem/igir.md#L174-L185)).
Title Select adds scan requests and reconciliation on top of the same boundary.

## Gaps

- The docs' anchors were taken at main and not diffed against 5.4.1.
- Whether a scan ending in `scan:done_ko` leaves its RQ job `failed` or `finished` was not verified. #141 must treat
  both as possible.
- The "flaky over SMB/NFS" behaviour of the watcher rests on RomM's docs only.
