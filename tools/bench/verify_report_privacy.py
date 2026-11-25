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
from typing import Sequence


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("files", nargs="+", type=Path, help="Files to scan")
    return p.parse_args()


def scan_files(files: Sequence[Path]) -> list[str]:
    """Scan `files` for hostnames, usernames, local paths, phone numbers, or env secrets."""
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

    # Also check any runtime secrets present in the environment so they can never leak into reports.
    for env_key in ("RUSTGRAM_API_HASH", "TGFORK_API_HASH", "BENCH_TARGET_LINK"):
        val = os.environ.get(env_key, "").strip()
        if val and len(val) >= 6:
            forbidden_literals.append((env_key, val))

    patterns: list[tuple[str, re.Pattern[str]]] = [
        ("Windows drive path", re.compile(r"(?i)\b[a-z]:[\\/]")),
        ("Users path", re.compile(r"(?i)[\\/]users[\\/]")),
        ("Home path", re.compile(r"(?i)(?:^|[\s\"'(=])/home/[a-zA-Z0-9._-]+/")),
        ("UNC path", re.compile(r"\\\\[a-zA-Z0-9._-]+\\")),
        ("Phone number (+digits)", re.compile(r"\+\d[\d\s\-]{7,15}\d")),
        ("Test phone 99966...", re.compile(r"\b99966\d{4,6}\b")),
    ]

    failures: list[str] = []
    for fp in files:
        text = Path(fp).read_text(encoding="utf-8")
        lines = text.splitlines()
        for label, lit in forbidden_literals:
            if label in ("RUSTGRAM_API_HASH", "TGFORK_API_HASH", "BENCH_TARGET_LINK"):
                pat = re.compile(re.escape(lit), re.IGNORECASE)
            else:
                # Word-boundary match case-insensitively to avoid false positives on short common words.
                pat = re.compile(r"(?i)\b" + re.escape(lit) + r"\b")
            for m in pat.finditer(text):
                line_no = text.count("\n", 0, m.start()) + 1
                snippet = lines[line_no - 1].strip() if line_no <= len(lines) else ""
                if label == "username" and lit.lower() == "user" and m.group(0) == "user":
                    continue
                if label in ("RUSTGRAM_API_HASH", "TGFORK_API_HASH", "BENCH_TARGET_LINK"):
                    failures.append(f"{fp}:{line_no}: matched {label} (value redacted)")
                else:
                    failures.append(f"{fp}:{line_no}: matched {label} ({m.group(0)!r}) in: {snippet[:120]}")
        for label, rx in patterns:
            for m in rx.finditer(text):
                line_no = text.count("\n", 0, m.start()) + 1
                snippet = lines[line_no - 1].strip() if line_no <= len(lines) else ""
                failures.append(f"{fp}:{line_no}: matched {label} ({m.group(0)!r}) in: {snippet[:120]}")

    return failures


def verify_files(files: Sequence[Path]) -> int:
    """Verify `files` contain no PII/local paths; print diagnostics and return 0 or 1."""
    failures = scan_files(files)
    if failures:
        for f in failures:
            sys.stderr.write(f"[privacy-fail] {f}\n")
        return 1

    print(f"[privacy-ok] Scanned {len(files)} file(s): no PII or local paths found.")
    return 0


def main() -> int:
    args = parse_args()
    return verify_files(args.files)


if __name__ == "__main__":
    raise SystemExit(main())
