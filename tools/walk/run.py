"""Everyday scenario walk: plays the scenarios of tools/walk/scenarios.toml (listed in
docs/SCENARIOS.md) in the official Telegram Desktop and in Rustgram, on scratch copies of the same
TEST-server account, and saves `official | Rustgram` screenshot pairs plus a
pass / FAIL / blocked / needs-a-look table.

    python -I run.py --config <walk_config.json> [--only ID,ID] [--clients official,rustgram]

The config is a local JSON file (never committed; see walk_config.example.json). It names the
client executables, the test-account working directories to COPY, the peer binary and its state
file, and the output folder. Result tables hold scenario ids, titles and verdicts only.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
import time
import tomllib
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from engine import Runner, make_clients  # noqa: E402,F401  (make_clients: used by probe.py)

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
LABEL = {"pass": "pass", "fail": "FAIL", "blocked": "blocked", "look": "needs a look"}


def write_results(results: list[dict[str, Any]], out: Path, meta: dict[str, Any]) -> Path:
    (out / "results.json").write_text(json.dumps({"meta": meta, "results": results}, indent=1),
                                      encoding="utf-8")
    lines = [f"Run {meta['date']} on {meta['commit']} ({meta['seconds']:.0f} s)", "",
             "| Id | Scenario | Result | Note |", "|---|---|---|---|"]
    for r in results:
        note = r["note"].replace("|", "/").replace("\n", " ")[:180]
        lines.append(f"| {r['id']} | {r['title']} | {LABEL.get(r['status'], r['status'])} | {note} |")
    counts = {k: sum(1 for r in results if r["status"] == k) for k in LABEL}
    lines += ["", ", ".join(f"{LABEL[k]}: {v}" for k, v in counts.items())]
    md = out / "results.md"
    md.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return md


def git_commit() -> str:
    try:
        return subprocess.run(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"],
                              capture_output=True, text=True, timeout=10).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def load_scenarios(path: Path) -> list[dict[str, Any]]:
    return tomllib.loads(path.read_text(encoding="utf-8"))["scenario"]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    ap.add_argument("--scenarios", default=str(HERE / "scenarios.toml"))
    ap.add_argument("--only", default="", help="comma-separated scenario ids")
    ap.add_argument("--clients", default="official,rustgram")
    ap.add_argument("--label", default="", help="suffix of the output folder")
    ap.add_argument("--commit", default="", help="what the Rustgram build was made from")
    a = ap.parse_args()
    cfg = json.loads(Path(a.config).read_text(encoding="utf-8"))
    scenarios = load_scenarios(Path(a.scenarios))
    if a.only:
        want = a.only.split(",")
        scenarios = [s for s in scenarios if s["id"] in want or any(s["id"].startswith(w.rstrip("*")) and w.endswith("*") for w in want)]
    stamp = dt.datetime.now().strftime("%Y-%m-%d_%H%M")
    out = Path(cfg["snap_root"]) / (stamp + (f"_{a.label}" if a.label else ""))
    out.mkdir(parents=True, exist_ok=True)
    runner = Runner(cfg, out, Path(cfg["scratch"]), [c for c in a.clients.split(",") if c])
    commit = a.commit or git_commit()
    results: list[dict[str, Any]] = []
    t0 = time.time()
    try:
        runner.start()
        for sc in scenarios:
            print(f"[walk] {sc['id']} {sc['title']}", flush=True)
            r = runner.play(sc)
            results.append(r)
            print(f"[walk]   -> {LABEL.get(r['status'])} {r['note'][:200]}", flush=True)
            write_results(results, out, {"date": stamp, "commit": commit, "seconds": time.time() - t0})
    finally:
        runner.stop()
    md = write_results(results, out, {"date": stamp, "commit": commit, "seconds": time.time() - t0})
    print(md.read_text(encoding="utf-8"))
    print(f"[walk] pairs and results in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
