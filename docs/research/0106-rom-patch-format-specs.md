# ROM/binary patch formats: headers, checksums, reversibility and header-sensitivity

Format reference for #106, companion to [0106-patch-derived-additions.md](0106-patch-derived-additions.md),
which holds the recommendation for Stage Select. It documents, from primary sources read directly
(spec text, RFC text, and reference-implementation source code, not descriptions), what each common ROM
patch format puts in its header/footer, which checksums (if any) it carries for the source file, the
target file and the patch file itself, whether it is reversible, and how known implementations handle
SNES copier headers (512-byte SMC/SWC) and iNES headers (16-byte NES). Sources were fetched 2026-11-14 and
are pinned to the commit, tag or snapshot named in each section and in **Sources** below; `no-intro.org`
and `datomatic.no-intro.org` were not consulted (excluded per task scope).

## Recommendation

**BPS and UPS are the only formats here that can verify the source file before applying and the target
file after applying, from fields carried in the patch itself.** Both store three CRC32 values in a fixed
12-byte footer: source checksum, target checksum, patch-file checksum
[Source: Alcaro/Flips (ff216a75):bps_spec.md] [Source: Alcaro/Flips (ff216a75):libups.cpp]. BPS additionally
carries explicit source/target sizes before the footer, so a size mismatch is cheap to detect before even
hashing [Source: Alcaro/Flips (ff216a75):bps_spec.md]. VCDIFF/xdelta3 checks integrity per-window (RFC 3284
does not mandate a whole-file digest at all), which is why xdelta3 upstream added its own out-of-band
"armor mode" (BLAKE3 digests of the whole source and target, outside the VCDIFF grammar) specifically to
fix the "wrong source produces a late, confusing error" failure mode [Source:
jmacd/xdelta (5d936d2c):site/docs/armor.md]. IPS, classic PPF and APS (GBA) carry no source/target/patch
checksum at all in the base format: APS (GBA) is the exception among the "no checksum" group, carrying a
per-64KB-block CRC16 for source and target [Source: UniPatcher wiki — APS (GBA)]. bsdiff (BSDIFF40) and RUP
(NINJA2) sit in between: bsdiff carries no checksums of any kind [Source:
marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.bdf.js], while RUP carries
whole-file MD5 for both source and target [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.rup.js].
For curation purposes, where a Source Set must be audited against its DAT before any Selection trusts it,
formats with an embedded source checksum (BPS, UPS, RUP, APS-GBA) let a tool refuse to apply a patch to the
wrong base file without first computing an external hash; formats without one (classic IPS, PPF, APS-N64's
simple-patch mode, bsdiff) rely entirely on the operator supplying the right base file, or on the
patcher's own out-of-band ROM database (Flips keeps a local CRC32-indexed "known good ROMs" list, BPS
only [Source: Alcaro/Flips (ff216a75):flips.cpp — `FindRomForPatch`]).

## 1. IPS

- **Header/magic**: 5-byte ASCII `"PATCH"`, not NUL-terminated. Records follow; each is a 3-byte
  big-endian offset, a 2-byte big-endian size, then `size` bytes of data. A 3-byte `"EOF"` marker
  (0x45 0x4F 0x46 read as the 24-bit value `0x454F46`) ends the record stream in place of another offset
  [Source: zerosoft.zophar.net/ips.php (Z.e.r.o's IPS spec, live 2026-11-14)] [Source:
  Alcaro/Flips (ff216a75):libips.cpp — `ips_study`].
- **RLE extension**: if a record's 2-byte size field is `0x0000`, the next 2 bytes are a run length and
  the following single byte is the fill value, repeated that many times starting at the offset
  [Source: zerosoft.zophar.net/ips.php] [Source: Alcaro/Flips (ff216a75):libips.cpp].
- **Truncation/EOF extension**: if exactly 3 bytes remain after the `EOF` marker, they are a 24-bit
  big-endian length the output file must be truncated (or, per a 2023 Flips-ecosystem pull request,
  expanded with zero bytes) to after all records are applied
  [Source: Alcaro/Flips (ff216a75):libips.cpp — `ips_study`, the `outlen_max`/`truncate` handling]
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.ips.js —
  `IPS.prototype.apply`, referencing `marcrobledo/RomPatcher.js#46`].
- **Size limits**: 24-bit offsets/sizes cap the patchable file at 2^24−1 bytes (~16 MiB); Flips rejects
  anything larger with `ips_16MB` [Source: Alcaro/Flips (ff216a75):flips.cpp — `ipserrors[]`].
- **IPS32**: a 32-bit-offset variant. Magic is `"IPS32"` (5 bytes), records are a 4-byte big-endian offset
  + 2-byte size (RLE on zero, identical to classic IPS), and the terminator is the 4-byte ASCII `"EEOF"`
  (not 3-byte `"EOF"`, precisely because a 4-byte offset field could otherwise collide with a 3-byte
  marker), optionally followed by a 4-byte truncation length
  [Source: Nifyr/ips32 (7c48b06d):src/lib.rs — the `ips()` and `hunk()` parsers]. Flips itself does not
  implement IPS32.
