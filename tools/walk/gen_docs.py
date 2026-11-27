"""Writes docs/SCENARIOS.md from tools/walk/scenarios.toml, plus the result table of a walk run.

    python -I gen_docs.py [--results <run folder>/results.json]

Without --results the existing "Results" section of docs/SCENARIOS.md is kept as it is.
The table holds scenario ids, titles and verdicts only (no account data, no local paths).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
DOC = HERE.parent.parent / "docs" / "SCENARIOS.md"
LABEL = {"pass": "pass", "fail": "FAIL", "blocked": "blocked", "look": "needs a look"}
MARK = "## Results"

INTRO = """# Everyday scenario walk

A repeatable walk through everyday actions, played in Rustgram and in the official Telegram Desktop
side by side on the same Telegram **test-server** account. It measures what docs/PARITY.md only
estimates: every scenario below is played in both clients and judged from the screens.

- **Data**: `tools/walk/peer` fills two test-server accounts (A, B) once with groups, a channel,
  mentions, reactions (also on old messages), a pinned message, a poll, an album, a tall photo, a
  video, a GIF, a voice and a round message, a file, stickers from a user-made and a server set, a
  link preview, every text style with a spoiler, a 320-line private chat, a folder, an archived and
  a muted chat, a draft, a scheduled message, a story and contacts. Running it again adds nothing.
- **Runner**: `tools/walk/run.py` starts both clients on scratch copies of the same account,
  plays one step list per scenario in each (so the two runs cannot drift), drives B through the
  session layer, and saves `official | Rustgram` screenshot pairs plus a verdict per scenario.
- **Verdicts**: *pass* (every automatic check held), *FAIL* (a check failed in Rustgram),
  *blocked* (the runner could not play a step; the reason is given), *needs a look* (only a person
  can judge the pair, or the official client failed the same check, so the check is in doubt).
  Automatic checks read the screen with Windows OCR, colours of the corner counters, window state
  and server state through the peer; nothing is marked *pass* without one.

## How to run

1. Build: `cargo build -p tg-app --bin rustgram -p tg-walk-peer` (any target dir).
2. Write a local config from `tools/walk/walk_config.example.json` (paths to both clients, the
   test-account working dirs to copy, the peer, the output folder). It stays out of the repo.
3. Once: `python -I tools/walk/make_media.py --out <media dir>` and
   `tg-walk-peer fixture --a <copy of A> --b <copy of B> --state <state.json> --media <media dir>`.
4. Every release: `python -I tools/walk/run.py --config <config>` (`--only OB-*` for the owner's bugs).
   Pairs and `results.md` land in the config's `snap_root`; `gen_docs.py --results` updates the
   table below.

Rules the runner keeps: test servers only, scratch copies of working dirs, input posted to the
clients' own windows (the real mouse and keyboard are never used), processes stopped by PID, the
user's own Telegram never touched (so tray clicks run in Rustgram only), and messages the walk sends
are deleted at the end.

## Scenarios

Each scenario names what must be visible afterwards and the tdesktop source it follows.
`OB-` scenarios are bugs the owner reported; `EV-` are everyday flows.
"""


def scenario_md(sc: dict) -> str:
    out = [f"### {sc['id']}: {sc['title']}", ""]
    if sc.get("bug"):
        out.append(f"- Reported: {sc['bug']}")
    if sc.get("only"):
        out.append(f"- Runs in: {', '.join(sc['only'])} only")
    out.append(f"- Must be visible: {sc['expect']}")
    out.append(f"- tdesktop: `{sc['source']}`")
    steps = [ln.strip() for ln in sc["steps"].strip().splitlines() if ln.strip()]
    out.append("- Steps:")
    out.append("")
    out.append("  ```")
    out.extend(f"  {ln}" for ln in steps)
    out.append("  ```")
    out.append("")
    return "\n".join(out)


def results_md(path: Path, scenarios: list[dict]) -> str:
    data = json.loads(path.read_text(encoding="utf-8"))
    meta = data["meta"]
    rows = data["results"]
    counts = {k: sum(1 for r in rows if r["status"] == k) for k in LABEL}
    lines = [MARK, "",
             f"Walk of {meta['date'][:10]} on Rustgram `{meta['commit']}` against Telegram Desktop 7.4.6 "
             f"({len(rows)} scenarios, {meta['seconds'] / 60:.0f} min). "
             + ", ".join(f"{LABEL[k]}: {v}" for k, v in counts.items()) + ".", "",
             "| Id | Scenario | Result | Note |", "|---|---|---|---|"]
    for r in rows:
        note = re.sub(r"[A-Za-z]:[\\/][^\s|]*", "<path>", r.get("note", ""))
        note = note.replace("|", "/").replace("\n", " ")[:200]
        lines.append(f"| {r['id']} | {r['title']} | {LABEL.get(r['status'], r['status'])} | {note} |")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default="")
    ap.add_argument("--extra", default="", help="markdown appended after the table (issue links)")
    a = ap.parse_args()
    scenarios = tomllib.loads((HERE / "scenarios.toml").read_text(encoding="utf-8"))["scenario"]
    body = INTRO + "\n" + "\n".join(scenario_md(s) for s in scenarios)
    if a.results:
        tail = results_md(Path(a.results), scenarios)
        if a.extra:
            tail += "\n" + Path(a.extra).read_text(encoding="utf-8")
    elif DOC.exists() and MARK in DOC.read_text(encoding="utf-8"):
        tail = DOC.read_text(encoding="utf-8").split(MARK, 1)[1]
        tail = MARK + tail
    else:
        tail = MARK + "\n\nNo run recorded yet.\n"
    DOC.write_text(body + "\n" + tail, encoding="utf-8", newline="\n")
    print(f"{DOC.name}: {len(scenarios)} scenarios")
    return 0


if __name__ == "__main__":
    sys.exit(main())
