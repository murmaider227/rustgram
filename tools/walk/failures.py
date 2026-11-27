"""Prints one issue draft per failed scenario of a walk run (expected vs seen, tdesktop source).

    python -I failures.py <run folder>/results.json [--status fail,look]

The drafts hold no account data and no local paths; screenshots stay local.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("results")
    ap.add_argument("--status", default="fail")
    a = ap.parse_args()
    want = set(a.status.split(","))
    data = json.loads(Path(a.results).read_text(encoding="utf-8"))
    scen = {s["id"]: s for s in tomllib.loads((HERE / "scenarios.toml").read_text(encoding="utf-8"))["scenario"]}
    for r in data["results"]:
        if r["status"] not in want:
            continue
        sc = scen.get(r["id"], {})
        rg = r["clients"].get("rustgram", {})
        off = r["clients"].get("official", {})
        seen = [f"{c['text']} -> {'ok' if c['ok'] else 'FAILED'}{' (' + c['detail'] + ')' if c['detail'] else ''}"
                for c in rg.get("checks", []) if c["ok"] is not None]
        ref = [f"{c['text']} -> {'ok' if c['ok'] else 'FAILED'}" for c in off.get("checks", []) if c["ok"] is not None]
        text = "\n".join([
            f"=== {r['id']}: {r['title']}  [{r['status']}]",
            f"Reported before: {sc.get('bug', '-')}",
            f"Expected (tdesktop): {sc.get('expect', '')}",
            f"Seen in Rustgram: {r['note'] or rg.get('error', '')}",
            "Rustgram checks: " + "; ".join(seen),
            "Official checks: " + "; ".join(ref),
            f"tdesktop source: {sc.get('source', '')}",
            "",
        ])
        print(re.sub(r"[A-Za-z]:[\\/][^\s;]*", "<path>", text))
    return 0


if __name__ == "__main__":
    sys.exit(main())