- **Checksums**: none. No source, target or patch checksum field exists in either variant; a patcher can
  only compare the ROM's observed size against where the highest record writes
  [Source: Alcaro/Flips (ff216a75):libips.cpp]. Flips' own "is this the right ROM" GUI feature works only
  for BPS, via a separately maintained CRC32-to-filename list, not from the IPS file itself
  [Source: Alcaro/Flips (ff216a75):flips.cpp — `FindRomForPatch` ("UPS has checksums too, but screw UPS.
  Nobody cares." applies to BPS vs. IPS/UPS discovery, not to IPS carrying a checksum)].
- **Reversibility**: one-directional. Nothing in the format records the original bytes it overwrote, so a
  target cannot be turned back into its source without a second, separately created patch.
- **Header-sensitivity**: Flips strips a 512-byte SNES copier header (detected by `.smc`/`.sfc` extension
  and `(romlen & 0x7FFF) == 512`) before diffing or applying **only for BPS**; `ips_create`/`ips_apply` in
  Flips operate on the file memory it is given with no header awareness of their own
  [Source: Alcaro/Flips (ff216a75):flips.cpp — `shouldRemoveHeader`, and its use sites in
  `CreatePatchToMem`/`ApplyPatchMem2`, both gated `patchtype==ty_bps`]. RomPatcher.js separately defines a
  table of known header sizes (iNES 16 bytes, fwNES 16 bytes, LNX 64 bytes, SNES copier 512 bytes) used for
  its own ROM-system detection, independent of IPS parsing itself
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/RomPatcher.js — `HEADERS_INFO`].
- **Metadata**: none in base IPS. RomPatcher.js's module doubles as a parser for "EBP" (EarthBound Patch),
  a third-party convention that appends a JSON object after the `EOF` marker instead of a truncation
  length; this is not part of the IPS format itself
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.ips.js].

## 2. UPS

- **Header/magic**: 4-byte ASCII `"UPS1"`, then a variable-length-encoded source size, then a
  variable-length-encoded target size [Source: Alcaro/Flips (ff216a75):libups.cpp — `ups_apply`]. The
  integer encoding is the same base-128, continuation-bit, "subtract after shift" scheme BPS uses (see
  §3); Flips' UPS decoder implements it with the identical `shift`/`addthis` structure.
  No independent `ups_spec` document could be found (see Gaps); this description is read directly from
  Flips' implementation.
- **Body**: a sequence of `(skip, xor-run)` pairs: a variable-length "bytes to skip unchanged", then a
  run of XOR bytes terminated by a `0x00` byte, applied as `output[i] = input[i] ^ patch_byte` for each
  byte of the run (and `output[i] = input[i]` for the skipped bytes). Reading past the end of the shorter
  file yields `0x00` [Source: Alcaro/Flips (ff216a75):libups.cpp — the `while (patchat<patchend)` loop].
- **Footer**: 12 bytes — `uint32 source-checksum`, `uint32 target-checksum`, `uint32 patch-checksum`, all
  CRC32, patch-checksum over every byte preceding it (i.e., `patch.len-4`)
  [Source: Alcaro/Flips (ff216a75):libups.cpp — `crc_in_expected`/`crc_out_expected`/`crc_patch_expected`
  and `crc_patch=crc32(patch.ptr, patch.len-4)`]. This is the same three-CRC32 shape as BPS's footer (§3),
  but the two formats were not read from one shared spec document for UPS; it is inferred from matching
  implementation behaviour, not confirmed by a byuu-authored UPS spec text.
