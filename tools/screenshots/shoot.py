"""Screenshots of workbench screens over a synthetic store, in both themes and at two widths, for a PR.

Not a CI gate. Needs cargo-leptos, the wasm32-unknown-unknown target, Playwright for Python, and what
tools/clone-lists/fetch needs (curl, tar, sha256sum). Run it from the repository's root:

    python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium
    .venv/bin/python tools/screenshots/shoot.py                       # this tree, as "after"
    .venv/bin/python tools/screenshots/shoot.py --ref main            # main, as "main"
    .venv/bin/python tools/screenshots/shoot.py --screens source-sets,activity --label after
    .venv/bin/python tools/screenshots/shoot.py --route 'kept=/activity/kept' --screens kept
    .venv/bin/python tools/screenshots/shoot.py --list                # the screens it knows, and stops

It builds the workbench and the Service with `cargo leptos build --release` (with `--ref`, from that git ref checked
out in a temporary git worktree, which it removes once built), runs the Service on a loopback port with a scratch
store and a dev identity, seeds the store through the Service's API, and captures each screen in the light and dark
themes at 1440px and 1000px wide, full page, to target/screenshots/<label>/<screen>-<theme>-<width>.png. The label is
"after" for this tree and the ref's name for `--ref`, so `--ref main` and a plain run give a before/after pair over
the same seed. It stops the Service when done. It reports any request the workbench sends to another origin, which
should be none, and then exits non-zero. Each screenshot shows the scratch folder's path as /srv/stage-select, so
it shows no local path.

The seed is synthetic: DATs written here in Logiqx XML with invented game titles, and source set folders of ZIPs whose
checksums match them. It never reads a real DAT, a ROM or the Host. It seeds, all under
target/screenshots/.work/<label>/:

- roots: `sets` (read-only, but repairs may write to the Game Boy source set), `play` (writable) and `drop` (ingest);
- four DATs and four source sets: Atari - 2600, complete; Nintendo - Game Boy, with a misnamed file, a missing game,
  a file of wrong content and an unknown file, given a full audit and a repair proposal; Sega - Mega Drive - Genesis,
  missing two games; NEC - PC Engine - TurboGrafx-16, audited once, then its folder removed and audited again, so
  Activity lists a failed job;
- a selection profile, Everyday, with a selection of the Atari and the Game Boy source sets;
- two library folders: play/atari-2600, its plan approved and built (so READY shows), and play/game-boy, its plan
  waiting for approval;
- the ingest folder `drop`, scanned, holding a file that a source set is missing and one nobody knows.

It doesn't seed an additions root, curation, cue packs, RomM, notifications, a drifted or stale library folder, a
DAT fetch, kept files from an approved repair or ingest plan, or a running or interrupted job. A step the Service
refuses (as an older ref may) is reported and skipped, and so is any screen whose route needs what it would have
made.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target" / "screenshots"
WORK = OUT / ".work"

THEMES = ["light", "dark"]
WIDTHS = [1440, 1000]
HEIGHT = 900

# Each screen's route; `{…}` names what the seed made, and a screen whose name the seed didn't make is skipped.
SCREENS = {
    "home": "/",
    "source-sets": "/source-sets",
    "source-set": "/source-sets/{findings_set}",
    "source-set-repair": "/source-sets/{findings_set}?tab=repair",
    "dats": "/dats",
    "audit": "/audits/{audit}",
    "profiles": "/profiles",
    "profile": "/profiles/Everyday",
    "selection": "/selections/{selection}",
    "libraries": "/libraries",
    "library": "/libraries/{library}",
    "library-plan": "/libraries/{library}?tab=plan",
    "library-pending-plan": "/libraries/{pending_library}?tab=plan",
    "build": "/builds/{build}",
    "ingest": "/ingest",
    "ingest-folder": "/ingest/drop",
    "activity": "/activity",
    "roots": "/settings/roots",
    "about": "/settings/about",
}

ATARI = "Atari - 2600"
GAME_BOY = "Nintendo - Game Boy"
MEGA_DRIVE = "Sega - Mega Drive - Genesis"
PC_ENGINE = "NEC - PC Engine - TurboGrafx-16"


def payload(name):
    """A ROM's synthetic content: a few KiB that only `name` has."""
    seed = hashlib.sha256(name.encode()).digest()
    return seed * (64 + seed[0] % 64)


