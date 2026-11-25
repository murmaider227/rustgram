#!/usr/bin/env python3
"""Build Rustgram release binaries and stage the portable distribution via tg-xtask dist.

Generates an ephemeral Ed25519 keypair in memory via `tg-xtask keygen` (never printed
or committed), passes the public key in `RUSTGRAM_UPDATE_PUBKEY` and the private seed
via a temporary file that is removed immediately after packaging.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--workspace",
        type=Path,
        default=Path.cwd(),
        help="Path to the Rustgram repository root (default: cwd)",
    )
    p.add_argument(
        "--out-dir",
        type=Path,
        required=True,
        help="Output directory for tg-xtask dist (e.g. <CARGO_TARGET_DIR>/dist)",
    )
    p.add_argument(
        "--ffmpeg-bin",
        type=Path,
        default=None,
        help="Optional directory containing FFmpeg runtime DLLs",
    )
    p.add_argument(
        "--cargo",
        default="cargo",
        help="Cargo executable name or path (default: cargo)",
    )
    return p.parse_args()


def extract_keygen_pair(stdout: str) -> tuple[str, str]:
    pub_m = re.search(r"Hex:\s*([0-9a-fA-F]{64})", stdout)
    # Private key seed is the final standalone 64-hex line after section 2.
    hex_lines = re.findall(r"(?m)^\s*([0-9a-fA-F]{64})\s*$", stdout)
    if not pub_m or not hex_lines:
        raise RuntimeError("Could not parse ephemeral keypair from tg-xtask keygen output")
    return pub_m.group(1), hex_lines[-1]


def main() -> int:
    args = parse_args()
    ws = args.workspace.resolve()
    out_dir = args.out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env.setdefault("CARGO_INCREMENTAL", "0")
    env.setdefault("TGFORK_HEADLESS_AUDIO", "1")

    print("[build_dist] Generating ephemeral signing keypair (not printed)...", flush=True)
    kg = subprocess.run(
        [args.cargo, "run", "-q", "-p", "tg-xtask", "--", "keygen"],
        cwd=ws,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if kg.returncode != 0:
        sys.stderr.write(f"[build_dist] tg-xtask keygen failed (exit {kg.returncode})\n")
        if kg.stderr:
            sys.stderr.write(kg.stderr)
        return kg.returncode

    pub_hex, seed_hex = extract_keygen_pair(kg.stdout)
    env["RUSTGRAM_UPDATE_PUBKEY"] = pub_hex

    tmp_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="ascii",
            prefix="rustgram_ephemeral_",
            suffix=".key",
            dir=out_dir,
            delete=False,
        ) as tf:
            tf.write(seed_hex)
            tmp_path = Path(tf.name)

        cmd = [
            args.cargo,
            "run",
            "-p",
            "tg-xtask",
            "--",
            "dist",
            "--out-dir",
            str(out_dir),
            "--signing-key",
            str(tmp_path),
        ]
        if args.ffmpeg_bin is not None:
            cmd.extend(["--ffmpeg-bin", str(args.ffmpeg_bin.resolve())])

        print("[build_dist] Running tg-xtask dist (release build + packaging)...", flush=True)
        res = subprocess.run(cmd, cwd=ws, env=env, check=False)
        return res.returncode
    finally:
        if tmp_path is not None and tmp_path.exists():
            try:
                # Overwrite before unlink.
                tmp_path.write_bytes(b"\x00" * 64)
            except OSError:
                pass
            try:
                tmp_path.unlink()
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
