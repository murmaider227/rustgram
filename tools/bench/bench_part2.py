#!/usr/bin/env python3
"""Part 2 benchmark: test-account A performance (chat list load, open chat, 10 s scroll,
memory after opening 10 chats, and A->B message delivery latency).

Never touches the original workdirs: always operates on temporary copies in --scratch-dir
and deletes every copy afterward. Never terminates processes it did not launch.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent))
import win_metrics as wm

VK_RETURN = 0x0D
VK_BACK = 0x08
VK_CONTROL = 0x11
VK_A = 0x41


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    def add_common(sp: argparse.ArgumentParser) -> None:
        sp.add_argument("--official-exe", type=Path, required=True)
        sp.add_argument("--official-workdir", type=Path, required=True)
        sp.add_argument("--rustgram-exe", type=Path, required=True)
        sp.add_argument("--rustgram-workdir", type=Path, required=True)
        sp.add_argument("--scratch-dir", type=Path, required=True)
        sp.add_argument("--api-creds-doc", type=Path, default=None)
        sp.add_argument("--diag-dir", type=Path, default=None)

    sp_probe = sub.add_parser("probe", help="Launch copies of both clients and save annotated layout PNGs")
    add_common(sp_probe)
    sp_probe.add_argument("--official-workdir-b", type=Path, default=None, help="Optional B workdir to test Official compatibility")
    sp_probe.add_argument("--rustgram-workdir-b", type=Path, default=None, help="Optional B workdir to test Rustgram B")
    sp_probe.add_argument("--wait-s", type=float, default=12.0)

    sp_cold = sub.add_parser("cold-start", help="Measure start -> chat list loaded (5 runs each)")
    add_common(sp_cold)
    sp_cold.add_argument("--out-json", type=Path, required=True)
    sp_cold.add_argument("--runs", type=int, default=5)
    sp_cold.add_argument("--clients", choices=["both", "official", "rustgram"], default="both")
    sp_cold.add_argument("--idle-check-s", type=float, default=30.0)
    sp_cold.add_argument("--idle-max-cpu", type=float, default=15.0)
    sp_cold.add_argument("--stable-ms", type=float, default=500.0)
    sp_cold.add_argument("--diff-threshold", type=float, default=0.001)
    sp_cold.add_argument("--resume", action="store_true")

    sp_ui = sub.add_parser("interactive", help="Run open-chat, 10s scroll, and 10-chat memory benchmarks")
    add_common(sp_ui)
    sp_ui.add_argument("--layout-json", type=Path, required=True, help="JSON file describing click coordinates & pane rects")
    sp_ui.add_argument("--out-json", type=Path, required=True)
    sp_ui.add_argument("--runs", type=int, default=5)
    sp_ui.add_argument("--clients", choices=["both", "official", "rustgram"], default="both")
    sp_ui.add_argument("--tests", nargs="+", choices=["open_chat", "scroll", "mem_10_chats"], default=["open_chat", "scroll", "mem_10_chats"])
    sp_ui.add_argument("--idle-check-s", type=float, default=30.0)
    sp_ui.add_argument("--idle-max-cpu", type=float, default=15.0)
    sp_ui.add_argument("--resume", action="store_true")

    sp_deliv = sub.add_parser("delivery", help="Measure A->B message delivery latency")
    add_common(sp_deliv)
    sp_deliv.add_argument("--workdir-b", type=Path, required=True, help="Source workdir for account B")
    sp_deliv.add_argument("--layout-json", type=Path, required=True)
    sp_deliv.add_argument("--out-json", type=Path, required=True)
    sp_deliv.add_argument("--client", choices=["rustgram", "official"], required=True)
    sp_deliv.add_argument("--runs", type=int, default=5)
    sp_deliv.add_argument("--idle-check-s", type=float, default=30.0)
    sp_deliv.add_argument("--idle-max-cpu", type=float, default=15.0)

    return p.parse_args()


def build_rustgram_env(api_creds_doc: Path | None) -> dict[str, str]:
    env = os.environ.copy()
    env["TGFORK_HEADLESS_AUDIO"] = "1"
    env["RUSTGRAM_LIVE"] = "1"
    api_id = env.get("RUSTGRAM_API_ID") or env.get("TGFORK_API_ID")
    api_hash = env.get("RUSTGRAM_API_HASH") or env.get("TGFORK_API_HASH")
    if (not api_id or not api_hash) and api_creds_doc is not None:
        api_id, api_hash = wm.read_test_api_credentials(api_creds_doc)
    if not api_id or not api_hash:
        raise RuntimeError("Missing RUSTGRAM_API_ID/RUSTGRAM_API_HASH and --api-creds-doc")
    env["RUSTGRAM_API_ID"] = api_id
    env["RUSTGRAM_API_HASH"] = api_hash
    return env


def launch_client(
    client: str,
    exe_path: Path,
    workdir: Path,
    api_creds_doc: Path | None,
    extra_args: list[str] | None = None,
) -> tuple[subprocess.Popen[bytes], float]:
    env = build_rustgram_env(api_creds_doc) if client == "rustgram" else os.environ.copy()
    if client == "rustgram":
        cmd = [str(exe_path), "--live", "-testmode", "-workdir", str(workdir)]
    else:
        cmd = [str(exe_path), "-workdir", str(workdir)]
    if extra_args:
        cmd.extend(extra_args)
    t_launch = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        cwd=str(exe_path.parent),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return proc, t_launch


def draw_grid_overlay(frame: np.ndarray, step: int = 100) -> np.ndarray:
    img = Image.fromarray(frame, mode="RGB")
    draw = ImageDraw.Draw(img)
    w, h = img.size
    for x in range(0, w, step):
        draw.line([(x, 0), (x, h)], fill=(255, 0, 0), width=1)
        draw.text((x + 2, 2), str(x), fill=(255, 0, 0))
    for y in range(0, h, step):
        draw.line([(0, y), (w, y)], fill=(0, 180, 0), width=1)
        draw.text((2, y + 2), str(y), fill=(0, 180, 0))
    return np.array(img)


def wait_for_chat_list_ready(
    pid: int,
    t_launch: float,
    stable_ms: float = 500.0,
    diff_threshold: float = 0.001,
    min_wait_after_visible_s: float = 0.3,
    timeout_s: float = 60.0,
    diag_png: Path | None = None,
) -> tuple[int | None, dict[str, Any]]:
    """Wait for main window visible, then wait until PrintWindow stops changing for `stable_ms`."""
    deadline = t_launch + timeout_s
    hwnd: int | None = None
    t_vis: float | None = None
    win_size = [0, 0]

    while time.perf_counter() < deadline:
        wins = wm.list_visible_windows_for_pid(pid, min_w=300, min_h=200)
        if wins:
            t_vis = time.perf_counter()
            hwnd = wins[0].hwnd
            win_size = [wins[0].width, wins[0].height]
            break
        time.sleep(0.005)

    if hwnd is None or t_vis is None:
        return None, {
            "window_found": False,
            "first_visible_ms": None,
            "chat_list_ready_ms": None,
            "window_size": [0, 0],
        }

    # Track frame changes; require content to be non-blank and then stable for `stable_ms`.
    stable_s = stable_ms / 1000.0
    blank_cutoff_s = 8.0
    prev_frame = None
    stable_since: float | None = None
    ready_time: float | None = None
    first_non_blank_t: float | None = None
    last_frame = None
    changes_seen = 0
    printwindow_blank = False

    while time.perf_counter() < deadline:
        t_cap = time.perf_counter()
        frame = wm.capture_window_printwindow(hwnd)
        if frame is None:
            wins = wm.list_visible_windows_for_pid(pid, min_w=300, min_h=200)
            if wins:
                hwnd = wins[0].hwnd
                win_size = [wins[0].width, wins[0].height]
            time.sleep(0.02)
            continue

        last_frame = frame
        if not wm.frame_is_non_blank(frame):
            prev_frame = frame
            stable_since = None
            if first_non_blank_t is None and (t_cap - t_vis) >= blank_cutoff_s:
                printwindow_blank = True
                break
            time.sleep(0.03)
            continue

        if first_non_blank_t is None:
            first_non_blank_t = t_cap

        if prev_frame is None:
            prev_frame = frame
            stable_since = t_cap
            time.sleep(0.03)
            continue

        diff = wm.frame_diff_ratio(prev_frame, frame)
        if diff <= diff_threshold:
            if stable_since is None:
                stable_since = t_cap
            elif (t_cap - stable_since) >= stable_s and (t_cap - t_vis) >= min_wait_after_visible_s:
                ready_time = stable_since
                break
        else:
            changes_seen += 1
            prev_frame = frame
            stable_since = t_cap

        time.sleep(0.03)

    if diag_png is not None and last_frame is not None:
        wm.save_png(last_frame, diag_png)

    return hwnd, {
        "window_found": True,
        "first_visible_ms": round((t_vis - t_launch) * 1000.0, 1),
        "first_non_blank_ms": round((first_non_blank_t - t_launch) * 1000.0, 1) if first_non_blank_t is not None else None,
        "chat_list_ready_ms": round((ready_time - t_launch) * 1000.0, 1) if ready_time is not None else None,
        "printwindow_blank": printwindow_blank,
        "frame_transitions": changes_seen,
        "window_size": win_size,
    }


def cmd_probe(args: argparse.Namespace) -> int:
    diag = args.diag_dir or (args.scratch_dir / "probe_diag")
    diag.mkdir(parents=True, exist_ok=True)

    targets: list[tuple[str, Path, Path]] = [
        ("official_a", args.official_exe, args.official_workdir),
        ("rustgram_a", args.rustgram_exe, args.rustgram_workdir),
    ]
    if args.official_workdir_b is not None:
        targets.append(("official_b", args.official_exe, args.official_workdir_b))
    if args.rustgram_workdir_b is not None:
        targets.append(("rustgram_b", args.rustgram_exe, args.rustgram_workdir_b))

    for label, exe, src_wd in targets:
        client = "official" if label.startswith("official") else "rustgram"
        wd = args.scratch_dir / f"probe_{label}"
        print(f"[probe] Copying workdir for {label}...", flush=True)
        wm.copy_workdir_safely(src_wd, wd)
        proc, t_launch = launch_client(client, exe, wd, args.api_creds_doc)
        try:
            hwnd = None
            deadline = t_launch + 30.0
            while time.perf_counter() < deadline:
                wins = wm.list_visible_windows_for_pid(proc.pid, min_w=300, min_h=200)
                if wins:
                    hwnd = wins[0].hwnd
                    break
                time.sleep(0.05)
            if hwnd is None:
                print(f"[probe] {label}: NO visible window found!", flush=True)
                continue
            wm.focus_window(hwnd)
            wm.move_window(hwnd, 60, 40, 1200, 800)
            time.sleep(args.wait_s)
            frame = wm.capture_window_printwindow(hwnd)
            if frame is not None:
                wm.save_png(frame, diag / f"{label}.png")
                wm.save_png(draw_grid_overlay(frame, step=100), diag / f"{label}_grid.png")
                mem = wm.get_process_tree_memory(proc.pid)
                print(
                    f"[probe] {label}: captured {frame.shape[1]}x{frame.shape[0]}, "
                    f"priv={mem['private_bytes']/1048576:.1f} MiB, ws={mem['working_set_bytes']/1048576:.1f} MiB",
                    flush=True,
                )
            else:
                print(f"[probe] {label}: PrintWindow returned None", flush=True)
        finally:
            wm.stop_process_by_pid(proc.pid, graceful_timeout_s=5.0)
            time.sleep(1.5)
            shutil.rmtree(wd, ignore_errors=True)
    return 0


def cmd_cold_start(args: argparse.Namespace) -> int:
    args.scratch_dir.mkdir(parents=True, exist_ok=True)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)

    if args.resume and args.out_json.exists():
        data = json.loads(args.out_json.read_text(encoding="utf-8"))
    else:
        data = {
            "part": "part2_cold_start",
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "machine": wm.get_machine_summary(),
            "idle_checks": [],
            "runs": {"official": [], "rustgram": []},
            "summary": {},
        }

    print(f"[part2:cold] Checking idle CPU < {args.idle_max_cpu}% for {args.idle_check_s} s...", flush=True)
    idle = wm.check_system_idle(duration_s=args.idle_check_s, max_cpu_pct=args.idle_max_cpu)
    idle["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
    data["idle_checks"].append(idle)
    print(f"[part2:cold] Idle check: passed={idle['passed']}, mean={idle['mean_pct']}%, max={idle['max_pct']}%", flush=True)
    if not idle["passed"]:
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")
        return 2

    def save() -> None:
        summary: dict[str, Any] = {}
        for cl in ("official", "rustgram"):
            rs = data["runs"][cl]
            summary[cl] = {
                "first_visible_ms": wm.summarize_values([r["first_visible_ms"] for r in rs if r["first_visible_ms"] is not None]),
                "chat_list_ready_ms": wm.summarize_values([r["chat_list_ready_ms"] for r in rs if r["chat_list_ready_ms"] is not None]),
                "post_load_private_mib": wm.summarize_values([r["memory_post_load"]["private_bytes"] / 1048576.0 for r in rs if r.get("memory_post_load")]),
                "post_load_working_set_mib": wm.summarize_values([r["memory_post_load"]["working_set_bytes"] / 1048576.0 for r in rs if r.get("memory_post_load")]),
            }
        data["summary"] = summary
        data["updated_utc"] = datetime.now(timezone.utc).isoformat()
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")

    clients = ["official", "rustgram"] if args.clients == "both" else [args.clients]
    for run_num in range(1, args.runs + 1):
        for cl in clients:
            if any(r["run"] == run_num for r in data["runs"][cl]):
                continue
            src_wd = args.official_workdir if cl == "official" else args.rustgram_workdir
            exe = args.official_exe if cl == "official" else args.rustgram_exe
            wd = args.scratch_dir / f"p2_cold_{cl}_{run_num}"
            wm.copy_workdir_safely(src_wd, wd)
            diag_png = (args.diag_dir / f"part2_cold_{cl}_run{run_num}.png") if (args.diag_dir and run_num == 1) else None
            proc, t_launch = launch_client(cl, exe, wd, args.api_creds_doc)
            try:
                _hwnd, res = wait_for_chat_list_ready(
                    proc.pid,
                    t_launch,
                    stable_ms=args.stable_ms,
                    diff_threshold=args.diff_threshold,
                    timeout_s=60.0,
                    diag_png=diag_png,
                )
                time.sleep(5.0)
                mem = wm.get_process_tree_memory(proc.pid)
                rec = {
                    "run": run_num,
                    "timestamp_utc": datetime.now(timezone.utc).isoformat(),
                    **res,
                    "memory_post_load": mem,
                }
                print(
                    f"[part2:cold] {cl} run {run_num}: visible={rec['first_visible_ms']} ms, "
                    f"chat_list_ready={rec['chat_list_ready_ms']} ms, priv={mem['private_bytes']/1048576:.1f} MiB",
                    flush=True,
                )
                data["runs"][cl].append(rec)
                data["runs"][cl].sort(key=lambda r: r["run"])
                save()
            finally:
                wm.stop_process_by_pid(proc.pid, graceful_timeout_s=5.0)
                time.sleep(2.0)
                shutil.rmtree(wd, ignore_errors=True)

    return 0


def _capture_region(
    hwnd: int,
    win_rect: tuple[int, int, int, int],
    rel_rect: tuple[int, int, int, int],
    use_printwindow: bool,
) -> np.ndarray | None:
    hx, hy, hw, hh = rel_rect
    if use_printwindow:
        full = wm.capture_window_printwindow(hwnd)
        if full is None:
            return None
        fh, fw, _ = full.shape
        x0 = max(0, min(hx, fw - 1))
        y0 = max(0, min(hy, fh - 1))
        x1 = max(x0 + 1, min(hx + hw, fw))
        y1 = max(y0 + 1, min(hy + hh, fh))
        return full[y0:y1, x0:x1, :]
    wl, wt_y, _, _ = win_rect
    return wm.capture_screen_rect(wl + hx, wt_y + hy, hw, hh)


def _detect_isolated_desktop(win_rect: tuple[int, int, int, int]) -> bool:
    wl, wt_y, _, _ = win_rect
    probe = wm.capture_screen_rect(wl, wt_y, 32, 32)
    return probe is None


def measure_open_chat_once(
    hwnd: int,
    win_rect: tuple[int, int, int, int],
    click_rel: tuple[int, int],
    history_rel_rect: tuple[int, int, int, int],
    stable_ms: float = 500.0,
    diff_threshold: float = 0.001,
    timeout_s: float = 15.0,
    isolated_desktop: bool = False,
) -> dict[str, Any]:
    """Click a chat item and measure time until history pane changes and stabilizes."""
    wl, wt_y, _, _ = win_rect

    baseline = _capture_region(hwnd, win_rect, history_rel_rect, use_printwindow=isolated_desktop)
    if baseline is None:
        return {"ok": False, "first_paint_ms": None, "stable_ms": None}

    if isolated_desktop:
        t_click = wm.post_click(hwnd, click_rel[0], click_rel[1])
    else:
        t_click = wm.send_mouse_click(wl + click_rel[0], wt_y + click_rel[1])
    deadline = t_click + timeout_s
    stable_s = stable_ms / 1000.0

    first_change_t: float | None = None
    prev_frame = baseline
    stable_since: float | None = None
    stable_t: float | None = None

    while time.perf_counter() < deadline:
        t_cap = time.perf_counter()
        cur = _capture_region(hwnd, win_rect, history_rel_rect, use_printwindow=isolated_desktop)
        if cur is None:
            time.sleep(0.005)
            continue

        if first_change_t is None:
            if wm.frame_diff_ratio(baseline, cur) > 0.005:
                first_change_t = t_cap
                prev_frame = cur
                stable_since = t_cap
        else:
            d = wm.frame_diff_ratio(prev_frame, cur)
            if d <= diff_threshold:
                if stable_since is None:
                    stable_since = t_cap
                elif (t_cap - stable_since) >= stable_s:
                    stable_t = stable_since
                    break
            else:
                prev_frame = cur
                stable_since = t_cap
        time.sleep(0.008)

    return {
        "ok": first_change_t is not None and stable_t is not None,
        "first_paint_ms": round((first_change_t - t_click) * 1000.0, 1) if first_change_t is not None else None,
        "stable_ms": round((stable_t - t_click) * 1000.0, 1) if stable_t is not None else None,
        "capture_method": "PrintWindow" if isolated_desktop else "BitBlt",
    }


def measure_scroll_10s(
    pid: int,
    hwnd: int,
    win_rect: tuple[int, int, int, int],
    history_rel_rect: tuple[int, int, int, int],
    duration_s: float = 10.0,
    wheel_hz: float = 40.0,
    wheel_delta: int = 120,
    oscillate_period_s: float = 2.5,
    isolated_desktop: bool = False,
) -> dict[str, Any]:
    """Scroll the history pane with mouse wheel for 10 s while measuring CPU % and frame pacing."""
    wl, wt_y, _, _ = win_rect
    hx, hy, hw, hh = history_rel_rect
    rel_cx, rel_cy = hx + hw // 2, hy + hh // 2
    sx, sy = wl + hx, wt_y + hy
    cx, cy = sx + hw // 2, sy + hh // 2

    if not isolated_desktop:
        wm.send_mouse_move(cx, cy)
    time.sleep(0.15)

    stop_evt = threading.Event()
    wheel_events = [0]

    def wheel_worker() -> None:
        interval = 1.0 / wheel_hz
        t_start = time.perf_counter()
        next_t = t_start
        while not stop_evt.is_set():
            now = time.perf_counter()
            if now < next_t:
                time.sleep(min(0.005, next_t - now))
                continue
            elapsed = now - t_start
            # Alternate scrolling up (older messages) and down (newer) so we stay inside history.
            phase = int(elapsed // oscillate_period_s) % 2
            delta = wheel_delta if phase == 0 else -wheel_delta
            if isolated_desktop:
                wm.post_wheel(hwnd, rel_cx, rel_cy, delta)
            else:
                wm.send_mouse_wheel(delta)
            wheel_events[0] += 1
            next_t += interval

    ncpu = os.cpu_count() or 1
    cap_times: list[float] = []
    unique_times: list[float] = []

    t0 = time.perf_counter()
    ticks0 = wm.get_tree_cpu_ticks(pid)
    th = threading.Thread(target=wheel_worker, daemon=True)
    th.start()

    prev = _capture_region(hwnd, win_rect, history_rel_rect, use_printwindow=isolated_desktop)
    while (time.perf_counter() - t0) < duration_s:
        t_cap = time.perf_counter()
        cur = _capture_region(hwnd, win_rect, history_rel_rect, use_printwindow=isolated_desktop)
        if cur is None:
            continue
        cap_times.append(t_cap)
        if prev is None or wm.frame_diff_ratio(prev, cur, channel_threshold=6) > 0.001:
            unique_times.append(t_cap)
            prev = cur

    stop_evt.set()
    th.join(timeout=2.0)
    t1 = time.perf_counter()
    ticks1 = wm.get_tree_cpu_ticks(pid)

    wall_s = max(1e-6, t1 - t0)
    cpu_s = max(0.0, (ticks1 - ticks0) * 1e-7)
    cpu_pct_total = (cpu_s / (wall_s * ncpu)) * 100.0
    cpu_pct_single = (cpu_s / wall_s) * 100.0

    intervals_ms = [(unique_times[i] - unique_times[i - 1]) * 1000.0 for i in range(1, len(unique_times))]
    if intervals_ms:
        arr = np.array(intervals_ms, dtype=np.float64)
        p50 = float(np.percentile(arr, 50))
        p95 = float(np.percentile(arr, 95))
        p99 = float(np.percentile(arr, 99))
        mx = float(np.max(arr))
        mn = float(np.mean(arr))
    else:
        p50 = p95 = p99 = mx = mn = 0.0

    return {
        "duration_s": round(wall_s, 3),
        "wheel_events": wheel_events[0],
        "captures_total": len(cap_times),
        "capture_hz": round(len(cap_times) / wall_s, 1),
        "unique_frames": len(unique_times),
        "effective_fps": round(len(unique_times) / wall_s, 2),
        "frame_interval_p50_ms": round(p50, 2),
        "frame_interval_p95_ms": round(p95, 2),
        "frame_interval_p99_ms": round(p99, 2),
        "frame_interval_max_ms": round(mx, 2),
        "frame_interval_mean_ms": round(mn, 2),
        "cpu_pct_total": round(cpu_pct_total, 3),
        "cpu_pct_single_core": round(cpu_pct_single, 2),
        "capture_method": "PrintWindow" if isolated_desktop else "BitBlt",
    }


def cmd_interactive(args: argparse.Namespace) -> int:
    layout = json.loads(args.layout_json.read_text(encoding="utf-8"))
    args.scratch_dir.mkdir(parents=True, exist_ok=True)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)

    if args.resume and args.out_json.exists():
        data = json.loads(args.out_json.read_text(encoding="utf-8"))
    else:
        data = {
            "part": "part2_interactive",
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "machine": wm.get_machine_summary(),
            "idle_checks": [],
            "open_chat": {"official": [], "rustgram": []},
            "scroll": {"official": [], "rustgram": []},
            "mem_10_chats": {"official": [], "rustgram": []},
            "summary": {},
        }

    print(f"[part2:ui] Checking idle CPU < {args.idle_max_cpu}% for {args.idle_check_s} s...", flush=True)
    idle = wm.check_system_idle(duration_s=args.idle_check_s, max_cpu_pct=args.idle_max_cpu)
    idle["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
    data["idle_checks"].append(idle)
    print(f"[part2:ui] Idle check: passed={idle['passed']}, mean={idle['mean_pct']}%, max={idle['max_pct']}%", flush=True)
    if not idle["passed"]:
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")
        return 2

    def save() -> None:
        summary: dict[str, Any] = {}
        for cl in ("official", "rustgram"):
            oc = data["open_chat"][cl]
            sc = data["scroll"][cl]
            m10 = data["mem_10_chats"][cl]
            summary[cl] = {
                "open_chat_first_paint_ms": wm.summarize_values([r["first_paint_ms"] for r in oc if r.get("first_paint_ms") is not None]),
                "open_chat_stable_ms": wm.summarize_values([r["stable_ms"] for r in oc if r.get("stable_ms") is not None]),
                "scroll_effective_fps": wm.summarize_values([r["effective_fps"] for r in sc]),
                "scroll_p50_ms": wm.summarize_values([r["frame_interval_p50_ms"] for r in sc]),
                "scroll_p95_ms": wm.summarize_values([r["frame_interval_p95_ms"] for r in sc]),
                "scroll_cpu_pct_total": wm.summarize_values([r["cpu_pct_total"] for r in sc]),
                "scroll_cpu_pct_single_core": wm.summarize_values([r["cpu_pct_single_core"] for r in sc]),
                "mem_10_chats_private_mib": wm.summarize_values([r["memory"]["private_bytes"] / 1048576.0 for r in m10]),
                "mem_10_chats_working_set_mib": wm.summarize_values([r["memory"]["working_set_bytes"] / 1048576.0 for r in m10]),
            }
        data["summary"] = summary
        data["updated_utc"] = datetime.now(timezone.utc).isoformat()
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")

    clients = ["official", "rustgram"] if args.clients == "both" else [args.clients]
    for cl in clients:
        cfg = layout[cl]
        win_x, win_y, win_w, win_h = cfg["window_rect"]
        hist_rect = tuple(cfg["history_rel_rect"])
        chat_rows: list[list[int]] = cfg["chat_rows"]
        long_chat_idx: int = cfg.get("long_chat_row_index", 0)
        alt_chat_idx: int = cfg.get("alt_chat_row_index", 1 if len(chat_rows) > 1 else 0)

        src_wd = args.official_workdir if cl == "official" else args.rustgram_workdir
        exe = args.official_exe if cl == "official" else args.rustgram_exe

        # 1. open_chat + scroll (can run inside a clean fresh-copy session per run or one session for open/scroll)
        if "open_chat" in args.tests or "scroll" in args.tests:
            wd = args.scratch_dir / f"p2_ui_{cl}"
            wm.copy_workdir_safely(src_wd, wd)
            proc, t_launch = launch_client(cl, exe, wd, args.api_creds_doc)
            try:
                hwnd, _ = wait_for_chat_list_ready(proc.pid, t_launch, timeout_s=60.0)
                if hwnd is None:
                    raise RuntimeError(f"Could not find window for {cl}")
                wm.focus_window(hwnd)
                wm.move_window(hwnd, win_x, win_y, win_w, win_h)
                time.sleep(3.0)
                wrect = (win_x, win_y, win_x + win_w, win_y + win_h)
                isolated = _detect_isolated_desktop(wrect)
                print(f"[part2:ui] {cl} isolated_desktop={isolated}", flush=True)

                if "open_chat" in args.tests and len(data["open_chat"][cl]) < args.runs:
                    for run_num in range(len(data["open_chat"][cl]) + 1, args.runs + 1):
                        # First switch to alternate chat so clicking the target chat always loads a different history.
                        if alt_chat_idx != long_chat_idx:
                            if isolated:
                                wm.post_click(hwnd, chat_rows[alt_chat_idx][0], chat_rows[alt_chat_idx][1])
                            else:
                                wm.send_mouse_click(win_x + chat_rows[alt_chat_idx][0], win_y + chat_rows[alt_chat_idx][1])
                            time.sleep(1.8)
                        rec = measure_open_chat_once(
                            hwnd,
                            wrect,
                            tuple(chat_rows[long_chat_idx]),
                            hist_rect,
                            stable_ms=500.0,
                            isolated_desktop=isolated,
                        )
                        rec["run"] = run_num
                        rec["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
                        print(
                            f"[part2:open] {cl} run {run_num}: first_paint={rec['first_paint_ms']} ms, "
                            f"stable={rec['stable_ms']} ms",
                            flush=True,
                        )
                        data["open_chat"][cl].append(rec)
                        save()
                        time.sleep(1.0)

                if "scroll" in args.tests and len(data["scroll"][cl]) < args.runs:
                    # Ensure long chat is open and scrolled to bottom first.
                    if isolated:
                        wm.post_click(hwnd, chat_rows[long_chat_idx][0], chat_rows[long_chat_idx][1])
                    else:
                        wm.send_mouse_click(win_x + chat_rows[long_chat_idx][0], win_y + chat_rows[long_chat_idx][1])
                    time.sleep(2.5)
                    for run_num in range(len(data["scroll"][cl]) + 1, args.runs + 1):
                        rec = measure_scroll_10s(
                            proc.pid,
                            hwnd,
                            wrect,
                            hist_rect,
                            duration_s=10.0,
                            wheel_hz=float(cfg.get("scroll_wheel_hz", 40.0)),
                            wheel_delta=int(cfg.get("scroll_wheel_delta", 120)),
                            oscillate_period_s=float(cfg.get("scroll_oscillate_s", 2.5)),
                            isolated_desktop=isolated,
                        )
                        rec["run"] = run_num
                        rec["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
                        print(
                            f"[part2:scroll] {cl} run {run_num}: fps={rec['effective_fps']}, "
                            f"p50={rec['frame_interval_p50_ms']} ms, p95={rec['frame_interval_p95_ms']} ms, "
                            f"cpu={rec['cpu_pct_total']}% (1-core {rec['cpu_pct_single_core']}%)",
                            flush=True,
                        )
                        data["scroll"][cl].append(rec)
                        save()
                        time.sleep(2.0)
            finally:
                wm.stop_process_by_pid(proc.pid, graceful_timeout_s=5.0)
                time.sleep(2.0)
                shutil.rmtree(wd, ignore_errors=True)

        # 2. memory after opening up to 10 chats (fresh process per run!)
        if "mem_10_chats" in args.tests and len(data["mem_10_chats"][cl]) < args.runs:
            ten_rows: list[list[int]] = cfg.get("ten_chat_rows", chat_rows[:10])
            for run_num in range(len(data["mem_10_chats"][cl]) + 1, args.runs + 1):
                wd = args.scratch_dir / f"p2_m10_{cl}_{run_num}"
                wm.copy_workdir_safely(src_wd, wd)
                proc, t_launch = launch_client(cl, exe, wd, args.api_creds_doc)
                try:
                    hwnd, _ = wait_for_chat_list_ready(proc.pid, t_launch, timeout_s=60.0)
                    if hwnd is None:
                        raise RuntimeError(f"Could not find window for {cl}")
                    wm.focus_window(hwnd)
                    wm.move_window(hwnd, win_x, win_y, win_w, win_h)
                    time.sleep(3.0)
                    wrect = (win_x, win_y, win_x + win_w, win_y + win_h)
                    isolated = _detect_isolated_desktop(wrect)
                    mem_before = wm.get_process_tree_memory(proc.pid)

                    # Scroll chat list down if more rows than visible are specified as {x,y,scroll_before}
                    for item in ten_rows:
                        if isinstance(item, dict):
                            if item.get("scroll_list_delta"):
                                lx, ly = cfg["chat_list_center"]
                                for _ in range(int(item.get("scroll_list_steps", 1))):
                                    if isolated:
                                        wm.post_wheel(hwnd, lx, ly, int(item["scroll_list_delta"]))
                                    else:
                                        wm.send_mouse_move(win_x + lx, win_y + ly)
                                        wm.send_mouse_wheel(int(item["scroll_list_delta"]))
                                    time.sleep(0.08)
                            rx, ry = item["x"], item["y"]
                        else:
                            rx, ry = item[0], item[1]
                        if isolated:
                            wm.post_click(hwnd, rx, ry)
                        else:
                            wm.send_mouse_click(win_x + rx, win_y + ry)
                        time.sleep(2.0)

                    time.sleep(5.0)
                    mem_after = wm.get_process_tree_memory(proc.pid)
                    rec = {
                        "run": run_num,
                        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
                        "chats_opened": len(ten_rows),
                        "memory_before": mem_before,
                        "memory": mem_after,
                    }
                    print(
                        f"[part2:mem10] {cl} run {run_num} ({len(ten_rows)} chats): "
                        f"priv={mem_after['private_bytes']/1048576:.1f} MiB, "
                        f"ws={mem_after['working_set_bytes']/1048576:.1f} MiB",
                        flush=True,
                    )
                    data["mem_10_chats"][cl].append(rec)
                    save()
                finally:
                    wm.stop_process_by_pid(proc.pid, graceful_timeout_s=5.0)
                    time.sleep(2.0)
                    shutil.rmtree(wd, ignore_errors=True)

    return 0


def cmd_delivery(args: argparse.Namespace) -> int:
    layout = json.loads(args.layout_json.read_text(encoding="utf-8"))
    cl = args.client
    dcfg = layout[f"delivery_{cl}"]
    args.scratch_dir.mkdir(parents=True, exist_ok=True)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)

    print(f"[part2:delivery:{cl}] Checking idle CPU < {args.idle_max_cpu}% for {args.idle_check_s} s...", flush=True)
    idle = wm.check_system_idle(duration_s=args.idle_check_s, max_cpu_pct=args.idle_max_cpu)
    print(f"[part2:delivery:{cl}] Idle check: passed={idle['passed']}, mean={idle['mean_pct']}%, max={idle['max_pct']}%", flush=True)
    if not idle["passed"]:
        return 2

    exe = args.official_exe if cl == "official" else args.rustgram_exe
    src_a = args.official_workdir if cl == "official" else args.rustgram_workdir
    src_b = args.workdir_b
    wd_a = args.scratch_dir / f"p2_deliv_{cl}_a"
    wd_b = args.scratch_dir / f"p2_deliv_{cl}_b"
    wm.copy_workdir_safely(src_a, wd_a)
    wm.copy_workdir_safely(src_b, wd_b)

    proc_a, ta = launch_client(cl, exe, wd_a, args.api_creds_doc)
    time.sleep(2.0)
    proc_b, tb = launch_client(cl, exe, wd_b, args.api_creds_doc)
    runs_out: list[dict[str, Any]] = []
    try:
        hwnd_a, _ = wait_for_chat_list_ready(proc_a.pid, ta, timeout_s=60.0)
        hwnd_b, _ = wait_for_chat_list_ready(proc_b.pid, tb, timeout_s=60.0)
        if hwnd_a is None or hwnd_b is None:
            raise RuntimeError(f"Could not find both windows for delivery ({hwnd_a}, {hwnd_b})")

        ax, ay, aw, ah = dcfg["win_a_rect"]
        bx, by, bw, bh = dcfg["win_b_rect"]
        wm.focus_window(hwnd_b)
        wm.move_window(hwnd_b, bx, by, bw, bh)
        wm.focus_window(hwnd_a)
        wm.move_window(hwnd_a, ax, ay, aw, ah)
        time.sleep(2.0)
        wrect_b = (bx, by, bx + bw, by + bh)
        isolated = _detect_isolated_desktop(wrect_b)

        # Open A<->B chat in both windows.
        if isolated:
            wm.post_click(hwnd_b, dcfg["b_chat_click"][0], dcfg["b_chat_click"][1])
            time.sleep(2.0)
            wm.post_click(hwnd_a, dcfg["a_chat_click"][0], dcfg["a_chat_click"][1])
        else:
            wm.focus_window(hwnd_b)
            wm.send_mouse_click(bx + dcfg["b_chat_click"][0], by + dcfg["b_chat_click"][1])
            time.sleep(2.0)
            wm.focus_window(hwnd_a)
            wm.send_mouse_click(ax + dcfg["a_chat_click"][0], ay + dcfg["a_chat_click"][1])
        time.sleep(2.0)

        bh_rel = tuple(dcfg["b_history_rel_rect"])
        comp_rel_x, comp_rel_y = dcfg["a_composer_click"][0], dcfg["a_composer_click"][1]

        for run_num in range(1, args.runs + 1):
            token = f"bench-deliv-{cl}-{run_num}-{int(time.time()) % 10000}"
            if isolated:
                wm.user32.PostMessageW(hwnd_a, wm.WM_ACTIVATE, 1, 0)
                wm.user32.PostMessageW(hwnd_a, wm.WM_SETFOCUS, 0, 0)
                time.sleep(0.1)
                wm.post_click(hwnd_a, comp_rel_x, comp_rel_y)
                time.sleep(0.2)
                wm.post_unicode_text(hwnd_a, token)
            else:
                wm.focus_window(hwnd_a)
                wm.send_mouse_click(ax + comp_rel_x, ay + comp_rel_y)
                time.sleep(0.25)
                wm.send_unicode_text(token)
            time.sleep(0.3)

            baseline = _capture_region(hwnd_b, wrect_b, bh_rel, use_printwindow=isolated)
            if baseline is None:
                raise RuntimeError("Could not capture B history baseline")

            t_send = wm.post_vk(hwnd_a, VK_RETURN) if isolated else wm.send_vk(VK_RETURN)
            deadline = t_send + 15.0
            t_recv: float | None = None
            while time.perf_counter() < deadline:
                t_cap = time.perf_counter()
                cur = _capture_region(hwnd_b, wrect_b, bh_rel, use_printwindow=isolated)
                if cur is not None and wm.frame_diff_ratio(baseline, cur, channel_threshold=10) > 0.0008:
                    t_recv = t_cap
                    break
                time.sleep(0.004)

            latency_ms = round((t_recv - t_send) * 1000.0, 1) if t_recv is not None else None
            print(f"[part2:delivery:{cl}] run {run_num}: latency={latency_ms} ms (token={token})", flush=True)
            runs_out.append(
                {
                    "run": run_num,
                    "timestamp_utc": datetime.now(timezone.utc).isoformat(),
                    "delivery_ms": latency_ms,
                    "capture_method": "PrintWindow" if isolated else "BitBlt",
                }
            )
            time.sleep(2.5)

        if args.diag_dir:
            fb = _capture_region(hwnd_b, wrect_b, bh_rel, use_printwindow=isolated)
            if fb is not None:
                wm.save_png(fb, args.diag_dir / f"part2_delivery_{cl}_b_after.png")

        result = {
            "part": f"part2_delivery_{cl}",
            "client": cl,
            "timestamp_utc": datetime.now(timezone.utc).isoformat(),
            "idle_check": idle,
            "runs": runs_out,
            "summary": wm.summarize_values([r["delivery_ms"] for r in runs_out if r["delivery_ms"] is not None]),
        }
        args.out_json.write_text(json.dumps(result, indent=2), encoding="utf-8")
        return 0
    finally:
        wm.stop_process_by_pid(proc_a.pid, graceful_timeout_s=5.0)
        wm.stop_process_by_pid(proc_b.pid, graceful_timeout_s=5.0)
        time.sleep(2.0)
        shutil.rmtree(wd_a, ignore_errors=True)
        shutil.rmtree(wd_b, ignore_errors=True)


def main() -> int:
    args = parse_args()
    if args.cmd == "probe":
        return cmd_probe(args)
    if args.cmd == "cold-start":
        return cmd_cold_start(args)
    if args.cmd == "interactive":
        return cmd_interactive(args)
    if args.cmd == "delivery":
        return cmd_delivery(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
