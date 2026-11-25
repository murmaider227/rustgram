#!/usr/bin/env python3
"""Startup and open-chat benchmark that treats both clients the same way (test account).

Each run starts one client on a fresh copy of its workdir and records, from the same capture
loop for both clients:

- `first_visible_ms`: launch until a visible top-level window of the process exists.
- `first_frame_ms`: launch until the first capture of that window that is not blank.
- `chat_list_ready_ms`: launch until the window shows its final, loaded chat list. All window
  captures are kept (only when they change), the loop runs until nothing changed for
  `--settle-s` seconds, and the ready time is the first capture from which every later capture
  matches the final one. A loading state that stays still for a while ("Loading..." in the chat
  list) therefore does not count as ready, for either client.
- `open_chat_first_paint_ms` / `open_chat_stable_ms`: after the window is moved to the same
  geometry for both clients, a click on the first chat row until the window first changes, and
  until it then stays unchanged for `--stable-ms`.

Window captures use PrintWindow(PW_RENDERFULLCONTENT), which needs the clients on the logged-on
user's interactive desktop: on a separate (non-input) desktop, Telegram Desktop's Direct3D 11
surface is captured as a white rectangle. The script refuses to run elsewhere. From a shell
that runs on another desktop, start it with a Task Scheduler task that has the "run only when
the user is logged on" (/IT) option; see README.md.

Never touches the original workdirs (copies go to --scratch-dir and are deleted after each run)
and only ever stops processes it started, by PID.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import shutil
import sys
import time
from ctypes import wintypes as wt
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench_part2 as b2  # noqa: E402
import win_metrics as wm  # noqa: E402

WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
UOI_NAME = 2
DESKTOP_READOBJECTS = 0x0001


def _desktop_name(handle: int) -> str | None:
    if not handle:
        return None
    user32 = ctypes.windll.user32
    buf = ctypes.create_unicode_buffer(256)
    needed = wt.DWORD()
    if not user32.GetUserObjectInformationW(handle, UOI_NAME, buf, ctypes.sizeof(buf), ctypes.byref(needed)):
        return None
    return buf.value


def interactive_desktop_check() -> dict[str, Any]:
    """Whether this process runs on the desktop that receives user input (WinSta0\\Default)."""
    user32 = ctypes.windll.user32
    kernel32 = ctypes.windll.kernel32
    own = _desktop_name(user32.GetThreadDesktop(kernel32.GetCurrentThreadId()))
    station = _desktop_name(user32.GetProcessWindowStation())
    input_handle = user32.OpenInputDesktop(0, False, DESKTOP_READOBJECTS)
    input_name = _desktop_name(input_handle)
    if input_handle:
        user32.CloseDesktop(input_handle)
    session = wt.DWORD()
    kernel32.ProcessIdToSessionId(kernel32.GetCurrentProcessId(), ctypes.byref(session))
    return {
        "window_station": station,
        "thread_desktop": own,
        "input_desktop": input_name,
        "session_id": int(session.value),
        "console_session_id": int(kernel32.WTSGetActiveConsoleSessionId()),
        "interactive": station == "WinSta0" and own is not None and own == input_name,
    }


def disk_idle_check(duration_s: float, min_idle_pct: float) -> dict[str, Any]:
    """Mean `% Idle Time` of every physical disk over `duration_s` (typeperf, 1 Hz).

    A CPU-only idle gate misses disk-bound background work (e.g. Windows servicing), which
    slows process start-up and window creation of both clients by seconds.
    """
    import csv
    import io
    import subprocess

    samples = max(2, int(round(duration_s)))
    out = subprocess.run(
        ["typeperf", r"\PhysicalDisk(*)\% Idle Time", "-sc", str(samples)],
        capture_output=True,
        text=True,
        timeout=duration_s + 30,
        check=False,
    ).stdout
    rows = [r for r in csv.reader(io.StringIO(out)) if len(r) > 1]
    if len(rows) < 2:
        return {"passed": True, "note": "typeperf returned no samples"}
    header = rows[0]
    per_disk: dict[str, list[float]] = {}
    for row in rows[1:]:
        for name, val in zip(header[1:], row[1:]):
            if "_Total" in name:
                continue
            try:
                per_disk.setdefault(name.split("(")[-1].split(")")[0], []).append(float(val))
            except ValueError:
                pass
    means = {k: round(sum(v) / len(v), 1) for k, v in per_disk.items() if v}
    return {"passed": all(m >= min_idle_pct for m in means.values()), "mean_idle_pct": means, "min_idle_pct": min_idle_pct}


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--official-exe", type=Path, required=True)
    p.add_argument("--official-workdir", type=Path, required=True)
    p.add_argument("--rustgram-exe", type=Path, required=True)
    p.add_argument("--rustgram-workdir", type=Path, required=True)
    p.add_argument("--scratch-dir", type=Path, required=True)
    p.add_argument("--out-json", type=Path, required=True)
    p.add_argument("--api-creds-doc", type=Path, default=None)
    p.add_argument("--diag-dir", type=Path, default=None, help="Saves first/ready/opened captures of run 1 (may show account data; keep private)")
    p.add_argument("--layout-json", type=Path, default=None, help='{"official": {"chat_row": [x, y]}, "rustgram": {...}} in client pixels at --window size')
    p.add_argument("--window", type=int, nargs=4, default=[80, 60, 1200, 800], metavar=("X", "Y", "W", "H"))
    p.add_argument("--runs", type=int, default=5)
    p.add_argument("--clients", choices=["both", "official", "rustgram"], default="both")
    p.add_argument("--settle-s", type=float, default=3.0)
    p.add_argument("--timeout-s", type=float, default=30.0)
    p.add_argument("--stable-ms", type=float, default=500.0)
    p.add_argument("--diff-threshold", type=float, default=0.001)
    p.add_argument("--idle-check-s", type=float, default=30.0)
    p.add_argument("--idle-max-cpu", type=float, default=15.0)
    p.add_argument("--disk-idle-min", type=float, default=80.0, help="Minimum mean %% idle time of every disk during the idle check")
    p.add_argument("--rustgram-env", nargs="*", default=[], metavar="KEY=VALUE", help="Extra environment for Rustgram (e.g. RUSTGRAM_PROFILE_STARTUP=1)")
    p.add_argument("--rustgram-stderr-dir", type=Path, default=None, help="Keep Rustgram's stderr per run (profiling builds)")
    p.add_argument("--resume", action="store_true")
    return p.parse_args()


def launch(
    client: str,
    exe: Path,
    workdir: Path,
    args: argparse.Namespace,
    run: int,
    sess: wm.ScratchSession | None = None,
) -> tuple[Any, float]:
    import subprocess

    if client == "official":
        return b2.launch_client(client, exe, workdir, None)
    env = b2.build_rustgram_env(args.api_creds_doc)
    for kv in args.rustgram_env:
        k, _, v = kv.partition("=")
        env[k] = v
    stderr: Any = subprocess.DEVNULL
    if args.rustgram_stderr_dir is not None:
        args.rustgram_stderr_dir.mkdir(parents=True, exist_ok=True)
        stderr = open(args.rustgram_stderr_dir / f"rustgram_run{run}.txt", "wb")  # noqa: SIM115
        if sess is not None:
            sess.track_file(stderr)
    cmd = [str(exe), "--live", "-testmode", "-workdir", str(workdir)]
    t_launch = time.perf_counter()
    proc = subprocess.Popen(cmd, cwd=str(exe.parent), env=env, stdout=subprocess.DEVNULL, stderr=stderr)
    return proc, t_launch


def wait_visible(pid: int, t_launch: float, timeout_s: float) -> tuple[int | None, float | None]:
    deadline = t_launch + timeout_s
    while time.perf_counter() < deadline:
        wins = wm.list_visible_windows_for_pid(pid, min_w=300, min_h=200)
        if wins:
            return wins[0].hwnd, time.perf_counter()
        time.sleep(0.002)
    return None, None


def record_until_settled(
    pid: int, hwnd: int, t_start: float, settle_s: float, timeout_s: float, threshold: float
) -> tuple[int, list[tuple[float, np.ndarray]]]:
    """Captures the window until nothing changed for `settle_s`; keeps each changed capture."""
    kept: list[tuple[float, np.ndarray]] = []
    deadline = t_start + timeout_s
    last_change = time.perf_counter()
    while time.perf_counter() < deadline:
        t_cap = time.perf_counter()
        frame = wm.capture_window_printwindow(hwnd)
        if frame is None:
            wins = wm.list_visible_windows_for_pid(pid, min_w=300, min_h=200)
            if wins:
                hwnd = wins[0].hwnd
            time.sleep(0.01)
            continue
        if not kept or wm.frame_diff_ratio(kept[-1][1], frame) > threshold:
            kept.append((t_cap, frame))
            last_change = t_cap
        elif t_cap - last_change >= settle_s:
            break
        time.sleep(0.005)
    return hwnd, kept


def ready_time(kept: list[tuple[float, np.ndarray]], threshold: float) -> float | None:
    """First capture time from which every later kept capture matches the final one."""
    if not kept:
        return None
    final = kept[-1][1]
    ready = kept[-1][0]
    for t_cap, frame in reversed(kept):
        if wm.frame_diff_ratio(frame, final) > threshold:
            break
        ready = t_cap
    return ready


def measure_open_chat(
    hwnd: int, click: tuple[int, int], stable_ms: float, threshold: float, timeout_s: float = 15.0
) -> dict[str, Any]:
    # Hover first, so the row's hover highlight is part of the baseline and the first change
    # after the click is the click's own effect.
    lp = ((click[1] & 0xFFFF) << 16) | (click[0] & 0xFFFF)
    ctypes.windll.user32.PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp)
    time.sleep(0.6)
    baseline = wm.capture_window_printwindow(hwnd)
    if baseline is None:
        return {"ok": False}
    # winit ignores a move to the position it saw last, and Windows reports the pointer as gone
    # (the real cursor is elsewhere), so the click goes 1 px beside the hover position.
    lp = ((click[1] & 0xFFFF) << 16) | ((click[0] + 1) & 0xFFFF)
    t_click = time.perf_counter()
    ctypes.windll.user32.PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp)
    ctypes.windll.user32.PostMessageW(hwnd, WM_LBUTTONDOWN, 1, lp)
    ctypes.windll.user32.PostMessageW(hwnd, WM_LBUTTONUP, 0, lp)
    first_change: float | None = None
    stable_since: float | None = None
    stable_at: float | None = None
    prev = baseline
    last_frame = baseline
    while time.perf_counter() - t_click < timeout_s:
        t_cap = time.perf_counter()
        cur = wm.capture_window_printwindow(hwnd)
        if cur is None:
            time.sleep(0.005)
            continue
        last_frame = cur
        if first_change is None:
            if wm.frame_diff_ratio(baseline, cur) > threshold:
                first_change = t_cap
                prev = cur
                stable_since = t_cap
        elif wm.frame_diff_ratio(prev, cur) > threshold:
            prev = cur
            stable_since = t_cap
        elif stable_since is not None and (t_cap - stable_since) * 1000.0 >= stable_ms:
            stable_at = stable_since
            break
        time.sleep(0.005)
    return {
        "ok": first_change is not None and stable_at is not None,
        "first_paint_ms": round((first_change - t_click) * 1000.0, 1) if first_change else None,
        "stable_ms": round((stable_at - t_click) * 1000.0, 1) if stable_at else None,
        "last_frame": last_frame,
    }


def one_run(client: str, run: int, args: argparse.Namespace, layout: dict[str, Any]) -> dict[str, Any]:
    exe = args.official_exe if client == "official" else args.rustgram_exe
    src = args.official_workdir if client == "official" else args.rustgram_workdir
    rec: dict[str, Any] = {"run": run, "timestamp_utc": datetime.now(timezone.utc).isoformat()}
    diag = args.diag_dir if (args.diag_dir is not None and run == 1) else None
    with wm.ScratchSession(args.scratch_dir, graceful_timeout_s=5.0, post_stop_delay_s=2.0) as sess:
        wd = sess.copy_workdir(src, f"fair_{client}_{run}")
        proc, t_launch = launch(client, exe, wd, args, run, sess=sess)
        sess.track_process(proc)
        hwnd, t_vis = wait_visible(proc.pid, t_launch, args.timeout_s)
        if hwnd is None or t_vis is None:
            rec["error"] = "no visible window"
            return rec
        rec["first_visible_ms"] = round((t_vis - t_launch) * 1000.0, 1)
        hwnd, kept = record_until_settled(proc.pid, hwnd, t_vis, args.settle_s, args.timeout_s, args.diff_threshold)
        non_blank = [t for t, f in kept if wm.frame_is_non_blank(f)]
        rec["first_frame_ms"] = round((non_blank[0] - t_launch) * 1000.0, 1) if non_blank else None
        t_ready = ready_time(kept, args.diff_threshold)
        rec["chat_list_ready_ms"] = round((t_ready - t_launch) * 1000.0, 1) if t_ready else None
        rec["distinct_frames"] = [round((t - t_launch) * 1000.0, 1) for t, _ in kept]
        rec["window_size"] = list(kept[-1][1].shape[1::-1]) if kept else None
        mem = wm.get_process_tree_memory(proc.pid)
        rec["memory_post_load"] = mem
        if diag is not None and kept:
            wm.save_png(kept[0][1], diag / f"{client}_first_frame.png")
            wm.save_png(kept[-1][1], diag / f"{client}_ready.png")
        click = (layout.get(client) or {}).get("chat_row")
        if click:
            x, y, w, h = args.window
            wm.move_window(hwnd, x, y, w, h)
            _, settled = record_until_settled(proc.pid, hwnd, time.perf_counter(), 1.5, 10.0, args.diff_threshold)
            if diag is not None and settled:
                wm.save_png(settled[-1][1], diag / f"{client}_before_click.png")
            res = measure_open_chat(hwnd, (int(click[0]), int(click[1])), args.stable_ms, args.diff_threshold)
            last = res.pop("last_frame", None)
            if diag is not None and last is not None:
                wm.save_png(last, diag / f"{client}_opened.png")
            rec["open_chat"] = res
        print(
            f"[fair] {client} run {run}: visible={rec.get('first_visible_ms')} first_frame={rec.get('first_frame_ms')} "
            f"chat_list_ready={rec.get('chat_list_ready_ms')} open_chat={rec.get('open_chat')} "
            f"priv={mem['private_bytes'] / 1048576:.1f} MiB",
            flush=True,
        )
        return rec


def summarize(data: dict[str, Any]) -> None:
    summary: dict[str, Any] = {}
    for cl, runs in data["runs"].items():
        def vals(key: str, sub: str | None = None) -> list[float]:
            out = []
            for r in runs:
                v = r.get(key)
                if sub is not None:
                    v = v.get(sub) if isinstance(v, dict) else None
                if v is not None:
                    out.append(v)
            return out

        summary[cl] = {
            "first_visible_ms": wm.summarize_values(vals("first_visible_ms")),
            "first_frame_ms": wm.summarize_values(vals("first_frame_ms")),
            "chat_list_ready_ms": wm.summarize_values(vals("chat_list_ready_ms")),
            "open_chat_first_paint_ms": wm.summarize_values(vals("open_chat", "first_paint_ms")),
            "open_chat_stable_ms": wm.summarize_values(vals("open_chat", "stable_ms")),
            "post_load_private_mib": wm.summarize_values(
                [r["memory_post_load"]["private_bytes"] / 1048576.0 for r in runs if r.get("memory_post_load")]
            ),
            "post_load_working_set_mib": wm.summarize_values(
                [r["memory_post_load"]["working_set_bytes"] / 1048576.0 for r in runs if r.get("memory_post_load")]
            ),
        }
    data["summary"] = summary


def main() -> int:
    args = parse_args()
    try:
        wm.ensure_outside_git_repo(args.scratch_dir, label="--scratch-dir")
        if args.diag_dir is not None:
            wm.ensure_outside_git_repo(args.diag_dir, label="--diag-dir")
        if args.rustgram_stderr_dir is not None:
            wm.ensure_outside_git_repo(args.rustgram_stderr_dir, label="--rustgram-stderr-dir")
    except ValueError as exc:
        sys.stderr.write(f"[fair] {exc}\n")
        return 2
    desk = interactive_desktop_check()
    print(f"[fair] desktop: {desk}", flush=True)
    if not desk["interactive"]:
        print("[fair] not on the interactive desktop: window captures of Telegram Desktop would be blank. "
              "Start this script through a Task Scheduler task with /IT (see README.md).", flush=True)
        return 3
    args.scratch_dir.mkdir(parents=True, exist_ok=True)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    if args.diag_dir is not None:
        args.diag_dir.mkdir(parents=True, exist_ok=True)
    layout = json.loads(args.layout_json.read_text(encoding="utf-8")) if args.layout_json else {}
    if args.resume and args.out_json.exists():
        data = json.loads(args.out_json.read_text(encoding="utf-8"))
    else:
        data = {
            "part": "startup_fair",
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "machine": wm.get_machine_summary(),
            "desktop": desk,
            "settings": {
                "settle_s": args.settle_s,
                "stable_ms": args.stable_ms,
                "diff_threshold": args.diff_threshold,
                "window": args.window,
            },
            "idle_checks": [],
            "runs": {"official": [], "rustgram": []},
        }
    clients = ["official", "rustgram"] if args.clients == "both" else [args.clients]
    print(f"[fair] Checking idle CPU < {args.idle_max_cpu}% for {args.idle_check_s} s...", flush=True)
    idle = wm.check_system_idle(duration_s=args.idle_check_s, max_cpu_pct=args.idle_max_cpu)
    idle["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
    data["idle_checks"].append(idle)
    print(f"[fair] idle: passed={idle['passed']} mean={idle['mean_pct']}% max={idle['max_pct']}%", flush=True)
    disk = disk_idle_check(min(args.idle_check_s, 10.0), args.disk_idle_min)
    disk["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
    data.setdefault("disk_idle_checks", []).append(disk)
    print(f"[fair] disk idle: {disk}", flush=True)
    if not idle["passed"] or not disk["passed"]:
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")
        return 2
    for run in range(1, args.runs + 1):
        for cl in clients:
            if any(r["run"] == run for r in data["runs"][cl]):
                continue
            exe = args.official_exe if cl == "official" else args.rustgram_exe
            rec = one_run(cl, run, args, layout)
            rec["exe_size_bytes"] = exe.stat().st_size
            data["runs"][cl].append(rec)
            data["runs"][cl].sort(key=lambda r: r["run"])
            summarize(data)
            data["updated_utc"] = datetime.now(timezone.utc).isoformat()
            args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except wm.WorkdirCleanupError as exc:
        sys.stderr.write(f"[fair] {exc}\n")
        raise SystemExit(4)
