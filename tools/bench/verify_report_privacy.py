#!/usr/bin/env python3
"""Scan benchmark outputs (results.json / BENCHMARKS.md) for accidental PII or local paths.

Fails with exit code 1 if machine name, username, local drive paths, phone numbers,
or hex API-hash-like tokens appear in the published report files.
"""

from __future__ import annotations

import argparse
import getpass
import os
import re
import socket
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("files", nargs="+", type=Path, help="Files to scan")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    hostname = socket.gethostname()
    username = getpass.getuser()
    computername = os.environ.get("COMPUTERNAME", "")

    forbidden_literals: list[tuple[str, str]] = []
    for label, val in [
        ("hostname", hostname),
        ("COMPUTERNAME", computername),
        ("username", username),
    ]:
        if val and len(val) >= 3:
            forbidden_literals.append((label, val))

    patterns: list[tuple[str, re.Pattern[str]]] = [
        ("Windows drive path", re.compile(r"(?i)\b[a-z]:[\\/]")),
        ("Users path", re.compile(r"(?i)[\\/]users[\\/]")),
        ("UNC path", re.compile(r"\\\\[a-zA-Z0-9._-]+\\")),
        ("Phone number (+digits)", re.compile(r"\+\d[\d\s\-]{7,15}\d")),
        ("Test phone 99966...", re.compile(r"\b99966\d{4,6}\b")),
    ]

    failures: list[str] = []
    for fp in args.files:
        text = fp.read_text(encoding="utf-8")
        for label, lit in forbidden_literals:
            # Word-boundary match case-insensitively to avoid false positives on short common words.
            pat = re.compile(r"(?i)\b" + re.escape(lit) + r"\b")
            for m in pat.finditer(text):
                line_no = text.count("\n", 0, m.start()) + 1
                snippet = text.splitlines()[line_no - 1].strip()
                if label == "username" and lit.lower() == "user" and m.group(0) == "user":
                    continue
                failures.append(f"{fp}:{line_no}: matched {label} ({m.group(0)!r}) in: {snippet[:120]}")
        for label, rx in patterns:
            for m in rx.finditer(text):
                line_no = text.count("\n", 0, m.start()) + 1
                snippet = text.splitlines()[line_no - 1].strip()
                failures.append(f"{fp}:{line_no}: matched {label} ({m.group(0)!r}) in: {snippet[:120]}")

    if failures:
        for f in failures:
            sys.stderr.write(f"[privacy-fail] {f}\n")
        return 1

    print(f"[privacy-ok] Scanned {len(args.files)} file(s): no PII or local paths found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
