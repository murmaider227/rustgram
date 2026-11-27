"""Starts both clients on scratch copies and saves screenshots, to calibrate the layout file.

    python -I probe.py --config <walk_config.json> --out <dir> [--keep-open-s N]
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as walk  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--wait-s", type=float, default=20)
    ap.add_argument("--clients", default="official,rustgram")
    a = ap.parse_args()
    cfg = json.loads(Path(a.config).read_text(encoding="utf-8"))
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    clients = walk.make_clients(cfg, out / "scratch", [c for c in a.clients.split(",") if c])
    try:
        for c in clients:
            c.start()
        time.sleep(a.wait_s)
        for c in clients:
            c.place()
            c.shot(out / f"probe_{c.name}.png")
            print(c.name, c.pid, c.client_size())
    finally:
        for c in clients:
            c.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