- **Size limits**: none fixed by the format; the variable-length integer encoding supports arbitrary
  sizes (Flips uses `size_t`, so limited by the host's `size_t`)
  [Source: Alcaro/Flips (ff216a75):libups.cpp — `ups_too_big`/`assert_shift`/`assert_sum`].
- **Reversibility**: **bidirectional by construction.** If the input file's length matches the patch's
  recorded *target* size rather than its source size, `ups_apply` swaps the roles of the two recorded
  sizes and checksums and treats the operation as "undo" (XOR is its own inverse), reconstructing the
  original source from the target
  [Source: Alcaro/Flips (ff216a75):libups.cpp — the `backwards` flag and the `if (inlen!=in.len)` swap].
  When source and target are the same length, `ups_apply` accepts either checksum pairing
  [Source: Alcaro/Flips (ff216a75):libups.cpp — the `inlen==outlen` checksum branch].
- **Header-sensitivity**: Flips comments "UPS has checksums too, but screw UPS. Nobody cares." next to the
  code that deliberately does *not* wire UPS into its CRC32-based ROM-recall feature — this is a UX
  decision in Flips, not a format limitation
  [Source: Alcaro/Flips (ff216a75):flips.cpp — `FindRomForPatch`]. Flips' 512-byte SMC/SWC header removal
  is gated to BPS only; it is never applied for UPS creation or application
  [Source: Alcaro/Flips (ff216a75):flips.cpp — `CreatePatchToMem`/`ApplyPatchMem2`, both test
  `patchtype==ty_bps`].
- **Metadata**: none. `ups_create` is unimplemented in Flips ("unimplemented, just pick a random error"),
  so Flips can apply but not create UPS patches
  [Source: Alcaro/Flips (ff216a75):libups.cpp — `ups_create`].

## 3. BPS

Per byuu's "beat" / BPS specification, preserved verbatim in the Flips repository
(`bps_spec.md`, "by byuu. Public domain."):

- **Header**: 4-byte ASCII `"BPS1"`, then `number source-size`, `number target-size`,
  `number metadata-size`, then `metadata-size` bytes of metadata (UTF-8 XML by convention, but the format
  accepts anything), then a stream of actions until 12 bytes remain
  [Source: Alcaro/Flips (ff216a75):bps_spec.md — "Header" and "Linear creation" sections].
- **Variable-length integer encoding**: base-128, low 7 bits per byte, high bit marks the final byte, and
  the value is decremented by one at each non-final step to avoid dual encodings of the same number
  (`0x81` vs `0x01 0x80` for 1) — this removes the classic IPS/IPS32 16 MiB/4 GiB offset ceiling entirely;
  BPS's own comment calls patching a file over 16 exabytes "silly to even imagine" but technically
  supported [Source: Alcaro/Flips (ff216a75):bps_spec.md — "Variable-length number encoding"].
- **Actions**: 2-bit opcode packed into the low bits of each decoded length value — `SourceRead` (0),
  `TargetRead` (1, literal bytes follow in the patch), `SourceCopy` (2, signed relative seek into the
  source file, used as an LZ-style dictionary), `TargetCopy` (3, signed relative seek into the
  already-written target, enabling RLE of arbitrarily long runs from a single literal byte)
  [Source: Alcaro/Flips (ff216a75):bps_spec.md — per-action sections] [Source:
  Alcaro/Flips (ff216a75):libbps.cpp — `bps_apply`'s `switch (action)`].
- **Footer (12 bytes)**: `uint32 source-checksum`, `uint32 target-checksum`, `uint32 patch-checksum`, all
  CRC32; the patch checksum covers every byte of the patch except itself (the last 4 bytes)
  [Source: Alcaro/Flips (ff216a75):bps_spec.md — "Footer"] [Source:
  Alcaro/Flips (ff216a75):libbps.cpp — `crc_patch_a = crc32(patch.ptr, patch.len-4)`].
  ```
  uint32 source_checksum   // CRC32 of the whole source file
  uint32 target_checksum   // CRC32 of the whole target file
  uint32 patch_checksum    // CRC32 of every preceding byte of this patch file
  ```
- **Checksums present**: source, target and patch-self, all CRC32 — the most complete integrity story of
  any format here. `bps_apply` checks the input file's recorded size and CRC32 *before* applying and
  reports `bps_not_this`/`bps_to_output` distinctly (the latter meaning "you handed me the already-patched
  file") [Source: Alcaro/Flips (ff216a75):libbps.cpp — `bps_apply`'s `inlen!=in.len` / `crc_in_a!=crc_in_e`
  checks], and verifies the freshly produced target's CRC32 against the footer afterward
  [Source: Alcaro/Flips (ff216a75):libbps.cpp — `crc_out_a = crc32(out->ptr, out->len)`].
- **Reversibility**: not reversible by a single BPS file the way UPS is; BPS is a one-directional linear
  encoding of "how to build the target from scratch", though because `SourceRead`/`SourceCopy` reference
  the original file, the patch is typically small even without an explicit reverse-diff mode. (byuu's
  tooling conventionally ships a second, reverse BPS to undo a patch, rather than one bidirectional file.)
- **Header-sensitivity**: Flips strips a detected 512-byte SNES copier header before BPS creation and
  application, via a `fileheader`/`fileheadermap` wrapper that offsets all reads by 512 and reports a
  512-byte-shorter length; on an apply-time BPS size/checksum mismatch it transparently retries with the
  header re-attached, emitting the warning "the patch was applied, but it was created from a headered ROM,
  which may not work for everyone" if that retry succeeds [Source: Alcaro/Flips (ff216a75):flips.cpp —
  `shouldRemoveHeader`, `fileheader`, `fileheadermap`, and the recursive retry in `ApplyPatchMem2`]. The
  CLI's `--exact` flag disables this stripping [Source: Alcaro/Flips (ff216a75):flips.cpp — `usage()`].
- **Metadata**: the one field other than UPS-style checksums that sets BPS apart — a length-prefixed,
  freeform byte blob, conventionally XML (byuu's manifest convention) but validated only for syntax, not
  schema, by tools: "the actual contents here are entirely domain-specific, so literally anything can go
  here and the patch will still be considered valid"
  [Source: Alcaro/Flips (ff216a75):bps_spec.md — "Header"]. Flips reads/writes this metadata as a sibling
  `.xml` file next to the ROM during creation/application
  [Source: Alcaro/Flips (ff216a75):flips.cpp — `GetManifestName`, `manifestinfo`].

## 4. xdelta3 / VCDIFF

- **RFC 3284** defines the base VCDIFF container: a 4-byte header (`0xD6 0xC3 0xC4`, ASCII `VCD` with the
  high bit set, plus a version byte currently `0`), an `Hdr_Indicator` byte for an optional secondary
  compressor and code table, then a sequence of windows, each with a `Win_Indicator` byte (bit 0
  `VCD_SOURCE`, bit 1 `VCD_TARGET` — at most one set), optional source-segment size/position, and the
  window's own delta encoding (target size, a `Delta_Indicator` byte for per-section secondary
  compression, and the three COPY/ADD/RUN data sections)
  [Source: RFC 3284 §4, §4.1, §4.2, §4.3 — www.rfc-editor.org/rfc/rfc3284]. Integers use the same
  base-128, high-bit-continuation encoding described for BPS/UPS above, without the "subtract one" trick
  [Source: RFC 3284 §2].
- **No whole-file checksum in the base RFC.** Checking is per-window only; the RFC's `Win_Indicator` byte
  defines just `VCD_SOURCE`/`VCD_TARGET` (bits 0–1), leaving bit 2 upward unspecified
  [Source: RFC 3284 §4.2].
- **xdelta3's own extension — `VCD_ADLER32`**: xdelta3 defines `VCD_ADLER32` as bit 2 of the
  `Win_Indicator` byte (`#define VCD_ADLER32 (1U << 2)`), a non-RFC extension that stores an Adler-32
  checksum of each decoded target window for corruption detection; the decoder verifies it unless
  `XD3_ADLER32_NOVER` is set [Source: jmacd/xdelta (5d936d2c):xdelta3/xdelta3.c — `VCD_ADLER32`
  definition] [Source: jmacd/xdelta (5d936d2c):xdelta3/xdelta3-decode.h — the `(stream->dec_win_ind &
  VCD_ADLER32)` checks]. This is per-window, not a whole-source/whole-target digest, so (per xdelta3's own
  docs) a wrong source file can still partially decode before a late window-checksum failure
  [Source: jmacd/xdelta (5d936d2c):site/docs/checksum-mismatch.md].
- **xdelta3 application header**: xdelta3 additionally writes an "application header" (a free-form byte
  blob in the VCDIFF header region, outside RFC 3284's mandatory fields) historically used to record
  filenames. As of the current `main` branch (fetched 2026-11-14, well after RFC 3284's 2002 publication),
  xdelta3 added "armor mode", on by default, which repurposes this application header to store BLAKE3
  digests of the *entire* source and target files (`name#blake3hex` syntax), checked up front on decode
  — turning the historically late, ambiguous window-checksum failure into an immediate "wrong source"
  error, and reporting a distinct "already up to date" exit code 2 if the supplied source already matches
  the target digest [Source: jmacd/xdelta (5d936d2c):site/docs/armor.md]. Armor requires seekable
  (regular-file) source and target and can be disabled with `-a`, which restores the legacy
  streaming-compatible behaviour [Source: jmacd/xdelta (5d936d2c):site/docs/armor.md].
- **Size limits**: none fixed by the format itself; xdelta3 defaults to a 64 MiB (`1U<<26`) hard maximum
  per-window decode size as a DoS guard, not a format ceiling
  [Source: jmacd/xdelta (5d936d2c):xdelta3/xdelta3.h — `XD3_HARDMAXWINSIZE`].
- **Reversibility**: one-directional, like BPS; VCDIFF encodes how to build the target from the source,
  with no standard reverse-diff convention.
- **Header-sensitivity**: no ROM-specific (SNES/NES) header handling exists in VCDIFF or xdelta3 itself;
  any such stripping would be an external, out-of-band step before diffing, same as for bsdiff.
- **Metadata**: the application header is free-form; aside from xdelta3's own filename/armor conventions,
  VCDIFF does not define a manifest-style metadata field comparable to BPS's.

## 5. PPF

Primary byte-for-byte PPF 3.0 specification text could not be located, live or archived (see Gaps); the
following is read from a from-scratch reimplementation that cites the original spec location
[Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.ppf.js — top-of-file
comment "File format specification: https://www.romhacking.net/utilities/353/"].

- **Header/magic**: 3-byte ASCII `"PPF"`, then a 2-ASCII-digit version string (`"10"`, `"20"` or `"30"`,
  i.e. version×10), then a version-confirmation byte (`version-1`), then a 50-byte space-padded
  description field [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js — `PPF.fromFile`].
- **Version-specific header tail**:
  - PPF2: a 4-byte `inputFileSize`, followed unconditionally by a 1024-byte "block check" region.
  - PPF3: 1 image-type byte (0 = BIN, 1 = Graphic Image), 1 "block check present?" byte, 1 "undo data
    present?" byte, 1 dummy byte, then the 1024-byte block-check region only if that flag was set
    [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js — `PPF.fromFile`'s version-3/version-2
    branches].
- **Block check**: a raw 1024-byte copy of bytes from the *original* file (not a hash of them), intended
  to let a patcher sanity-check it has the right source image before applying. RomPatcher.js's reader
  parses and stores this block but its `PPF.prototype.apply` **never compares it against the ROM being
  patched** — the block-check field is read, not enforced, in this implementation
  [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js — `PPF.prototype.apply` has no reference to
  `this.blockCheck`].
- **Records**: a 4-byte little-endian offset (PPF3 doubles this with an additional 4-byte "high" offset
  word, assembled as a 64-bit value, to extend past 4 GiB), a 1-byte data length, then that many data
  bytes; PPF3 with "undo data" enabled stores an equal-length block of the original bytes immediately
  after each record's new data [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js — `PPF.fromFile`'s
  record loop and `PPF.prototype.addRecord`].
- **Undo data / "block check" vs. true reversibility**: PPF3's undo-data flag is the format's reversal
  mechanism (not the block-check field, despite the similar name). When applying, RomPatcher.js detects
  whether it is "undoing" by comparing the *first* record's would-be-written bytes against what's already
  on disk at that offset: if they already match, it writes each record's stored `undoData` instead of
  `data`, effectively reconstructing the source from the target using the same patch file
  [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js — `PPF.prototype.apply`'s `undoingData` branch].
  Without undo data, PPF (any version) is one-directional.
- **Checksums**: none, in any version — no source checksum, no target checksum, no patch-self checksum.
  The 1024-byte block check is the closest analogue to a source-verification mechanism, and (per above) at
  least this reimplementation does not act on it.
- **Size limits**: PPF1/2 offsets are 32-bit (4 GiB); PPF3 extends to a 64-bit offset by reading a second
  4-byte word and left-shifting it 32 bits [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js —
  `PPF.prototype.export`'s `offset2` construction and the version-3 read branch]. Per-record data length is
  capped at 255 bytes (1-byte length field).
- **Header-sensitivity**: no SNES/NES-specific header logic in this module; PPF originates from the PSX
  scene (CD images), where there is no equivalent copier-header convention.
- **Metadata**: the 50-byte description field is the only built-in freeform text. PPF3 optionally appends
  a `@BEGIN_FILE_ID.DIZ` / `@END_FILE_ID.DIZ`-delimited block (up to 3072 bytes) for a longer, classic
  "FILE_ID.DIZ"-style archive description, trailed by a 2-byte length and a 2-byte pad
  [Source: marcrobledo/RomPatcher.js (4f17297c):…ppf.js — `PPF_BEGIN_FILE_ID_DIZ_MAGIC` and its
  read/write sites].

## 6. APS

The N64 and GBA variants share only the name; they are unrelated formats with different magics, header
layouts and patch encodings.

### APS (N64)

Per the original 1998 "Advanced Patching System" draft (Silo/Fractal, Blackbag), reproduced on the
UniPatcher wiki from the original `aps.txt`, with a documented byte-range erratum from `xPSapply.txt`:

- **Header/magic**: 5-byte ASCII `"APS10"`, then 1 patch-type byte (0 = simple, 1 = N64-specific), 1
  encoding-method byte (0 = simple/IPS-like), then a 50-byte space-padded description
  [Source: UniPatcher wiki — APS (N64), "Standard Header"].
- **Type 0 (simple) header tail**: a 4-byte destination-image size.
- **Type 1 (N64) header tail**: 1 original-format byte (0 = Doctor V64, 1 = CD64/Z64/etc.), a 2-byte Cart
  ID, a 1-byte country code, an 8-byte CRC (both taken verbatim from the original ROM image), a 5-byte pad
  (per the corrected byte ranges; the original draft misnumbered these as a 6+6 split), then a 4-byte
  destination-image size [Source: UniPatcher wiki — APS (N64), "Type 1: N64 Header" plus the erratum
  comment]. RomPatcher.js's implementation matches the corrected layout: `originalN64Format` (1),
  `cartId` (3, not 2 — RomPatcher.js reads 3 bytes), `crc` (8), `pad` (5)
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.aps_n64.js —
  `APS.fromFile`]. (RomPatcher.js's 3-byte Cart ID disagrees with the 2-byte Cart ID in the archived
  1998 draft text; this is flagged as unresolved in Gaps.)
- **Records**: 4-byte offset, 1-byte length (`y`); if `y==0`, the next byte is the fill value and the byte
  after that is the repeat count (RLE); otherwise `y` literal bytes follow
  [Source: UniPatcher wiki — APS (N64), "Encoding Method #0"] [Source:
  marcrobledo/RomPatcher.js (4f17297c):…aps_n64.js — `APS.fromFile`'s record loop].
- **Checksums / pre-apply verification**: the N64 header embeds the Cart ID, country code and the
  console's own 8-byte CRC lifted straight from the original ROM header — not a checksum of the whole
  file, but of the *console header fields* the N64 boot ROM itself checks, used by APS as a "is this the
  right cartridge" gate before applying
  [Source: UniPatcher wiki — APS (N64), "Introduction" ("Target file verification")]
  [Source: marcrobledo/RomPatcher.js (4f17297c):…aps_n64.js — `APS.prototype.validateSource`, comparing
  `header.cartId` and `header.crc` against the live ROM at fixed offsets `0x3c`/`0x10`]. There is no
  checksum of the target file or of the patch itself. Type-0 (simple) patches carry none of this.
- **Size limits**: 32-bit offsets/sizes (up to ~2 GiB), explicitly called out in the original draft as
  the reason APS exists ("ips files have problems with patching files that are greater than 16Mb")
  [Source: UniPatcher wiki — APS (N64), "Introduction"].
- **Reversibility**: one-directional; nothing in the record stream stores the bytes it overwrites.
- **Header-sensitivity**: APS(N64) is itself built around the N64 cartridge header (Cart ID/CRC at fixed
  ROM offsets `0x3c`/`0x10`), so it is inherently header-aware for N64, but has no SNES/NES handling
  (different console, not applicable).
- **Metadata**: the 50-byte description field only.

### APS (GBA)

Documented directly on the UniPatcher wiki (no equivalent archived 1998-era draft is known to exist for
this variant — it is GBA-specific and considerably newer than APS (N64)):

- **Header/magic**: 4-byte ASCII `"APS1"` (distinct from N64's `"APS10"`), then a 4-byte original file
  size and a 4-byte modified file size [Source: UniPatcher wiki — APS (GBA), "Header"].
- **Records**: fixed-size, one per 64 KiB block of the ROM — a 4-byte offset, a 2-byte CRC16 of that
  64 KiB block from the *original* file, a 2-byte CRC16 of the same block from the *modified* file, then
  64 KiB of XOR-of-original-and-modified bytes [Source: UniPatcher wiki — APS (GBA), "Record"]
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.aps_gba.js —
  `APS_GBA_RECORD_SIZE = 4+2+2+APS_GBA_BLOCK_SIZE`].
- **Checksums**: a per-block CRC16 of **both** the source and the target — the only format surveyed here
  that checks the target incrementally, per block, during application rather than only via one whole-file
  footer checksum. `validateSource` checks every record's source CRC16 before applying; `apply` can
  optionally re-check each block's target CRC16 immediately after XOR-ing it
  [Source: marcrobledo/RomPatcher.js (4f17297c):…aps_gba.js — `APSGBA.prototype.validateSource`,
  `APSGBA.prototype.apply`]. No whole-file or patch-self checksum exists.
- **Size limits**: none inherent beyond 32-bit sizes/offsets; practically bounded by GBA ROM sizes
  (≤32 MiB), which is why 64 KiB block granularity was chosen.
- **Reversibility**: the XOR-based record is self-inverse in principle (`modified ⊕ (original ⊕ modified)
  = original`), but this implementation's `apply()` only ever computes `romFile ⊕ xorBytes` in one
  direction (source → target) and does not implement an "undo" path the way the PPF3/UPS code paths do
  [Source: marcrobledo/RomPatcher.js (4f17297c):…aps_gba.js — `APSGBA.prototype.apply`].
- **Header-sensitivity**: none GBA-specific beyond the per-block CRC16 scheme itself (GBA carts have no
  copier-header convention analogous to SNES/NES).
- **Metadata**: none.

## 7. Optional extras: RUP (NINJA2) and bsdiff (BSDIFF40)

### RUP / NINJA2

- **Header/magic**: 6-byte ASCII `"NINJA2"`, then a large fixed-layout text header: 1 text-encoding byte,
  an 84-byte author field, an 11-byte version field, a 256-byte title, a 48-byte genre, a 48-byte
  language, an 8-byte date, a 512-byte web URL and a 1074-byte free-text description, padded to a fixed
  0x800-byte header region before the first command
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.rup.js —
  `RUP.fromFile`'s field reads and the `file.seek(0x800)` that follows them]. Per-file sections begin with
  an `0x01` ("open new file") command carrying a variable-length-encoded filename, a 1-byte ROM-type
  enum (raw/nes/fds/snes/n64/gb/sms/mega/pce/lynx), variable-length-encoded source and target sizes, and
  **16-byte source MD5 and 16-byte target MD5** digests, each written byte-by-byte
  [Source: …rup.js — the `RUP_COMMAND_OPEN_NEW_FILE` branch].
- **Checksums**: whole-file MD5 for both source and target — the only format surveyed here that uses MD5
  rather than a CRC. `RUP.prototype.validateSource` hashes the candidate ROM and matches it against either
  digest, also telling the caller whether the match was against the source (apply) or target (undo) MD5
  [Source: …rup.js — `RUP.prototype.validateSource`]. No patch-self checksum exists.
  Records are `(offset, xor-bytes)` pairs (XOR against the source, like UPS/APS-GBA)
  [Source: …rup.js — `RUP.prototype.apply`'s per-record XOR loop].
- **Reversibility**: bidirectional, the same way PPF3's undo data and UPS's size-swap work — if the
  supplied ROM's MD5 matches the *target* digest instead of the source digest, `apply()` XORs in the
  reverse direction and reconstructs the source, also handling an optional "overflow" trailer
  (XOR-0xFF-encoded appended/truncated bytes for files whose source and target sizes differ)
  [Source: …rup.js — the `undo` branch and `overflowMode` handling in `RUP.prototype.apply`].
- **Header-sensitivity**: RUP's own `romType` enum includes `nes`/`snes` as explicit values but the parser
  shown does not itself strip or re-add SNES/NES headers; any such handling would be caller-side (this
  implementation accepts a `headerSize` elsewhere in RomPatcher.js's generic size-skip path, not shown
  here).
- **Metadata**: by far the richest of any format surveyed — author, version, title, genre, language, date,
  web URL and a free-text description are first-class header fields, not an attached freeform blob like
  BPS's manifest [Source: …rup.js — `RUP.prototype.toString`].

### bsdiff (BSDIFF40)

- **Header/magic**: 8-byte ASCII `"BSDIFF40"`, then three 8-byte little-endian lengths: the bzip2-compressed
  control-block length, the bzip2-compressed diff-block length, and the uncompressed size of the patched
  (target) file; the remainder of the file is the bzip2-compressed diff block followed by the
  bzip2-compressed extra block, with the control block's length used to locate the diff block's start and
  everything after it treated as the extra block
  [Source: marcrobledo/RomPatcher.js (4f17297c):rom-patcher-js/modules/RomPatcher.format.bdf.js —
  `BDF.fromFile`, citing "https://www.daemonology.net/bsdiff/" as the format's own reference]. Each
  decompressed control-block record is three 8-byte little-endian integers: a diff-block length, an
  extra-block length, and a signed "skip" distance in the source file (sign carried in the top bit)
  [Source: …bdf.js — the `controlFile.readU64()` triple and the `skip&(1<<63)` sign handling].
- **Apply algorithm**: for each control record, add `diffLen` bytes of the diff block, byte-by-byte, to
  the next `diffLen` bytes read from the source file; append `extraLen` literal bytes from the extra
  block; then seek the source cursor by `skip`
  [Source: …bdf.js — `BDF.prototype.apply`].
- **Checksums**: none at all — no source, target or patch-self checksum anywhere in BSDIFF40. The only
  implicit size check is that applying terminates once `patchedSize` bytes have been written.
- **Reversibility**: one-directional; nothing in the format stores the original bytes it overwrote.
- **Header-sensitivity**: none; bsdiff is a general-purpose binary-diff tool with no ROM-specific
  conventions, so any SNES/NES header handling is entirely the caller's responsibility, outside the
  format.
- **Metadata**: none.

## Comparison table

| Format | Magic/Header | Source checksum | Target checksum | Patch checksum | Size limits | Metadata | Typical platforms |
|---|---|---|---|---|---|---|---|
| IPS | `"PATCH"` (5B) | none | none | none | 24-bit offset/size (~16 MiB) | none (EBP extension adds trailing JSON) | SNES, Genesis, NES |
| IPS32 | `"IPS32"` (5B) | none | none | none | 32-bit offset/size (~4 GiB) | none | larger/modern ROM hacks |
| UPS | `"UPS1"` (4B) | CRC32 | CRC32 | CRC32 | none (varint-encoded) | none | SNES and general, byuu tooling |
| BPS | `"BPS1"` (4B) | CRC32 | CRC32 | CRC32 | none (varint-encoded) | freeform blob, conventionally XML | SNES and general, byuu tooling |
| VCDIFF/xdelta3 | `0xD6 0xC3 0xC4` + version byte | none (RFC); whole-file BLAKE3 via xdelta3 "armor" (non-RFC) | per-window Adler32 (`VCD_ADLER32`, non-RFC); whole-file BLAKE3 via armor | none | none (64 MiB default per-window cap in xdelta3) | free-form application header | general-purpose, PC/console disk images |
| PPF (1/2/3) | `"PPF"` + 2-digit version | none (1024-byte raw "block check" region, unenforced in the surveyed reimplementation) | none | none | PPF1/2: 32-bit offsets; PPF3: 64-bit offsets, 255-byte records | 50-byte description; PPF3 optional FILE_ID.DIZ block | PSX/PS2 disc images |
| APS (N64) | `"APS10"` (5B) | N64 cartridge header fields (Cart ID, country, 8-byte console CRC), not a whole-file hash | none | none | 32-bit offset/size (~2 GiB) | 50-byte description | N64 |
| APS (GBA) | `"APS1"` (4B) | per-64KiB-block CRC16 | per-64KiB-block CRC16 | none | 32-bit sizes (practically ≤32 MiB) | none | GBA |
| RUP (NINJA2) | `"NINJA2"` (6B) | MD5 (whole file) | MD5 (whole file) | none | varint-encoded sizes | author/version/title/genre/language/date/web/description | multi-console (NES/FDS/SNES/N64/GB/SMS/Genesis/PCE/Lynx) |
| bsdiff (BSDIFF40) | `"BSDIFF40"` (8B) | none | none | none | 64-bit lengths; bzip2-compressed blocks | none | general-purpose binary diffing |

## BPS footer (12 bytes trailing the patch)

Per `bps_spec.md`'s "Footer" section and matched exactly by `bps_apply`/`bps_create_linear`
[Source: Alcaro/Flips (ff216a75):bps_spec.md; libbps.cpp]:

```c
// last 12 bytes of a BPS1 file, in this exact order
uint32_t source_checksum;  // CRC32 of the entire source (input) file
uint32_t target_checksum;  // CRC32 of the entire target (output) file
uint32_t patch_checksum;   // CRC32 of every byte of this patch EXCEPT these final 4 bytes
```

## UPS footer (12 bytes trailing the patch)

Read directly from `ups_apply`/`ups_create`'s footer handling — the same three-CRC32, same field
order and same sizes as BPS, though no standalone byuu `ups_spec` document could be located to confirm
this was authored identically on purpose (see Gaps)
[Source: Alcaro/Flips (ff216a75):libups.cpp]:

```c
// last 12 bytes of a UPS1 file, in this exact order
uint32_t source_checksum;  // CRC32 of the source file (or, if roles are swapped for an "undo", of
                           //   whichever of the two recorded sizes matches the supplied file)
uint32_t target_checksum;  // CRC32 of the target file
uint32_t patch_checksum;   // CRC32 of every byte of this patch EXCEPT these final 4 bytes
```

## Gaps / uncertainties

- **No standalone UPS spec document was found.** Unlike BPS's `bps_spec.md` (public domain, byuu-authored,
  preserved in the Flips repository), UPS's field layout here is read entirely from Flips' `libups.cpp`/
  `libups.h` reference implementation. The three-CRC32 footer and varint encoding plausibly mirror BPS
  because byuu authored both, but this is **inferred from matching implementation behaviour**, not
  confirmed against byuu's own prose.
- **PPF's original spec document ("PPF 3.0 spec") could not be located**, live or via web.archive.org,
  within this research session. The romhacking.net utility page cited in RomPatcher.js's own source
  comment (`https://www.romhacking.net/utilities/353/`) is itself a download listing, not spec text, and
  was not independently re-verified here. This section relies entirely on RomPatcher.js's reimplementation,
  which is internally consistent but may not capture every PPF1/PPF2 edge case (e.g., PPF1's exact header
  layout is not separately exercised by the code read — the `version===1` case falls through using the
  PPF1/2-shared defaults with no distinct branch).
- **RomPatcher.js's `PPF.prototype.apply` does not enforce the 1024-byte "block check" field** against the
  ROM being patched, even though it is parsed; whether other PPF implementations (e.g. the original Windows
  PPF3 patcher) do enforce it could not be confirmed from a primary source in this session.
  This is noted as a property of the *reimplementation surveyed*, not necessarily of the format.
  - **APS (N64)'s Cart ID field width is inconsistent between sources.** The archived 1998 draft
  (`aps.txt`, via the UniPatcher wiki) describes a 2-byte Cart ID at bytes 58–59, while
  RomPatcher.js's `aps_n64.js` reads 3 bytes. This discrepancy was not resolved by finding a byte-exact
  authoritative source for one over the other; both are cited as read.
- **xdelta3's "armor mode" (BLAKE3 whole-file digests) is a very recent addition** relative to RFC 3284
  (2002): the site docs read here come from the `jmacd/xdelta` `main` branch as fetched 2026-11-14, so
  older xdelta3 builds in the wild will not have it, and its application-header encoding
  (`name#blake3hex`) is specific to this implementation, not a VCDIFF standard.
  - **RUP/NINJA2's generic ROM-header handling** (the `headerSize` parameter referenced in RomPatcher.js's
  shared `RomPatcher.validateRom`/apply path) was not traced into this file in detail; only the
  format-specific module (`RomPatcher.format.rup.js`) was read for this note, so whether RUP's own patch
  data is itself header-size-sensitive (vs. that being handled generically by the host app) is not fully
  confirmed from the format module alone.
- The Alcaro/Flips GitHub repository was renamed/redirected to `Sir-Walrus/Flips` during this research
  window (visible via the repository's own README, which is itself a 2025 protest statement about AI
  contributions, not format documentation); the code content read here is unaffected, but the canonical
  repository identity for future re-verification may differ from `Alcaro/Flips` by the time this note is
  reread.

## Sources

- IPS: [zerosoft.zophar.net/ips.php](https://zerosoft.zophar.net/ips.php) (Z.e.r.o's IPS spec, fetched
  live 2026-11-14, no archive snapshot needed); Alcaro/Flips at `ff216a75df0987047a67d7923567dc4482ce07ac`
  (repository now also reachable as `Sir-Walrus/Flips`, same commit):
  [libips.cpp](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/libips.cpp),
  [flips.cpp](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/flips.cpp).
- IPS32: Nifyr/ips32 at
  [`7c48b06d745ecc9b35a7cc1365a55d2f1fae55e2`](https://github.com/Nifyr/ips32/blob/7c48b06d745ecc9b35a7cc1365a55d2f1fae55e2/src/lib.rs)
  (a fork of euclio/ips).
- UPS and BPS: Alcaro/Flips at `ff216a75df0987047a67d7923567dc4482ce07ac`:
  [bps_spec.md](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/bps_spec.md),
  [libbps.cpp](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/libbps.cpp),
  [libbps.h](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/libbps.h),
  [libbps-suf.cpp](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/libbps-suf.cpp),
  [libups.cpp](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/libups.cpp),
  [libups.h](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/libups.h),
  [flips.h](https://raw.githubusercontent.com/Alcaro/Flips/ff216a75df0987047a67d7923567dc4482ce07ac/flips.h).
- xdelta3/VCDIFF: [RFC 3284](https://www.rfc-editor.org/rfc/rfc3284); jmacd/xdelta at
  [`5d936d2c30f78729375c3ae070248a05bef27bd2`](https://github.com/jmacd/xdelta/tree/5d936d2c30f78729375c3ae070248a05bef27bd2)
  (fetched as the repository's `HEAD` on 2026-11-14):
  [xdelta3.c](https://github.com/jmacd/xdelta/blob/5d936d2c30f78729375c3ae070248a05bef27bd2/xdelta3/xdelta3.c)
  (`VCD_ADLER32` definition),
  [xdelta3-decode.h](https://github.com/jmacd/xdelta/blob/5d936d2c30f78729375c3ae070248a05bef27bd2/xdelta3/xdelta3-decode.h),
  [xdelta3.h](https://raw.githubusercontent.com/jmacd/xdelta/5d936d2c30f78729375c3ae070248a05bef27bd2/xdelta3/xdelta3.h),
  [site/docs/armor.md](https://raw.githubusercontent.com/jmacd/xdelta/5d936d2c30f78729375c3ae070248a05bef27bd2/site/docs/armor.md),
  [site/docs/checksum-mismatch.md](https://raw.githubusercontent.com/jmacd/xdelta/5d936d2c30f78729375c3ae070248a05bef27bd2/site/docs/checksum-mismatch.md).
- PPF, APS (N64/GBA), RUP, bsdiff: marcrobledo/RomPatcher.js at
  [`4f17297cd1dc0835a0b0747497fa15f5f63f1623`](https://github.com/marcrobledo/RomPatcher.js/tree/4f17297cd1dc0835a0b0747497fa15f5f63f1623):
  [RomPatcher.format.ppf.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/modules/RomPatcher.format.ppf.js),
  [RomPatcher.format.aps_n64.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/modules/RomPatcher.format.aps_n64.js),
  [RomPatcher.format.aps_gba.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/modules/RomPatcher.format.aps_gba.js),
  [RomPatcher.format.rup.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/modules/RomPatcher.format.rup.js),
  [RomPatcher.format.bdf.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/modules/RomPatcher.format.bdf.js),
  [RomPatcher.format.ips.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/modules/RomPatcher.format.ips.js),
  [RomPatcher.js](https://raw.githubusercontent.com/marcrobledo/RomPatcher.js/4f17297cd1dc0835a0b0747497fa15f5f63f1623/rom-patcher-js/RomPatcher.js)
  (header-size table).
- APS (N64/GBA) upstream spec text: UniPatcher wiki, cited directly from RomPatcher.js's own source
  comments — [APS (N64)](https://github.com/btimofeev/UniPatcher/wiki/APS-(N64)) (reproducing the original
  1998 `aps.txt` and `xPSapply.txt` draft by Silo/Fractal, Blackbag), [APS
  (GBA)](https://github.com/btimofeev/UniPatcher/wiki/APS-(GBA)) (last edited by Boris Timofeev, 2017-01-09).
- bsdiff's own format reference, cited directly in RomPatcher.js's source comment:
  [daemonology.net/bsdiff](https://www.daemonology.net/bsdiff/) (Colin Percival's original bsdiff page; not
  independently re-fetched in this session — see Gaps for what was and was not separately verified).
