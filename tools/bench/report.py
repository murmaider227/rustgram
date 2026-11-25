#!/usr/bin/env python3
"""Combine Part 1 and Part 2 benchmark JSON files into final results.json and BENCHMARKS.md.

Ensures no machine names, user names, local paths, phone numbers, or account names appear
in the published outputs.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import verify_report_privacy as vrp  # noqa: E402


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--part1", type=Path, required=True, help="Part 1 JSON output")
    p.add_argument("--part2-cold", type=Path, default=None, help="Part 2 cold-start JSON")
    p.add_argument("--part2-ui", type=Path, default=None, help="Part 2 interactive JSON")
    p.add_argument("--part2-delivery-rustgram", type=Path, default=None)
    p.add_argument("--part2-delivery-official", type=Path, default=None)
    p.add_argument("--official-version", default="7.4.6")
    p.add_argument("--rustgram-version", default="0.1.0 (commit 240ba8ff)")
    p.add_argument("--notes-json", type=Path, default=None, help="Optional JSON with extra caveats/not_measured notes")
    p.add_argument("--out-json", type=Path, required=True, help="Combined results.json output")
    p.add_argument("--out-md", type=Path, required=True, help="Published BENCHMARKS.md output")
    return p.parse_args()


def fmt_num(val: float | None, unit: str = "", digits: int = 1) -> str:
    if val is None:
        return "—"
    s = f"{val:.{digits}f}"
    return f"{s} {unit}".strip()


def fmt_raw(vals: list[float] | None, digits: int = 1) -> str:
    if not vals:
        return "—"
    return ", ".join(f"{v:.{digits}f}" for v in vals)


def ratio_str(rg: float | None, off: float | None, lower_is_better: bool = True) -> str:
    if rg is None or off is None or off == 0 or rg == 0:
        return "—"
    if lower_is_better:
        r = rg / off
        if r < 1.0:
            return f"{1.0 / r:.2f}× lower"
        return f"{r:.2f}× higher"
    else:
        r = rg / off
        if r > 1.0:
            return f"{r:.2f}× higher"
        return f"{1.0 / r:.2f}× lower"


def main() -> int:
    args = parse_args()
    p1 = json.loads(args.part1.read_text(encoding="utf-8"))
    p2_cold = json.loads(args.part2_cold.read_text(encoding="utf-8")) if args.part2_cold and args.part2_cold.exists() else None
    p2_ui = json.loads(args.part2_ui.read_text(encoding="utf-8")) if args.part2_ui and args.part2_ui.exists() else None
    p2_del_rg = json.loads(args.part2_delivery_rustgram.read_text(encoding="utf-8")) if args.part2_delivery_rustgram and args.part2_delivery_rustgram.exists() else None
    p2_del_off = json.loads(args.part2_delivery_official.read_text(encoding="utf-8")) if args.part2_delivery_official and args.part2_delivery_official.exists() else None
    notes = json.loads(args.notes_json.read_text(encoding="utf-8")) if args.notes_json and args.notes_json.exists() else {}

    combined: dict[str, Any] = {
        "generated_utc": datetime.now(timezone.utc).isoformat(),
        "clients": {
            "official": {
                "name": "Telegram Desktop",
                "version": args.official_version,
                "packaging": "Official signed Windows x64 portable build",
            },
            "rustgram": {
                "name": "Rustgram",
                "version": args.rustgram_version,
                "packaging": "Release build (thin LTO, 4 codegen units) staged via `cargo run -p tg-xtask -- dist` with FFmpeg shared DLLs",
            },
        },
        "machine": p1["machine"],
        "part1_no_account": p1,
        "part2_test_account": {
            "cold_start": p2_cold,
            "interactive": p2_ui,
            "delivery_rustgram": p2_del_rg,
            "delivery_official": p2_del_off,
        },
        "notes": notes,
    }

    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    args.out_md.parent.mkdir(parents=True, exist_ok=True)
    args.out_json.write_text(json.dumps(combined, indent=2), encoding="utf-8")

    # Build Markdown report
    m = p1["machine"]
    s1_off = p1["summary"]["official"]
    s1_rg = p1["summary"]["rustgram"]
    sz_off = p1["sizes"]["official"]
    sz_rg = p1["sizes"]["rustgram"]

    lines: list[str] = []
    lines.append("# Benchmarks: Telegram Desktop vs Rustgram")
    lines.append("")
    lines.append(
        f"Measured comparison between **Telegram Desktop {args.official_version}** (C++/Qt, official signed portable build) "
        f"and **Rustgram {args.rustgram_version}** (Rust/egui/eframe release build packaged via `tg-xtask dist`)."
    )
    lines.append("")
    lines.append("## Test Machine")
    lines.append("")
    lines.append(f"- **CPU:** {m['cpu_model']} ({m['logical_processors']} logical processors)")
    lines.append(f"- **RAM:** {m['ram_gib']} GiB")
    win_ver = f"{m['os']} {m['windows_display_version']} (Build {m['windows_build']})".strip()
    lines.append(f"- **OS:** {win_ver}")
    lines.append("")

    # Idle check summary
    lines.append("## Pre-Run Idle Gate")
    lines.append("")
    lines.append("Before each benchmark part, system-wide CPU utilization was verified to remain below 15% continuously for 30 s via `GetSystemTimes` (1 Hz sampling):")
    lines.append("")
    for idx, chk in enumerate(p1.get("idle_checks", []), 1):
        lines.append(
            f"- **Part 1 idle check #{idx}:** passed={chk['passed']}, mean={chk['mean_pct']}%, "
            f"median={chk['median_pct']}%, max={chk['max_pct']}% over {chk['duration_s']} s"
        )
    if p2_cold:
        for idx, chk in enumerate(p2_cold.get("idle_checks", []), 1):
            lines.append(
                f"- **Part 2 (cold start) idle check #{idx}:** passed={chk['passed']}, mean={chk['mean_pct']}%, "
                f"median={chk['median_pct']}%, max={chk['max_pct']}% over {chk['duration_s']} s"
            )
    if p2_ui:
        for idx, chk in enumerate(p2_ui.get("idle_checks", []), 1):
            lines.append(
                f"- **Part 2 (interactive) idle check #{idx}:** passed={chk['passed']}, mean={chk['mean_pct']}%, "
                f"median={chk['median_pct']}%, max={chk['max_pct']}% over {chk['duration_s']} s"
            )
    if p2_del_rg and p2_del_rg.get("idle_check"):
        chk = p2_del_rg["idle_check"]
        lines.append(
            f"- **Part 2 (delivery, Rustgram) idle check:** passed={chk['passed']}, mean={chk['mean_pct']}%, "
            f"median={chk['median_pct']}%, max={chk['max_pct']}% over {chk['duration_s']} s"
        )
    if p2_del_off and p2_del_off.get("idle_check"):
        chk = p2_del_off["idle_check"]
        lines.append(
            f"- **Part 2 (delivery, Official) idle check:** passed={chk['passed']}, mean={chk['mean_pct']}%, "
            f"median={chk['median_pct']}%, max={chk['max_pct']}% over {chk['duration_s']} s"
        )
    lines.append("")

    # Median summary table
    lines.append("## Median Summary Table")
    lines.append("")
    lines.append("| Metric | Telegram Desktop " + args.official_version + " | Rustgram | Comparison |")
    lines.append("|---|---:|---:|---|")

    def add_row(label: str, off_v: float | None, rg_v: float | None, unit: str, digits: int = 1, lower_better: bool = True) -> None:
        lines.append(
            f"| {label} | {fmt_num(off_v, unit, digits)} | {fmt_num(rg_v, unit, digits)} | "
            f"{ratio_str(rg_v, off_v, lower_is_better=lower_better)} |"
        )

    # Size rows
    add_row("Main executable size", sz_off["exe_mib"], sz_rg["exe_mib"], "MiB", 2, True)
    add_row("Installed portable folder size", sz_off["folder_mib"], sz_rg["folder_mib"], "MiB", 2, True)
    if sz_off.get("portable_zip_mib") is not None and sz_rg.get("portable_zip_mib") is not None:
        add_row("Portable ZIP archive size", sz_off["portable_zip_mib"], sz_rg["portable_zip_mib"], "MiB", 2, True)

    # Part 1 rows
    add_row("Part 1: Cold start → first window visible", s1_off["first_visible_ms"]["median"], s1_rg["first_visible_ms"]["median"], "ms", 1, True)
    add_row("Part 1: Cold start → login screen ready", s1_off["login_ready_ms"]["median"], s1_rg["login_ready_ms"]["median"], "ms", 1, True)
    add_row("Part 1: Idle private bytes @ 30 s", s1_off["idle_30s_private_mib"]["median"], s1_rg["idle_30s_private_mib"]["median"], "MiB", 1, True)
    add_row("Part 1: Idle working set @ 30 s", s1_off["idle_30s_working_set_mib"]["median"], s1_rg["idle_30s_working_set_mib"]["median"], "MiB", 1, True)
    add_row("Part 1: Idle CPU @ 30 s (5 s avg)", s1_off["idle_30s_cpu_pct"]["median"], s1_rg["idle_30s_cpu_pct"]["median"], "%", 3, True)
    add_row("Part 1: Idle private bytes @ 5 min", s1_off["idle_5min_private_mib"]["median"], s1_rg["idle_5min_private_mib"]["median"], "MiB", 1, True)
    add_row("Part 1: Idle working set @ 5 min", s1_off["idle_5min_working_set_mib"]["median"], s1_rg["idle_5min_working_set_mib"]["median"], "MiB", 1, True)
    add_row("Part 1: Idle CPU @ 5 min (5 s avg)", s1_off["idle_5min_cpu_pct"]["median"], s1_rg["idle_5min_cpu_pct"]["median"], "%", 3, True)

    # Part 2 rows
    if p2_cold:
        sc_o = p2_cold["summary"]["official"]
        sc_r = p2_cold["summary"]["rustgram"]
        add_row("Part 2: Start → first window visible (account A)", sc_o["first_visible_ms"]["median"], sc_r["first_visible_ms"]["median"], "ms", 1, True)
        add_row("Part 2: Start → chat list loaded (account A)", sc_o["chat_list_ready_ms"]["median"], sc_r["chat_list_ready_ms"]["median"], "ms", 1, True)
        add_row("Part 2: Post-load private bytes (account A)", sc_o["post_load_private_mib"]["median"], sc_r["post_load_private_mib"]["median"], "MiB", 1, True)
        add_row("Part 2: Post-load working set (account A)", sc_o["post_load_working_set_mib"]["median"], sc_r["post_load_working_set_mib"]["median"], "MiB", 1, True)
    if p2_ui:
        su_o = p2_ui["summary"]["official"]
        su_r = p2_ui["summary"]["rustgram"]
        add_row("Part 2: Open chat → first paint", su_o["open_chat_first_paint_ms"]["median"], su_r["open_chat_first_paint_ms"]["median"], "ms", 1, True)
        add_row("Part 2: Open chat → history stable", su_o["open_chat_stable_ms"]["median"], su_r["open_chat_stable_ms"]["median"], "ms", 1, True)
        add_row("Part 2: 10 s scroll effective FPS (PrintWindow-capped)", su_o["scroll_effective_fps"]["median"], su_r["scroll_effective_fps"]["median"], "fps", 2, False)
        add_row("Part 2: 10 s scroll frame interval p50", su_o["scroll_p50_ms"]["median"], su_r["scroll_p50_ms"]["median"], "ms", 2, True)
        add_row("Part 2: 10 s scroll frame interval p95", su_o["scroll_p95_ms"]["median"], su_r["scroll_p95_ms"]["median"], "ms", 2, True)
        add_row("Part 2: 10 s scroll CPU % (total machine)", su_o["scroll_cpu_pct_total"]["median"], su_r["scroll_cpu_pct_total"]["median"], "%", 2, True)
        add_row("Part 2: 10 s scroll CPU % (1 core equiv.)", su_o["scroll_cpu_pct_single_core"]["median"], su_r["scroll_cpu_pct_single_core"]["median"], "%", 1, True)
        chats_o = p2_ui["mem_10_chats"]["official"][0]["chats_opened"] if p2_ui["mem_10_chats"]["official"] else 0
        chats_r = p2_ui["mem_10_chats"]["rustgram"][0]["chats_opened"] if p2_ui["mem_10_chats"]["rustgram"] else 0
        chat_label = f"{chats_r}" if chats_o == chats_r else (f"{chats_r}" if chats_o == 0 else f"Official {chats_o} / Rustgram {chats_r}")
        add_row(f"Part 2: Private bytes after opening {chat_label} chats", su_o["mem_10_chats_private_mib"]["median"], su_r["mem_10_chats_private_mib"]["median"], "MiB", 1, True)
        add_row(f"Part 2: Working set after opening {chat_label} chats", su_o["mem_10_chats_working_set_mib"]["median"], su_r["mem_10_chats_working_set_mib"]["median"], "MiB", 1, True)

    del_off_med = p2_del_off["summary"]["median"] if p2_del_off else None
    del_rg_med = p2_del_rg["summary"]["median"] if p2_del_rg else None
    if del_off_med is not None or del_rg_med is not None:
        add_row("Part 2: Message delivery A → B (UI arrival in B)", del_off_med, del_rg_med, "ms", 1, True)
    lines.append("")

    # Size breakdown
    lines.append("## Installed Folder File Breakdown")
    lines.append("")
    lines.append(f"### Telegram Desktop {args.official_version} ({sz_off['folder_mib']:.2f} MiB total)")
    for f in sz_off["files"]:
        lines.append(f"- `{f['name']}`: {f['bytes'] / 1048576.0:.2f} MiB ({f['bytes']:,} bytes)")
    lines.append("")
    lines.append(f"### Rustgram ({sz_rg['folder_mib']:.2f} MiB total)")
    for f in sz_rg["files"]:
        lines.append(f"- `{f['name']}`: {f['bytes'] / 1048576.0:.2f} MiB ({f['bytes']:,} bytes)")
    lines.append("")

    # Method section
    lines.append("## Methodology")
    lines.append("")
    lines.append("- **Cold start (Part 1 & Part 2):** Each run launches the binary with a fresh working directory (empty for Part 1; a fresh copy of the test-account `tdata` without `working`/`log*` for Part 2). `first_window_visible` polls `EnumWindows` + `IsWindowVisible` every 5 ms for a top-level unowned window ≥ 200×150 px. Ready time captures the window via `PrintWindow(PW_RENDERFULLCONTENT)` and finds the first non-blank frame after which consecutive captures stay within ≤ 0.1% changed pixels (RGB channel diff > 8) continuously for 500 ms. Each process is closed gracefully by PID after its run and temporary workdirs are deleted.")
    lines.append("- **Idle memory & CPU (Part 1):** Sampled at 30 s and 5 min after launch via `GetProcessMemoryInfo` (`PrivateUsage` for private committed bytes, `WorkingSetSize` for working set) and `GetProcessTimes` over a 5 s window normalized by total logical processors (matching Task Manager).")
    lines.append("- **Install size (Part 1):** Exact filesystem byte counts for the main executable and all files in the portable distribution directory (zero-byte startup log files excluded).")
    lines.append("- **Open chat (Part 2):** With the window fixed at 1200×800, switches from the alternate chat to the 200-message A↔B chat via `PostMessageW` mouse click (fallback for isolated desktop where `SendInput` targets only the active desktop) and polls the history pane via `PrintWindow(PW_RENDERFULLCONTENT)` until the pane updates (`first_paint`) and then stays stable for 500 ms (`stable`).")
    lines.append("- **10 s wheel scroll (Part 2):** Sends steady `WM_MOUSEWHEEL` ticks at 40 Hz for 10 s (alternating up/down every 2.5 s within the 200-message A↔B chat so scrolling never stalls at an endpoint) while polling the history pane via `PrintWindow(PW_RENDERFULLCONTENT)`. Reports process-tree CPU % (`GetProcessTimes`), unique visually distinct frames/sec, and p50/p95 inter-frame intervals.")
    lines.append("- **Memory after opening chats (Part 2):** Starts a fresh process per run on a fresh workdir copy, clicks through every available distinct chat on test account A with a 2 s settle delay each, waits 5 s, and records `PrivateUsage` and `WorkingSetSize`.")
    lines.append("- **Message delivery A → B (Part 2):** Runs Rustgram A and Rustgram B concurrently with the A↔B chat open, sends a unique short message from A via `WM_KEYDOWN`/`WM_CHAR`/`WM_KEYUP` + Enter (`perf_counter` timestamp), and polls Rustgram B's dialog-row unread count badge via `PrintWindow` until the badge updates. All created test messages (both the 200 scroll messages and delivery test messages) were deleted from both A and B afterward.")
    lines.append("")

    # Raw runs
    lines.append("## Raw Runs")
    lines.append("")
    lines.append("### Part 1 — No Account")
    lines.append("")
    lines.append("| Metric | Telegram Desktop (raw) | Rustgram (raw) |")
    lines.append("|---|---|---|")
    for label, key, digits in [
        ("First visible (ms)", "first_visible_ms", 1),
        ("Login ready (ms)", "login_ready_ms", 1),
        ("Idle private @ 30 s (MiB)", "idle_30s_private_mib", 1),
        ("Idle working set @ 30 s (MiB)", "idle_30s_working_set_mib", 1),
        ("Idle CPU @ 30 s (%)", "idle_30s_cpu_pct", 3),
        ("Idle private @ 5 min (MiB)", "idle_5min_private_mib", 1),
        ("Idle working set @ 5 min (MiB)", "idle_5min_working_set_mib", 1),
        ("Idle CPU @ 5 min (%)", "idle_5min_cpu_pct", 3),
    ]:
        lines.append(f"| {label} | {fmt_raw(s1_off[key]['raw'], digits)} | {fmt_raw(s1_rg[key]['raw'], digits)} |")
    lines.append("")

    lines.append("### Part 2 — Test Account A")
    lines.append("")
    lines.append("| Metric | Telegram Desktop (raw) | Rustgram (raw) |")
    lines.append("|---|---|---|")
    if p2_cold:
        sc_o = p2_cold["summary"]["official"]
        sc_r = p2_cold["summary"]["rustgram"]
        lines.append(f"| Start → first visible (ms) | {fmt_raw(sc_o['first_visible_ms']['raw'], 1)} | {fmt_raw(sc_r['first_visible_ms']['raw'], 1)} |")
        lines.append(f"| Start → chat list ready (ms) | {fmt_raw(sc_o['chat_list_ready_ms']['raw'], 1)} | {fmt_raw(sc_r['chat_list_ready_ms']['raw'], 1)} |")
        lines.append(f"| Post-load private bytes (MiB) | {fmt_raw(sc_o['post_load_private_mib']['raw'], 1)} | {fmt_raw(sc_r['post_load_private_mib']['raw'], 1)} |")
        lines.append(f"| Post-load working set (MiB) | {fmt_raw(sc_o['post_load_working_set_mib']['raw'], 1)} | {fmt_raw(sc_r['post_load_working_set_mib']['raw'], 1)} |")
    if p2_ui:
        su_o = p2_ui["summary"]["official"]
        su_r = p2_ui["summary"]["rustgram"]
        for label, key, digits in [
            ("Open chat → first paint (ms)", "open_chat_first_paint_ms", 1),
            ("Open chat → stable (ms)", "open_chat_stable_ms", 1),
            ("10 s scroll effective FPS", "scroll_effective_fps", 2),
            ("10 s scroll p50 interval (ms)", "scroll_p50_ms", 2),
            ("10 s scroll p95 interval (ms)", "scroll_p95_ms", 2),
            ("10 s scroll CPU % (total)", "scroll_cpu_pct_total", 2),
            ("10 s scroll CPU % (1 core)", "scroll_cpu_pct_single_core", 1),
            ("Memory after chats — private (MiB)", "mem_10_chats_private_mib", 1),
            ("Memory after chats — working set (MiB)", "mem_10_chats_working_set_mib", 1),
        ]:
            lines.append(f"| {label} | {fmt_raw(su_o[key]['raw'], digits)} | {fmt_raw(su_r[key]['raw'], digits)} |")
    if p2_del_rg or p2_del_off:
        raw_o = p2_del_off["summary"]["raw"] if p2_del_off else None
        raw_r = p2_del_rg["summary"]["raw"] if p2_del_rg else None
        lines.append(f"| Message delivery A → B (ms) | {fmt_raw(raw_o, 1)} | {fmt_raw(raw_r, 1)} |")
    lines.append("")

    # Caveats / not measured
    lines.append("## Caveats & Unmeasured / Non-Comparable Items")
    lines.append("")
    caveats = notes.get("caveats", [])
    if caveats:
        for c in caveats:
            lines.append(f"- {c}")
    else:
        lines.append("- None recorded.")
    lines.append("")

    args.out_md.write_text("\n".join(lines), encoding="utf-8")

    written = [args.out_json, args.out_md]
    rc = vrp.verify_files(written)
    if rc != 0:
        for fp in written:
            try:
                fp.unlink(missing_ok=True)
            except OSError:
                pass
        sys.stderr.write("[report] Aborted: privacy verification failed; removed unsafe output files.\n")
        return rc

    print(f"[report] Wrote and verified {args.out_json} and {args.out_md}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