# Each system's DAT: its ROM extension and its games, each a name and the parent it is a clone of.
SYSTEMS = {
    ATARI: (".a26", [
        ("Asteroid Miner (USA)", None),
        ("Asteroid Miner (Europe)", "Asteroid Miner (USA)"),
        ("Canyon Glider (USA)", None),
        ("Harbor Patrol (USA) (Demo)", None),
        ("Lunar Courier (USA, Europe)", None),
        ("Night Market (USA) (Beta)", None),
    ]),
    GAME_BOY: (".gb", [
        ("Pocket Ranger (USA)", None),
        ("Pocket Ranger (Japan)", "Pocket Ranger (USA)"),
        ("Pocket Ranger 2 (USA)", None),
        ("Tile Tumbler (World)", None),
        ("Moss Garden (Europe) (En,Fr,De)", None),
        ("Signal Tower (USA) (Rev 1)", None),
        ("Signal Tower (USA)", "Signal Tower (USA) (Rev 1)"),
        ("Deep Current (Japan)", None),
    ]),
    MEGA_DRIVE: (".md", [
        ("Blade Circuit (USA)", None),
        ("Blade Circuit (Europe)", "Blade Circuit (USA)"),
        ("Cloud Fortress (World)", None),
        ("Iron Meadow (USA)", None),
        ("Rally Spirits (Japan)", None),
        ("Velvet Engine (USA, Europe)", None),
    ]),
    PC_ENGINE: (".pce", [
        ("Comet Dancer (Japan)", None),
        ("Comet Dancer (USA)", "Comet Dancer (Japan)"),
        ("Paper Kingdom (Japan)", None),
    ]),
}

# The Game Boy source set's flaws, and the Mega Drive one's missing games.
GB_MISNAMED = "Tile Tumbler (World)"
GB_MISSING = "Pocket Ranger 2 (USA)"
GB_WRONG = "Deep Current (Japan)"
MD_MISSING = {"Iron Meadow (USA)", "Rally Spirits (Japan)"}


def dat_xml(system, version):
    ext, games = SYSTEMS[system]
    out = [
        '<?xml version="1.0"?>',
        "<datafile>",
        "  <header>",
        f"    <name>{system}</name>",
        f"    <version>{version}</version>",
        "    <homepage>No-Intro</homepage>",
        "  </header>",
    ]
    for game, parent in games:
        rom = game + ext
        p = payload(rom)
        cloneof = f' cloneof="{parent}"' if parent else ""
        out.append(f'  <game name="{game}"{cloneof}>')
        out.append(
            f'    <rom name="{rom}" size="{len(p)}" crc="{zlib.crc32(p):08x}" md5="{hashlib.md5(p).hexdigest()}"'
            f' sha1="{hashlib.sha1(p).hexdigest()}"/>'
        )
        out.append("  </game>")
    out.append("</datafile>")
    return ("\n".join(out) + "\n").encode()


def write_zip(path, name, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0)), content)


def write_set(folder, system):
    """Writes `system`'s source set folder, flawed as the seed says."""
    ext, games = SYSTEMS[system]
    folder.mkdir(parents=True, exist_ok=True)
    for game, _ in games:
        rom = game + ext
        if (system, game) == (GAME_BOY, GB_MISSING) or (system == MEGA_DRIVE and game in MD_MISSING):
            continue
        content, name = payload(rom), game
        if (system, game) == (GAME_BOY, GB_MISNAMED):
            name = "Tile Tumbler (W) [!]"
        if (system, game) == (GAME_BOY, GB_WRONG):
            content = payload("overdump of " + rom)
        write_zip(folder / f"{name}.zip", rom, content)
    if system == GAME_BOY:
        (folder / "readme.txt").write_text("Notes kept beside the set.\n")


