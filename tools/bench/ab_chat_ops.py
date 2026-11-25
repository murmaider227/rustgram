#!/usr/bin/env python3
"""Helper to seed short A<->B test messages for scroll benchmarking and clean them up afterward.

All paths come from CLI arguments; target phone/username for deep link comes strictly from
environment variable BENCH_TARGET_LINK (never hardcoded or logged).
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import win_metrics as wm

VK_RETURN = 0x0D


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    sp_seed = sub.add_parser("seed", help="Send N short test messages in the A<->B chat via Rustgram UI")
    sp_seed.add_argument("--rustgram-exe", type=Path, required=True)
    sp_seed.add_argument("--workdir-a", type=Path, required=True)
    sp_seed.add_argument("--scratch-dir", type=Path, required=True)
    sp_seed.add_argument("--api-creds-doc", type=Path, default=None)
    sp_seed.add_argument("--count", type=int, default=200)
    sp_seed.add_argument("--delay-s", type=float, default=0.45)
    sp_seed.add_argument("--diag-png", type=Path, default=None)

    sp_menu = sub.add_parser("inspect-delete-menu", help="Right-click row 0 and save PNG to verify delete-chat UI")
    sp_menu.add_argument("--rustgram-exe", type=Path, required=True)
    sp_menu.add_argument("--workdir-a", type=Path, required=True)
    sp_menu.add_argument("--scratch-dir", type=Path, required=True)
    sp_menu.add_argument("--api-creds-doc", type=Path, default=None)
    sp_menu.add_argument("--diag-dir", type=Path, required=True)

    sp_clean = sub.add_parser("cleanup", help="Delete the A<->B chat history for both sides via Rustgram UI")
    sp_clean.add_argument("--rustgram-exe", type=Path, required=True)
    sp_clean.add_argument("--workdir-a", type=Path, required=True)
    sp_clean.add_argument("--scratch-dir", type=Path, required=True)
    sp_clean.add_argument("--api-creds-doc", type=Path, default=None)
    sp_clean.add_argument("--row-xy", type=int, nargs=2, default=[220, 125])
    sp_clean.add_argument("--menu-delete-xy", type=int, nargs=2, required=True)
    sp_clean.add_argument("--confirm-xy", type=int, nargs=2, required=True)
    sp_clean.add_argument("--revoke-checkbox-xy", type=int, nargs=2, default=None)
    sp_clean.add_argument("--diag-png", type=Path, default=None)

    return p.parse_args()


def build_env(api_creds_doc: Path | None) -> dict[str, str]:
    env = os.environ.copy()
    env["TGFORK_HEADLESS_AUDIO"] = "1"
    env["RUSTGRAM_LIVE"] = "1"
    aid = env.get("RUSTGRAM_API_ID") or env.get("TGFORK_API_ID")
    ah = env.get("RUSTGRAM_API_HASH") or env.get("TGFORK_API_HASH")
    if (not aid or not ah) and api_creds_doc is not None:
        aid, ah = wm.read_test_api_credentials(api_creds_doc)
    if not aid or not ah:
        raise RuntimeError("Missing API credentials")
    env["RUSTGRAM_API_ID"] = aid
    env["RUSTGRAM_API_HASH"] = ah
    return env


def focus_window_messages(hwnd: int) -> None:
    wm.user32.PostMessageW(hwnd, wm.WM_ACTIVATE, 1, 0)
    wm.user32.PostMessageW(hwnd, wm.WM_SETFOCUS, 0, 0)


def cmd_seed(args: argparse.Namespace) -> int:
    link = os.environ.get("BENCH_TARGET_LINK", "").strip()
    wd = args.scratch_dir / "ab_seed_wd"
    wm.copy_workdir_safely(args.workdir_a, wd)
    env = build_env(args.api_creds_doc)

    cmd = [str(args.rustgram_exe), "--live", "-testmode", "-workdir", str(wd)]
    if link:
        cmd.append(link)

    flood_hit = [0]
    p = subprocess.Popen(
        cmd,
        cwd=str(args.rustgram_exe.parent),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    def log_watcher() -> None:
        assert p.stdout is not None
        for raw in p.stdout:
            line = raw.decode("utf-8", errors="ignore")
            m = re.search(r"FLOOD_WAIT[_\s]+(\d+)", line, re.IGNORECASE)
            if m:
                secs = int(m.group(1))
                flood_hit[0] = max(flood_hit[0], secs)
                print(f"[seed] WARNING: saw FLOOD_WAIT_{secs}", flush=True)

    th = threading.Thread(target=log_watcher, daemon=True)
    th.start()

    try:
        hwnd = None
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < 25.0:
            wins = wm.list_visible_windows_for_pid(p.pid, min_w=300, min_h=200)
            if wins:
                hwnd = wins[0].hwnd
                break
            time.sleep(0.05)
        if hwnd is None:
            raise RuntimeError("Rustgram window did not appear")

        wm.move_window(hwnd, 60, 40, 1200, 800)
        time.sleep(4.0)

        # If no deep link was passed, click row 0 (assumed to be A<->B).
        if not link:
            wm.post_click(hwnd, 220, 125)
            time.sleep(1.5)

        focus_window_messages(hwnd)
        time.sleep(0.2)
        wm.post_click(hwnd, 650, 775)
        time.sleep(0.3)

        sent = 0
        for i in range(1, args.count + 1):
            if flood_hit[0] > 60:
                print(f"[seed] Aborting: FLOOD_WAIT {flood_hit[0]}s > 60s", flush=True)
                return 3
            if flood_hit[0] > 0:
                wait_s = flood_hit[0] + 2
                print(f"[seed] Pausing {wait_s}s for FLOOD_WAIT...", flush=True)
                flood_hit[0] = 0
                time.sleep(wait_s)

            focus_window_messages(hwnd)
            wm.post_click(hwnd, 650, 775)
            time.sleep(0.05)
            wm.post_unicode_text(hwnd, f"bench msg {i:03d}")
            time.sleep(0.05)
            wm.post_vk(hwnd, VK_RETURN)
            sent = i
            if i % 25 == 0 or i == args.count:
                print(f"[seed] Sent {i}/{args.count}", flush=True)
            # Gentle pacing + brief breather every 25 messages to stay clear of FLOOD_WAIT.
            time.sleep(args.delay_s)
            if i % 25 == 0 and i < args.count:
                time.sleep(2.0)

        time.sleep(2.5)
        if args.diag_png is not None:
            f = wm.capture_window_printwindow(hwnd)
            if f is not None:
                wm.save_png(f, args.diag_png)
        print(f"[seed] Done sending {sent} messages.", flush=True)
        return 0
    finally:
        wm.stop_process_by_pid(p.pid, graceful_timeout_s=5.0)
        time.sleep(1.5)
        shutil.rmtree(wd, ignore_errors=True)


def cmd_inspect_delete_menu(args: argparse.Namespace) -> int:
    wd = args.scratch_dir / "ab_del_inspect"
    wm.copy_workdir_safely(args.workdir_a, wd)
    env = build_env(args.api_creds_doc)
    p = subprocess.Popen(
        [str(args.rustgram_exe), "--live", "-testmode", "-workdir", str(wd)],
        cwd=str(args.rustgram_exe.parent),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(5.0)
        hwnd = wm.list_visible_windows_for_pid(p.pid)[0].hwnd
        wm.move_window(hwnd, 60, 40, 1200, 800)
        time.sleep(2.0)
        wm.post_right_click(hwnd, 220, 125)
        time.sleep(0.6)
        f = wm.capture_window_printwindow(hwnd)
        if f is not None:
            args.diag_dir.mkdir(parents=True, exist_ok=True)
            wm.save_png(f, args.diag_dir / "ab_ctx_menu.png")
            # Grid overlay
            from bench_part2 import draw_grid_overlay
            wm.save_png(draw_grid_overlay(f, step=50), args.diag_dir / "ab_ctx_menu_grid.png")
        return 0
    finally:
        wm.stop_process_by_pid(p.pid, graceful_timeout_s=5.0)
        time.sleep(1.5)
        shutil.rmtree(wd, ignore_errors=True)


def cmd_cleanup(args: argparse.Namespace) -> int:
    wd = args.scratch_dir / "ab_cleanup_wd"
    wm.copy_workdir_safely(args.workdir_a, wd)
    env = build_env(args.api_creds_doc)
    p = subprocess.Popen(
        [str(args.rustgram_exe), "--live", "-testmode", "-workdir", str(wd)],
        cwd=str(args.rustgram_exe.parent),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(5.0)
        hwnd = wm.list_visible_windows_for_pid(p.pid)[0].hwnd
        wm.move_window(hwnd, 60, 40, 1200, 800)
        time.sleep(2.0)
        wm.post_right_click(hwnd, args.row_xy[0], args.row_xy[1])
        time.sleep(0.6)
        wm.post_click(hwnd, args.menu_delete_xy[0], args.menu_delete_xy[1])
        time.sleep(0.8)
        if args.revoke_checkbox_xy is not None:
            wm.post_click(hwnd, args.revoke_checkbox_xy[0], args.revoke_checkbox_xy[1])
            time.sleep(0.4)
        wm.post_click(hwnd, args.confirm_xy[0], args.confirm_xy[1])
        time.sleep(4.0)
        if args.diag_png is not None:
            f = wm.capture_window_printwindow(hwnd)
            if f is not None:
                wm.save_png(f, args.diag_png)
        print("[cleanup] Delete chat confirmed.", flush=True)
        return 0
    finally:
        wm.stop_process_by_pid(p.pid, graceful_timeout_s=5.0)
        time.sleep(1.5)
        shutil.rmtree(wd, ignore_errors=True)


def main() -> int:
    args = parse_args()
    if args.cmd == "seed":
        return cmd_seed(args)
    if args.cmd == "inspect-delete-menu":
        return cmd_inspect_delete_menu(args)
    if args.cmd == "cleanup":
        return cmd_cleanup(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
