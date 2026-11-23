"""Keystroke-to-render time of a list filter over synthetic Entries, in headless Chromium.

Not a CI gate. Needs cargo-leptos, the wasm32-unknown-unknown target and Playwright for Python. Run it from the
repository's root:

    python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium
    .venv/bin/python crates/ui/bench/filter.py [--entries 100000] [--rounds 5] [--meter | --repair]

The `/bench` screen exists only with the workbench's `bench` feature, which no release or image has. So the script
builds the workbench with it into target/bench/site, runs the Service on a loopback port with a scratch store and
a dev identity under target/bench, measures, and stops the Service. With `--url`, it measures a Service already
serving a `bench` build instead. The screen renders the Audit screen's Entries panel, in the shared list every list
screen uses (`parts::list`), over deterministic synthetic Entries, or with `--repair` a repair plan of as many items
in the Repair tab's review (the shared plan review); `--meter` shows the same Entries in a plain shared list whose
rows each hold a segmented meter (`parts::meter`), to measure the meter's cost. It reads nothing from the Service. It
also reports how long the screen took to show its first rows. Each sample starts at the keydown's own timestamp and
ends after the first frame painted once the shown count reflects the new text and every rendered row matches it. The
shared list keeps a row's element while the row stays shown, so its rows need only match; a repair plan's rows also
match on their kind, which shows only as an icon, so there the shown count alone says the result is drawn.
"""

import argparse
import json
import os
import socket
import statistics
import subprocess
import time
import urllib.request
from contextlib import contextmanager
from pathlib import Path

from playwright.sync_api import sync_playwright

# Typed one key at a time, then erased one key at a time; each key is one sample.
QUERIES = ["river raid", "pit", "usa", "00042", "zzz"]

SETUP = """
() => {
  window.__samples = [];
  document.querySelector('.filter-text').addEventListener('keydown', (e) => {
    window.__down = e.timeStamp;
    window.__before = document.querySelector('p.shown').dataset.shown;
  }, { capture: true });
}
"""

# The shared list's rendered rows.
LIST_ROWS = ".list-body [role=row][id]"

# Resolves with the keystroke-to-paint time once the list shows `query`'s result.
SETTLED = """
([query, repair]) => new Promise((resolve) => {
  const q = query.toLowerCase();
  const done = () => {
    const shown = document.querySelector('p.shown');
    if (shown.dataset.query !== query) return false;
    // One key more or less narrows or widens the result, so the same count is the same Entries: nothing to redraw.
    if (shown.dataset.shown === window.__before) return true;
    const rows = [...document.querySelectorAll('.list-body [role=row][id]')];
    if (Number(shown.dataset.shown) > 0 && rows.length === 0) return false;
    // A repair row also matches on its kind, shown only as an icon, so its text needn't hold the query.
    if (repair) return true;
    return rows.every((r) => r.textContent.toLowerCase().includes(q));
  };
  const poll = () => {
    if (!done()) { requestAnimationFrame(poll); return; }
    // The frame after the one that rendered the result has been painted.
    requestAnimationFrame(() => setTimeout(() => resolve(performance.now() - window.__down), 0));
  };
  poll();
})
"""


def percentile(samples, p):
    ordered = sorted(samples)
    return ordered[min(len(ordered) - 1, round(p / 100 * (len(ordered) - 1)))]


@contextmanager
def bench_service():
    """Builds the workbench with the `bench` feature and serves it; yields the Service's URL."""
    work = Path("target/bench").resolve()
    site = work / "site"
    subprocess.run(
        ["cargo", "leptos", "build", "--release", "--lib-features", "bench"],
        check=True,
        env={**os.environ, "LEPTOS_SITE_ROOT": str(site)},
    )
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    # Cargo's target directory, which CARGO_TARGET_DIR or a cargo config may move from ./target.
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"], check=True, capture_output=True, text=True
    )
    binary = Path(json.loads(metadata.stdout)["target_directory"]) / "release" / "stage-select"
    service = subprocess.Popen(
        [str(binary), "--store", str(work / "store"), "serve", "--listen", f"127.0.0.1:{port}",
         "--ui", str(site), "--dev-identity", "Benchmark"]
    )
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"{url}/api/v1/health")
                break
            except OSError:
                time.sleep(0.1)
        yield url
    finally:
        service.terminate()
        service.wait()


@contextmanager
def given(url):
    yield url


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", help="a Service already serving a `bench` build of the workbench")
    parser.add_argument("--entries", type=int, default=100_000)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--repair", action="store_true", help="measure a repair plan's review")
    parser.add_argument("--meter", action="store_true", help="measure a plain shared list with a segmented meter in each row")
    args = parser.parse_args()

    with (given(args.url) if args.url else bench_service()) as url:
        samples, version = measure(url, args)
    what = (
        "a repair plan's review" if args.repair
        else "a shared list with a meter in each row" if args.meter
        else "the Entries panel"
    )
    print(f"{len(samples)} keystrokes after one warm-up round over {args.entries} rows in {what}; {version}")
    print(f"p50 {statistics.median(samples):.1f} ms, p95 {percentile(samples, 95):.1f} ms, max {max(samples):.1f} ms")


def measure(url, args):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        mode = "&repair=1" if args.repair else "&meter=1" if args.meter else ""
        started = time.monotonic()
        page.goto(f"{url}/bench?entries={args.entries}{mode}")
        page.wait_for_selector(LIST_ROWS, timeout=120_000)
        print(f"{page.text_content('p.shown')} loaded in {(time.monotonic() - started) * 1000:.0f} ms")
        version = browser.version
        page.evaluate(SETUP)
        field = page.locator(".filter-text")
        field.focus()
        samples = []
        for round_ in range(args.rounds):
            for query in QUERIES:
                for i in range(1, len(query) + 1):
                    page.keyboard.press(query[i - 1])
                    sample = page.evaluate(SETTLED, [query[:i], args.repair])
                    if round_ > 0:
                        samples.append(sample)
                for i in range(len(query) - 1, -1, -1):
                    page.keyboard.press("Backspace")
                    sample = page.evaluate(SETTLED, [query[:i], args.repair])
                    if round_ > 0:
                        samples.append(sample)
        browser.close()
    return samples, version


if __name__ == "__main__":
    main()