class Api:
    def __init__(self, base):
        self.base = base + "/api/v1"

    def call(self, method, path, body=None, raw=None, content_type=None):
        data, headers = None, {}
        if raw is not None:
            data, headers["Content-Type"] = raw, content_type
        elif body is not None:
            data, headers["Content-Type"] = json.dumps(body).encode(), "application/json"
        request = urllib.request.Request(self.base + path, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request) as response:
                text = response.read()
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{method} {path}: {e.code} {e.read().decode(errors='replace')[:400]}") from None
        return json.loads(text) if text else None

    def get(self, path):
        return self.call("GET", path)

    def post(self, path, body=None):
        return self.call("POST", path, body if body is not None else {})

    def upload_dat(self, file, content):
        boundary = "stage-select-screenshots"
        body = (
            f'--{boundary}\r\nContent-Disposition: form-data; name="source_url"\r\n\r\n\r\n'
            f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{file}"\r\n'
            "Content-Type: application/xml\r\n\r\n"
        ).encode() + content + f"\r\n--{boundary}--\r\n".encode()
        return self.call("POST", "/dat-revisions", raw=body, content_type=f"multipart/form-data; boundary={boundary}")

    def follow(self, job):
        """Waits for the job to end, and returns it."""
        job_id = job["id"] if isinstance(job, dict) else job
        for _ in range(1200):
            job = self.get(f"/jobs/{job_id}")
            if job["state"] in ("succeeded", "failed", "interrupted"):
                return job
            time.sleep(0.1)
        raise RuntimeError(f"job {job_id} didn't end")


def seed(api, data):
    """Seeds the store through the API; returns what the routes name, and the steps it skipped."""
    made, failures = {}, []

    def step(what, fn):
        try:
            return fn()
        except Exception as e:  # An older ref may refuse a step; the rest still seed.
            failures.append(what)
            print(f"seed: skipped {what}: {e}", file=sys.stderr)
            return None

    sets, play, drop = data / "sets", data / "play", data / "drop"
    for system in SYSTEMS:
        write_set(sets / system, system)
    play.mkdir(parents=True)
    drop.mkdir(parents=True)

    step("root sets", lambda: api.post("/roots", {
        "name": "sets", "path": str(sets), "kind": "general", "writable": False, "writable_paths": [GAME_BOY]}))
    step("root play", lambda: api.post("/roots", {
        "name": "play", "path": str(play), "kind": "general", "writable": True}))
    step("root drop", lambda: api.post("/roots", {
        "name": "drop", "path": str(drop), "kind": "ingest", "writable": True}))

    for system in SYSTEMS:
        step(f"DAT {system}", lambda s=system: api.upload_dat(
            f"{s} (20260101-000000).dat", dat_xml(s, "20260101-000000")))
        step(f"source set {system}", lambda s=system: api.post("/source-sets", {
            "root": "sets", "path": s, "dat": f"No-Intro/{s}"}))

    def audit(system, evidence=None):
        body = {"source_set": system}
        if evidence:
            body["evidence"] = evidence
        job = api.follow(api.post("/audits", body))
        if job["state"] != "succeeded":
            raise RuntimeError(f"the audit {job['state']}")
        return job

    for system in (ATARI, MEGA_DRIVE, PC_ENGINE):
        step(f"audit {system}", lambda s=system: audit(s))
    job = step(f"full audit {GAME_BOY}", lambda: audit(GAME_BOY, "full"))
    if job:
        made["findings_set"] = urllib.parse.quote(GAME_BOY)
        made["audit"] = job.get("audit")
    step(f"repair {GAME_BOY}", lambda: api.post(f"/source-sets/{urllib.parse.quote(GAME_BOY)}/repairs"))

    def profile():
        saved = api.post("/profiles/Everyday/versions", {
            "profile": {}, "base_version": None,
            "queue": [{"source_set": ATARI, "revision": None}, {"source_set": GAME_BOY, "revision": None}],
        })
        selections = {}
        for job in saved["jobs"]:
            job = api.follow(job)
            if job["state"] != "succeeded":
                raise RuntimeError(f"selection job {job['id']} {job['state']}")
            record = api.get(f"/selections/{job['selection']}")
            selections[record["source_set"]] = record["id"]
        return selections

    selections = step("selection profile Everyday and its selections", profile) or {}
    if ATARI in selections:
        made["selection"] = selections[ATARI]

    def library(system, path, approve):
        folder = api.post("/libraries", {"root": "play", "path": path, "source_set": system, "profile": "Everyday"})
        plan = api.post(f"/libraries/{folder['id']}/plans", {"selection": selections[system]})
        if not approve:
            return folder["id"], None
        job = api.follow(api.post(f"/plans/{plan['id']}/approval", {"digest": plan["digest"]}))
        if job["state"] != "succeeded":
            raise RuntimeError(f"build {job['id']} {job['state']}")
        return folder["id"], job["id"]

    built = step("library folder play/atari-2600, built", lambda: library(ATARI, "atari-2600", True))
    if built:
        made["library"], made["build"] = built
    pending = step("library folder play/game-boy, planned", lambda: library(GAME_BOY, "game-boy", False))
    if pending:
        made["pending_library"] = pending[0]

    def ingest():
        api.call("PATCH", "/settings", {"ingest_folders": {"drop": {"settle_secs": 0}}})
        api.post("/ingest-folders", {"root": "drop"})
        rom = GB_MISSING + ".gb"
        (drop / rom).write_bytes(payload(rom))
        (drop / "unsorted-0412.bin").write_bytes(payload("nobody knows"))
        # The first scan sees each file; the second, with it settled, assesses it.
        for _ in range(2):
            api.follow(api.post("/ingest-folders/drop/scans"))

    step("ingest folder drop", ingest)

    def failed_audit():
        shutil.rmtree(sets / PC_ENGINE)
        job = api.follow(api.post("/audits", {"source_set": PC_ENGINE}))
        if job["state"] != "failed":
            raise RuntimeError(f"the audit {job['state']}")

    step("a failed audit", failed_audit)
    return made, failures


