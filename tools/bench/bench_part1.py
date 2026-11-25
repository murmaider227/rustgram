#!/usr/bin/env python3
"""Part 1 benchmark: no-account cold start, idle memory/CPU (30 s & 5 min), and install size.

Compares official Telegram Desktop against Rustgram using fresh empty working directories
per run. All paths are supplied via CLI arguments or environment variables (no hardcoded paths).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import win_metrics as wm


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--official-exe", type=Path, required=True, help="Path to official Telegram.exe")
    p.add_argument("--official-dir", type=Path, required=True, help="Path to official portable folder")
    p.add_argument("--official-zip", type=Path, default=None, help="Optional official portable zip for size reference")
    p.add_argument("--rustgram-exe", type=Path, required=True, help="Path to Rustgram rustgram.exe")
    p.add_argument("--rustgram-dir", type=Path, required=True, help="Path to staged Rustgram portable folder")
    p.add_argument("--rustgram-zip", type=Path, default=None, help="Optional Rustgram portable zip for size reference")
    p.add_argument("--scratch-dir", type=Path, required=True, help="Directory for temporary empty workdirs")
    p.add_argument("--out-json", type=Path, required=True, help="Output JSON path (written incrementally)")
    p.add_argument("--diag-dir", type=Path, default=None, help="Optional folder for diagnostic PNGs of run 1")
    p.add_argument("--api-creds-doc", type=Path, default=None, help="Optional path to tdesktop api_credentials.md for live mode")
    p.add_argument("--rustgram-live", action="store_true", help="Launch Rustgram with --live -testmode (requires API creds)")
    p.add_argument("--runs", type=int, default=5, help="Number of runs per client (default: 5)")
    p.add_argument("--clients", choices=["both", "official", "rustgram"], default="both")
    p.add_argument("--skip-5min", action="store_true", help="Skip 5-minute idle measurement (for quick smoke tests only)")
    p.add_argument("--idle-check-s", type=float, default=30.0, help="Pre-run idle CPU check duration in seconds")
    p.add_argument("--idle-max-cpu", type=float, default=15.0, help="Max allowed system CPU %% during idle check")
    p.add_argument("--stable-ms", type=float, default=500.0, help="Stability duration in ms for ready-frame detection")
    p.add_argument("--diff-threshold", type=float, default=0.001, help="Max fraction of changed pixels for stability")
    p.add_argument("--resume", action="store_true", help="Resume from existing --out-json if present")
    return p.parse_args()


def wait_for_window_and_ready(
    pid: int,
    t_launch: float,
    stable_ms: float = 500.0,
    diff_threshold: float = 0.001,
    timeout_s: float = 45.0,
    diag_png: Path | None = None,
) -> dict[str, Any]:
    """Poll EnumWindows/IsWindowVisible for first visible window, then PrintWindow for stable frame."""
    deadline = t_launch + timeout_s
    hwnd: int | None = None
    t_visible: float | None = None
    win_title = ""
    win_size = (0, 0)

    while time.perf_counter() < deadline:
        wins = wm.list_visible_windows_for_pid(pid, min_w=200, min_h=150)
        if wins:
            t_visible = time.perf_counter()
            hwnd = wins[0].hwnd
            win_title = wins[0].title
            win_size = (wins[0].width, wins[0].height)
            break
        time.sleep(0.005)

    if hwnd is None or t_visible is None:
        return {
            "window_found": False,
            "first_visible_ms": None,
            "login_ready_ms": None,
            "window_title": "",
            "window_size": [0, 0],
        }

    # Capture frames via PrintWindow until a non-blank frame holds stable for `stable_ms`.
    stable_s = stable_ms / 1000.0
    blank_cutoff_s = 8.0
    prev_frame = None
    stable_since: float | None = None
    ready_time: float | None = None
    first_non_blank_t: float | None = None
    last_frame = None
    frames_captured = 0
    last_diff = 1.0
    printwindow_blank = False

    while time.perf_counter() < deadline:
        t_cap = time.perf_counter()
        frame = wm.capture_window_printwindow(hwnd)
        if frame is None:
            # Refresh HWND in case the app recreated its top-level window.
            wins = wm.list_visible_windows_for_pid(pid, min_w=200, min_h=150)
            if wins:
                hwnd = wins[0].hwnd
                win_size = (wins[0].width, wins[0].height)
            time.sleep(0.02)
            continue

        frames_captured += 1
        last_frame = frame

        if not wm.frame_is_non_blank(frame):
            prev_frame = frame
            stable_since = None
            if first_non_blank_t is None and (t_cap - t_visible) >= blank_cutoff_s:
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
        last_diff = diff
        if diff <= diff_threshold:
            if stable_since is None:
                stable_since = t_cap
            elif (t_cap - stable_since) >= stable_s:
                ready_time = stable_since
                break
        else:
            prev_frame = frame
            stable_since = t_cap

        time.sleep(0.03)

    if diag_png is not None and last_frame is not None:
        wm.save_png(last_frame, diag_png)

    return {
        "window_found": True,
        "first_visible_ms": round((t_visible - t_launch) * 1000.0, 1),
        "first_non_blank_ms": round((first_non_blank_t - t_launch) * 1000.0, 1) if first_non_blank_t is not None else None,
        "login_ready_ms": round((ready_time - t_launch) * 1000.0, 1) if ready_time is not None else None,
        "printwindow_blank": printwindow_blank,
        "last_diff_ratio": round(last_diff, 6),
        "frames_captured": frames_captured,
        "window_title": win_title,
        "window_size": list(win_size),
    }


def run_single_client(
    client: str,
    run_idx: int,
    exe_path: Path,
    workdir: Path,
    extra_args: list[str],
    extra_env: dict[str, str],
    stable_ms: float,
    diff_threshold: float,
    skip_5min: bool,
    diag_png: Path | None,
) -> dict[str, Any]:
    if workdir.exists():
        shutil.rmtree(workdir, ignore_errors=True)
    workdir.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env.update(extra_env)

    cmd = [str(exe_path), *extra_args, "-workdir", str(workdir)]
    print(f"[part1] {client} run {run_idx}: launching...", flush=True)

    t_launch = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        cwd=str(exe_path.parent),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        startup = wait_for_window_and_ready(
            proc.pid,
            t_launch,
            stable_ms=stable_ms,
            diff_threshold=diff_threshold,
            timeout_s=45.0,
            diag_png=diag_png,
        )
        print(
            f"[part1] {client} run {run_idx}: visible={startup['first_visible_ms']} ms, "
            f"ready={startup['login_ready_ms']} ms, size={startup['window_size']}",
            flush=True,
        )

        # Wait until t_launch + 30s
        rem_30 = (t_launch + 30.0) - time.perf_counter()
        if rem_30 > 0:
            time.sleep(rem_30)
        mem_30s = wm.get_process_tree_memory(proc.pid)
        cpu_30s = wm.sample_process_cpu_pct(proc.pid, window_s=5.0)
        print(
            f"[part1] {client} run {run_idx} @30s: priv={mem_30s['private_bytes']/1048576:.1f} MiB, "
            f"ws={mem_30s['working_set_bytes']/1048576:.1f} MiB, cpu={cpu_30s['cpu_pct_total']:.3f}% "
            f"(tree procs={mem_30s['tree_process_count']})",
            flush=True,
        )

        mem_5m = None
        cpu_5m = None
        if not skip_5min:
            rem_300 = (t_launch + 300.0) - time.perf_counter()
            if rem_300 > 0:
                time.sleep(rem_300)
            mem_5m = wm.get_process_tree_memory(proc.pid)
            cpu_5m = wm.sample_process_cpu_pct(proc.pid, window_s=5.0)
            print(
                f"[part1] {client} run {run_idx} @5m:  priv={mem_5m['private_bytes']/1048576:.1f} MiB, "
                f"ws={mem_5m['working_set_bytes']/1048576:.1f} MiB, cpu={cpu_5m['cpu_pct_total']:.3f}%",
                flush=True,
            )

        return {
            "run": run_idx,
            "timestamp_utc": datetime.now(timezone.utc).isoformat(),
            "startup": startup,
            "at_30s": {"memory": mem_30s, "cpu": cpu_30s},
            "at_5min": {"memory": mem_5m, "cpu": cpu_5m} if not skip_5min else None,
        }
    finally:
        wm.stop_process_by_pid(proc.pid, graceful_timeout_s=5.0)
        time.sleep(1.5)
        shutil.rmtree(workdir, ignore_errors=True)


def summarize_client_runs(runs: list[dict[str, Any]]) -> dict[str, Any]:
    vis = [r["startup"]["first_visible_ms"] for r in runs if r["startup"]["first_visible_ms"] is not None]
    rdy = [r["startup"]["login_ready_ms"] for r in runs if r["startup"]["login_ready_ms"] is not None]

    priv_30 = [r["at_30s"]["memory"]["private_bytes"] / 1048576.0 for r in runs]
    ws_30 = [r["at_30s"]["memory"]["working_set_bytes"] / 1048576.0 for r in runs]
    tpriv_30 = [r["at_30s"]["memory"]["tree_private_bytes"] / 1048576.0 for r in runs]
    tws_30 = [r["at_30s"]["memory"]["tree_working_set_bytes"] / 1048576.0 for r in runs]
    cpu_30 = [r["at_30s"]["cpu"]["cpu_pct_total"] for r in runs]

    has_5m = [r for r in runs if r.get("at_5min") is not None]
    priv_5m = [r["at_5min"]["memory"]["private_bytes"] / 1048576.0 for r in has_5m]
    ws_5m = [r["at_5min"]["memory"]["working_set_bytes"] / 1048576.0 for r in has_5m]
    tpriv_5m = [r["at_5min"]["memory"]["tree_private_bytes"] / 1048576.0 for r in has_5m]
    tws_5m = [r["at_5min"]["memory"]["tree_working_set_bytes"] / 1048576.0 for r in has_5m]
    cpu_5m = [r["at_5min"]["cpu"]["cpu_pct_total"] for r in has_5m]

    return {
        "first_visible_ms": wm.summarize_values(vis),
        "login_ready_ms": wm.summarize_values(rdy),
        "idle_30s_private_mib": wm.summarize_values(priv_30),
        "idle_30s_working_set_mib": wm.summarize_values(ws_30),
        "idle_30s_tree_private_mib": wm.summarize_values(tpriv_30),
        "idle_30s_tree_working_set_mib": wm.summarize_values(tws_30),
        "idle_30s_cpu_pct": wm.summarize_values(cpu_30),
        "idle_5min_private_mib": wm.summarize_values(priv_5m),
        "idle_5min_working_set_mib": wm.summarize_values(ws_5m),
        "idle_5min_tree_private_mib": wm.summarize_values(tpriv_5m),
        "idle_5min_tree_working_set_mib": wm.summarize_values(tws_5m),
        "idle_5min_cpu_pct": wm.summarize_values(cpu_5m),
    }


def main() -> int:
    args = parse_args()
    args.scratch_dir.mkdir(parents=True, exist_ok=True)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)

    rustgram_env: dict[str, str] = {"TGFORK_HEADLESS_AUDIO": "1"}
    rustgram_args: list[str] = []
    if args.rustgram_live:
        rustgram_args.extend(["--live", "-testmode"])
        api_id = os.environ.get("RUSTGRAM_API_ID") or os.environ.get("TGFORK_API_ID")
        api_hash = os.environ.get("RUSTGRAM_API_HASH") or os.environ.get("TGFORK_API_HASH")
        if (not api_id or not api_hash) and args.api_creds_doc is not None:
            api_id, api_hash = wm.read_test_api_credentials(args.api_creds_doc)
        if not api_id or not api_hash:
            raise RuntimeError("--rustgram-live requires RUSTGRAM_API_ID/RUSTGRAM_API_HASH or --api-creds-doc")
        rustgram_env["RUSTGRAM_API_ID"] = api_id
        rustgram_env["RUSTGRAM_API_HASH"] = api_hash

    data: dict[str, Any]
    if args.resume and args.out_json.exists():
        data = json.loads(args.out_json.read_text(encoding="utf-8"))
    else:
        data = {
            "part": "part1_no_account",
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "machine": wm.get_machine_summary(),
            "config": {
                "runs": args.runs,
                "stable_ms": args.stable_ms,
                "diff_threshold": args.diff_threshold,
                "rustgram_live_mode": args.rustgram_live,
                "skip_5min": args.skip_5min,
            },
            "idle_checks": [],
            "sizes": {},
            "runs": {"official": [], "rustgram": []},
            "summary": {},
        }

    # Measure sizes once.
    off_size = wm.measure_install_size(args.official_exe, args.official_dir)
    if args.official_zip and args.official_zip.is_file():
        off_size["portable_zip_bytes"] = args.official_zip.stat().st_size
        off_size["portable_zip_mib"] = round(off_size["portable_zip_bytes"] / 1048576.0, 2)
    rg_size = wm.measure_install_size(args.rustgram_exe, args.rustgram_dir)
    if args.rustgram_zip and args.rustgram_zip.is_file():
        rg_size["portable_zip_bytes"] = args.rustgram_zip.stat().st_size
        rg_size["portable_zip_mib"] = round(rg_size["portable_zip_bytes"] / 1048576.0, 2)
    data["sizes"] = {"official": off_size, "rustgram": rg_size}

    # Idle check before starting measurements.
    print(f"[part1] Checking system idle (CPU < {args.idle_max_cpu}% for {args.idle_check_s} s)...", flush=True)
    idle = wm.check_system_idle(duration_s=args.idle_check_s, max_cpu_pct=args.idle_max_cpu)
    idle["timestamp_utc"] = datetime.now(timezone.utc).isoformat()
    data["idle_checks"].append(idle)
    print(
        f"[part1] Idle check: passed={idle['passed']}, mean={idle['mean_pct']}%, max={idle['max_pct']}%",
        flush=True,
    )
    if not idle["passed"]:
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")
        sys.stderr.write("[part1] System CPU did not stay below threshold for 30 s; aborting.\n")
        return 2

    def save() -> None:
        data["summary"] = {
            "official": summarize_client_runs(data["runs"]["official"]),
            "rustgram": summarize_client_runs(data["runs"]["rustgram"]),
        }
        data["updated_utc"] = datetime.now(timezone.utc).isoformat()
        args.out_json.write_text(json.dumps(data, indent=2), encoding="utf-8")

    save()

    # Interleave runs when running both clients to avoid drift bias.
    clients_to_run = ["official", "rustgram"] if args.clients == "both" else [args.clients]
    for run_num in range(1, args.runs + 1):
        for cl in clients_to_run:
            done_nums = {r["run"] for r in data["runs"][cl]}
            if run_num in done_nums:
                continue
            diag_png = (args.diag_dir / f"part1_{cl}_run{run_num}.png") if (args.diag_dir and run_num == 1) else None
            if cl == "official":
                rec = run_single_client(
                    client="official",
                    run_idx=run_num,
                    exe_path=args.official_exe,
                    workdir=args.scratch_dir / f"official_empty_{run_num}",
                    extra_args=[],
                    extra_env={},
                    stable_ms=args.stable_ms,
                    diff_threshold=args.diff_threshold,
                    skip_5min=args.skip_5min,
                    diag_png=diag_png,
                )
            else:
                rec = run_single_client(
                    client="rustgram",
                    run_idx=run_num,
                    exe_path=args.rustgram_exe,
                    workdir=args.scratch_dir / f"rustgram_empty_{run_num}",
                    extra_args=rustgram_args,
                    extra_env=rustgram_env,
                    stable_ms=args.stable_ms,
                    diff_threshold=args.diff_threshold,
                    skip_5min=args.skip_5min,
                    diag_png=diag_png,
                )
            data["runs"][cl].append(rec)
            data["runs"][cl].sort(key=lambda r: r["run"])
            save()
            # Brief cooldown between runs.
            time.sleep(3.0)

    print(f"[part1] Complete. Saved {args.out_json}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
