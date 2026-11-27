"""Merges walk runs on the same build: a later run's verdict replaces an earlier one for the
scenarios it played (used when a second pass re-plays scenarios the first pass could not judge).

    python -I -X utf8 merge_runs.py --out <merged results.json> <run1/results.json> <run2/results.json> ...
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("runs", nargs="+")
    a = ap.parse_args()
    order = [s["id"] for s in tomllib.loads((HERE / "scenarios.toml").read_text(encoding="utf-8"))["scenario"]]
    by: dict[str, dict] = {}
    meta: dict = {}
    seconds = 0.0
    for p in a.runs:
        d = json.loads(Path(p).read_text(encoding="utf-8"))
        meta = d["meta"]
        seconds += float(d["meta"].get("seconds", 0))
        for r in d["results"]:
            by[r["id"]] = r
    meta = dict(meta)
    meta["seconds"] = seconds
    meta["runs"] = len(a.runs)
    results = [by[i] for i in order if i in by]
    Path(a.out).write_text(json.dumps({"meta": meta, "results": results}, indent=1), encoding="utf-8")
    counts: dict[str, int] = {}
    for r in results:
        counts[r["status"]] = counts.get(r["status"], 0) + 1
    print(len(results), counts)
    return 0


if __name__ == "__main__":
    sys.exit(main())