def build(tree, site, target):
    """Builds the workbench into `site` and the Service under `target`; returns the Service's binary."""
    subprocess.run(
        ["cargo", "leptos", "build", "--release"],
        cwd=tree,
        check=True,
        env={**os.environ, "LEPTOS_SITE_ROOT": str(site), "CARGO_TARGET_DIR": str(target)},
    )
    return target / "release" / "stage-select"


def built_ref(ref, label):
    """Builds `ref` from a temporary git worktree, which it removes; returns the binary and the site."""
    tree = WORK / label / "tree"
    if tree.exists():
        subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=ROOT)
    subprocess.run(["git", "worktree", "add", "--detach", str(tree), ref], cwd=ROOT, check=True)
    try:
        site = WORK / label / "site"
        # Shared by every ref, so a later ref's build is incremental.
        return build(tree, site, OUT / ".ref-target"), site
    finally:
        subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=ROOT, check=True)


def clone_lists():
    dest = OUT / ".clone-lists"
    subprocess.run([str(ROOT / "tools/clone-lists/fetch"), str(dest)], cwd=ROOT, check=True)
    return dest


def serve(binary, site, work):
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    log = open(work / "service.log", "w")
    service = subprocess.Popen(
        [str(binary), "--store", str(work / "store"), "serve", "--listen", f"127.0.0.1:{port}", "--ui", str(site),
         "--dev-identity", "Screenshots", "--clone-lists", str(clone_lists())],
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    for _ in range(300):
        try:
            urllib.request.urlopen(f"{url}/api/v1/health")
            return url, service
        except OSError:
            if service.poll() is not None:
                raise RuntimeError(f"the Service stopped; see {work / 'service.log'}")
            time.sleep(0.1)
    service.terminate()
    raise RuntimeError("the Service didn't answer")


# The scratch folder as screenshots show it, so a PR shows no local path.
SHOWN_AS = "/srv/stage-select"

# Replaces the scratch folder's path in the page's text and fields with SHOWN_AS.
MASK = """
([from, to]) => {
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (n.nodeValue.includes(from)) n.nodeValue = n.nodeValue.split(from).join(to);
  }
  for (const f of document.querySelectorAll('input, textarea')) {
    if (f.value.includes(from)) f.value = f.value.split(from).join(to);
    if (f.placeholder.includes(from)) f.placeholder = f.placeholder.split(from).join(to);
  }
  for (const e of document.querySelectorAll('[title]')) {
    if (e.title.includes(from)) e.title = e.title.split(from).join(to);
  }
}
"""

# True once the screen shows no visible loading note or pending status, and its fonts are in.
SETTLED = """
() => document.fonts.status === 'loaded'
  && document.querySelector('main') !== null
  && ![...document.querySelectorAll('.loading-note, .pending-status')].some((e) => e.checkVisibility())
"""


def capture(url, screens, label, work):
    from playwright.sync_api import sync_playwright

    out = OUT / label
    out.mkdir(parents=True, exist_ok=True)
    origin = urllib.parse.urlsplit(url).netloc
    foreign = set()

    def request(r):
        u = urllib.parse.urlsplit(r.url)
        if u.scheme in ("http", "https", "ws", "wss") and u.netloc != origin:
            foreign.add(r.url)

    with sync_playwright() as p:
        browser = p.chromium.launch()
        for theme in THEMES:
            for width in WIDTHS:
                context = browser.new_context(viewport={"width": width, "height": HEIGHT}, reduced_motion="reduce")
                context.add_init_script(f"localStorage.setItem('stage-select.theme', '{theme}')")
                page = context.new_page()
                page.on("request", request)
                for name, route in screens.items():
                    page.goto(url + route, wait_until="load")
                    try:
                        page.wait_for_function(SETTLED, timeout=15000)
                    except Exception:
                        print(f"capture: {name} still loading after 15 s; capturing it as it is", file=sys.stderr)
                    page.wait_for_timeout(500)
                    page.evaluate(MASK, [str(work), SHOWN_AS])
                    path = out / f"{name}-{theme}-{width}.png"
                    page.screenshot(path=str(path), full_page=True)
                    print(path.relative_to(ROOT), flush=True)
                context.close()
        browser.close()
    return foreign


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--ref", help="capture this git ref, built in a temporary worktree, instead of this tree")
    parser.add_argument("--label", help='the folder under target/screenshots; by default "after", or the ref')
    parser.add_argument("--screens", help="comma-separated screen names to capture; by default every one")
    parser.add_argument("--route", action="append", default=[], metavar="NAME=PATH",
                        help="add or replace a screen; PATH may name {findings_set}, {audit}, {selection}, "
                             "{library}, {pending_library} or {build}")
    parser.add_argument("--list", action="store_true", help="list the screens and stop")
    args = parser.parse_args()

    screens = dict(SCREENS)
    for item in args.route:
        name, _, path = item.partition("=")
        if not name or not path.startswith("/"):
            parser.error(f"--route {item!r}: want NAME=/path")
        screens[name] = path
    if args.list:
        for name, route in screens.items():
            print(f"{name:24} {route}")
        return
    if args.screens:
        wanted = [s.strip() for s in args.screens.split(",") if s.strip()]
        unknown = [s for s in wanted if s not in screens]
        if unknown:
            parser.error(f"unknown screens: {', '.join(unknown)}; see --list")
        screens = {s: screens[s] for s in wanted}

    label = args.label or (re.sub(r"[^A-Za-z0-9._-]+", "-", args.ref) if args.ref else "after")
    work = WORK / label
    for scratch in ("store", "data"):
        shutil.rmtree(work / scratch, ignore_errors=True)
    work.mkdir(parents=True, exist_ok=True)

    if args.ref:
        binary, site = built_ref(args.ref, label)
    else:
        site = work / "site"
        binary = build(ROOT, site, Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve())

    url, service = serve(binary, site, work)
    try:
        made, failures = seed(Api(url), work / "data")
        routes = {}
        for name, route in screens.items():
            try:
                routes[name] = route.format(**made)
            except KeyError as e:
                print(f"capture: skipped {name}: the seed made no {e}", file=sys.stderr)
        foreign = capture(url, routes, label, work)
    finally:
        service.terminate()
        service.wait()
    if failures:
        print(f"seed: {len(failures)} step(s) skipped: {', '.join(failures)}", file=sys.stderr)
    if foreign:
        print("The workbench requested another origin:", *sorted(foreign), sep="\n  ", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
