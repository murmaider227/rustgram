#!/usr/bin/env python3
"""Frame-time benchmark for Rustgram (TGB-2893).

Runs scripted scenarios against a release build that has the env-gated frame trace
(`crates/tg-app/src/startup/frame_trace`), several times each, and reports per scenario:
p50 / p95 / p99 / max CPU frame time, frames over 16.7 ms and 33.3 ms, and for the slow frames
the code that ran (UI-thread stack samples symbolized against the PDB).

Input comes from an in-app script (egui raw input), so runs repeat exactly and never touch the
real mouse or keyboard. Every run uses a fresh scratch workdir (demo) or a scratch copy of a test
account workdir (`-testmode`), deleted afterwards. The app is never started without `-workdir`.

  python -I frame_bench.py run --exe <release rustgram.exe> --scratch-dir <dir outside git> \
      --out-dir <dir> [--runs 5] [--scenarios chatlist,history,openchats,panels] \
      [--live-workdir <test-account workdir> --api-creds-doc <tdesktop docs/api_credentials.md>]
  python -I frame_bench.py analyze --out-dir <dir>            # re-analyze saved traces
  python -I frame_bench.py probe --exe ... --scratch-dir ... --out-dir ... [--live-workdir ...]
                                                              # screenshots to calibrate LAYOUT

Build the exe with line tables so the samples resolve to functions:
  CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release -p tg-app --bin rustgram
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import statistics
import struct
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from workdir_safety import ScratchSession  # noqa: E402

BUDGET_60 = 16.7
BUDGET_30 = 33.3

# Logical-point positions in a 1200x800 window (`size` in every script). Calibrated with `probe`.
LAYOUT: dict[str, Any] = {
    "window": (1200, 800),
    "chatlist_point": (270, 500),
    "history_point": (840, 450),
    "row_y0": 147,  # centre of the first row (Archived chats) in the demo
    "row_h": 62,
    "row_x": 270,
    "first_chat_row": 1,
    "bench_history_row": 3,
    "emoji_button": (1131, 775),
    "panel_point": (1050, 450),
    "panel_tabs": {"emoji": (956, 45), "stickers": (1053, 45), "gifs": (1151, 45)},
}


def row_point(i: int) -> tuple[int, int]:
    return LAYOUT["row_x"], LAYOUT["row_y0"] + i * LAYOUT["row_h"]


def script_chatlist() -> str:
    x, y = LAYOUT["chatlist_point"]
    return "\n".join(
        [
            f"move {x} {y}",
            "wait 1500",
            "section chatlist_scroll",
            "wheel down 40 10000",
            "wheel up 40 10000",
            "end",
        ]
    )


def script_history() -> str:
    rx, ry = row_point(LAYOUT["bench_history_row"])
    hx, hy = LAYOUT["history_point"]
    return "\n".join(
        [
            f"click {rx} {ry}",
            "wait 2500",
            f"move {hx} {hy}",
            "wait 500",
            "section history_scroll",
            "wheel up 40 12000",
            "wheel down 40 6000",
            "end",
        ]
    )


def script_openchats() -> str:
    lines = ["section open_chats"]
    first = LAYOUT["first_chat_row"]
    rows = LAYOUT.get("open_rows") or list(range(first, first + 10))
    for i in rows:
        x, y = row_point(i)
        lines += [f"click {x} {y}", "wait 1500"]
    lines.append("end")
    return "\n".join(lines)


def script_panels() -> str:
    rx, ry = row_point(LAYOUT["bench_history_row"])
    ex, ey = LAYOUT["emoji_button"]
    px, py = LAYOUT["panel_point"]
    tabs = LAYOUT["panel_tabs"]
    lines = [f"click {rx} {ry}", "wait 2500", "section panels", f"click {ex} {ey}", "wait 1200"]
    for tab in ("emoji", "stickers", "gifs"):
        tx, ty = tabs[tab]
        lines += [
            f"click {tx} {ty}",
            "wait 1200",
            f"move {px} {py}",
            "wheel down 20 3000",
            "wheel up 20 1500",
        ]
    lines += ["key Escape", "wait 500", "end"]
    return "\n".join(lines)


SCENARIOS = {
    "chatlist": ("chatlist_scroll", script_chatlist),
    "history": ("history_scroll", script_history),
    "openchats": ("open_chats", script_openchats),
    "panels": ("panels", script_panels),
}


def wrap_script(body: str, startup_wait_ms: int, shots_dir: Path | None = None) -> str:
    w, h = LAYOUT["window"]
    lines = [f"size {w} {h}", f"wait {startup_wait_ms}", body]
    if shots_dir is not None:
        lines.append(f"shot {shots_dir / 'end.bmp'}")
        lines.append("wait 300")
    lines += ["quit"]
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# Machine state
# ---------------------------------------------------------------------------


class FILETIME(ctypes.Structure):
    _fields_ = [("lo", wintypes.DWORD), ("hi", wintypes.DWORD)]

    def v(self) -> int:
        return (self.hi << 32) | self.lo


class MEMORYSTATUSEX(ctypes.Structure):
    _fields_ = [
        ("dwLength", wintypes.DWORD),
        ("dwMemoryLoad", wintypes.DWORD),
        ("ullTotalPhys", ctypes.c_ulonglong),
        ("ullAvailPhys", ctypes.c_ulonglong),
        ("ullTotalPageFile", ctypes.c_ulonglong),
        ("ullAvailPageFile", ctypes.c_ulonglong),
        ("ullTotalVirtual", ctypes.c_ulonglong),
        ("ullAvailVirtual", ctypes.c_ulonglong),
        ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)


def system_cpu_pct(window_s: float = 3.0) -> float:
    def read() -> tuple[int, int, int]:
        i, k, u = FILETIME(), FILETIME(), FILETIME()
        kernel32.GetSystemTimes(ctypes.byref(i), ctypes.byref(k), ctypes.byref(u))
        return i.v(), k.v(), u.v()

    a = read()
    time.sleep(window_s)
    b = read()
    idle, total = b[0] - a[0], (b[1] - a[1]) + (b[2] - a[2])
    return round(100.0 * max(0, total - idle) / total, 1) if total else 0.0


def machine_state() -> dict[str, Any]:
    mem = MEMORYSTATUSEX()
    mem.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
    kernel32.GlobalMemoryStatusEx(ctypes.byref(mem))
    names: dict[str, int] = {}
    try:
        out = subprocess.run(
            ["tasklist", "/FO", "CSV", "/NH"], capture_output=True, text=True, timeout=30
        ).stdout
        for line in out.splitlines():
            parts = line.split('","')
            if parts:
                n = parts[0].strip('"').lower()
                names[n] = names.get(n, 0) + 1
    except (OSError, subprocess.SubprocessError):
        pass
    return {
        "cpu_pct_3s": system_cpu_pct(),
        "free_ram_gib": round(mem.ullAvailPhys / 2**30, 1),
        "rustc": names.get("rustc.exe", 0),
        "cargo": names.get("cargo.exe", 0),
        "rustgram": names.get("rustgram.exe", 0),
    }


# ---------------------------------------------------------------------------
# Running
# ---------------------------------------------------------------------------


def launch_env(args: argparse.Namespace, live: bool) -> dict[str, str]:
    env = dict(os.environ)
    for k in list(env):
        if k.startswith("RUSTGRAM_"):
            del env[k]
    if args.ffmpeg_bin:
        env["PATH"] = args.ffmpeg_bin + os.pathsep + env.get("PATH", "")
    if live:
        doc = Path(args.api_creds_doc).read_text(encoding="utf-8")
        import re

        api_id = re.search(r"api_id\*\*: (\d+)", doc)
        api_hash = re.search(r"api_hash\*\*: ([0-9a-f]+)", doc)
        if not api_id or not api_hash:
            raise SystemExit("test api credentials not found in --api-creds-doc")
        env["RUSTGRAM_API_ID"] = api_id.group(1)
        env["RUSTGRAM_API_HASH"] = api_hash.group(1)
    else:
        env["RUSTGRAM_BENCH_DATA"] = args.bench_data
    return env


def run_once(
    args: argparse.Namespace,
    session: ScratchSession,
    name: str,
    script_text: str,
    run_dir: Path,
    mode: str,
    live: bool,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=True)
    script_path = run_dir / "script.txt"
    script_path.write_text(script_text, encoding="utf-8")
    trace_path = run_dir / "trace.tsv"
    if live:
        workdir = session.copy_workdir(Path(args.live_workdir), f"{name}_wd")
        argv = [args.exe, "-testmode", "-workdir", str(workdir), "-many", "-noupdate"]
    else:
        workdir = session.make_empty_workdir(f"{name}_wd")
        argv = [args.exe, "--demo", "--start-main", "-workdir", str(workdir), "-many", "-noupdate"]
    env = launch_env(args, live)
    env["RUSTGRAM_FRAME_TRACE"] = mode
    env["RUSTGRAM_FRAME_TRACE_OUT"] = str(trace_path)
    env["RUSTGRAM_FRAME_SCRIPT"] = str(script_path)
    state = machine_state()
    t0 = time.monotonic()
    proc = subprocess.Popen(argv, env=env, cwd=str(run_dir))
    session.track_process(proc)
    try:
        proc.wait(timeout=args.run_timeout)
        exit_code: int | None = proc.returncode
    except subprocess.TimeoutExpired:
        exit_code = None
    elapsed = round(time.monotonic() - t0, 1)
    return {
        "run_dir": str(run_dir),
        "mode": mode,
        "exit_code": exit_code,
        "timed_out": exit_code is None,
        "wall_s": elapsed,
        "machine_before": state,
    }


def lock_out_dir(out_dir: Path) -> Any:
    """Holds an exclusive lock on `out_dir` for this process, so a second benchmark started on the
    same directory stops instead of writing into the same trace files (TGB-2949). The OS drops the
    lock when the process exits."""
    handle = open(out_dir / ".frame_bench.lock", "a+b")  # noqa: SIM115 - held until exit
    try:
        if os.name == "nt":
            import msvcrt

            handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        handle.close()
        raise SystemExit(f"another frame_bench run is writing {out_dir}; use another --out-dir")
    return handle


def cmd_run(args: argparse.Namespace) -> None:
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    _lock = lock_out_dir(out_dir)
    if args.layout:
        LAYOUT.update(json.loads(Path(args.layout).read_text(encoding="utf-8")))
    scenarios = [s for s in args.scenarios.split(",") if s]
    live = bool(args.live_workdir)
    runs: list[dict[str, Any]] = []
    for scen in scenarios:
        section, make = SCENARIOS[scen]
        body = make()
        for i in range(args.runs + args.sample_runs):
            mode = "1" if i < args.runs else "sample"
            tag = f"{'live_' if live else ''}{scen}_{'r' if mode == '1' else 's'}{i}"
            script = wrap_script(body, args.startup_wait_ms)
            # One session per run: the process tree is stopped and the workdir deleted right after.
            with ScratchSession(Path(args.scratch_dir)) as session:
                info = run_once(args, session, tag, script, out_dir / tag, mode, live)
            info.update({"scenario": scen, "section": section, "live": live, "tag": tag})
            runs.append(info)
            print(f"{tag}: exit={info['exit_code']} wall={info['wall_s']}s "
                  f"cpu_before={info['machine_before']['cpu_pct_3s']}%", flush=True)
    manifest = out_dir / ("manifest_live.json" if live else "manifest.json")
    manifest.write_text(json.dumps({"exe": args.exe, "layout": LAYOUT, "runs": runs}, indent=2),
                        encoding="utf-8")
    analyze(out_dir, args.exe)


def cmd_probe(args: argparse.Namespace) -> None:
    """Screenshots of the demo (or live) window at each calibration step."""
    out_dir = Path(args.out_dir)
    shots = out_dir / "probe"
    shots.mkdir(parents=True, exist_ok=True)
    if args.layout:
        LAYOUT.update(json.loads(Path(args.layout).read_text(encoding="utf-8")))
    body = args.probe_script.replace("\\n", "\n").replace("{shots}", str(shots))
    live = bool(args.live_workdir)
    with ScratchSession(Path(args.scratch_dir)) as session:
        info = run_once(args, session, "probe", wrap_script(body, args.startup_wait_ms), shots,
                        "1", live)
    print(json.dumps(info, indent=2))


# ---------------------------------------------------------------------------
# Trace parsing and stats
# ---------------------------------------------------------------------------


def parse_trace(
    path: Path,
) -> tuple[list[dict[str, Any]], list[tuple[int, str]], int]:
    """Frame lines, event lines and the number of malformed lines, which are skipped.

    Traces written before TGB-2949 could garble a few lines when two processes wrote one file.
    """
    frames: list[dict[str, Any]] = []
    events: list[tuple[int, str]] = []
    malformed = 0
    if not path.exists():
        return frames, events, malformed
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line:
            continue
        parts = line.split("\t")
        try:
            if parts[0] == "F" and len(parts) == 6:
                cpu = None if parts[4] == "-" else int(parts[4])
                frames.append({"i": int(parts[1]), "start_us": int(parts[2]),
                               "ui_us": int(parts[3]), "cpu_us": cpu, "section": parts[5]})
            elif parts[0] == "E" and len(parts) >= 3:
                events.append((int(parts[1]), parts[2]))
            else:
                malformed += 1
        except ValueError:
            malformed += 1
    return frames, events, malformed


def trace_mixed(frames: list[dict[str, Any]], events: list[tuple[int, str]], malformed: int) -> bool:
    """Whether more than one process wrote the trace (two runs sharing an output file): two
    `trace_start` or `trace_end` events, frame indices that skip or go back, or malformed lines
    (one process writes whole lines only). The frames of the processes are then mixed and the
    run's numbers are not valid."""
    starts = sum(1 for _, text in events if text.startswith("trace_start"))
    ends = sum(1 for _, text in events if text == "trace_end")
    broken = any(b["i"] != a["i"] + 1 for a, b in zip(frames, frames[1:]))
    return starts > 1 or ends > 1 or broken or malformed > 0


def pct(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return 0.0
    k = (len(sorted_vals) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(sorted_vals) - 1)
    return sorted_vals[lo] + (sorted_vals[hi] - sorted_vals[lo]) * (k - lo)


def frame_stats(cpu_ms: list[float]) -> dict[str, Any]:
    s = sorted(cpu_ms)
    return {
        "frames": len(s),
        "p50": round(pct(s, 50), 2),
        "p95": round(pct(s, 95), 2),
        "p99": round(pct(s, 99), 2),
        "max": round(s[-1], 2) if s else 0.0,
        "over_16_7": sum(1 for v in s if v > BUDGET_60),
        "over_33": sum(1 for v in s if v > BUDGET_30),
    }


def section_frames(frames: list[dict[str, Any]], section: str) -> list[dict[str, Any]]:
    return [f for f in frames if f["section"] == section and f["cpu_us"] is not None]


# ---------------------------------------------------------------------------
# Symbolization (dbghelp, offline against the PDB next to the exe)
# ---------------------------------------------------------------------------


class SYMBOL_INFOW(ctypes.Structure):
    _fields_ = [
        ("SizeOfStruct", wintypes.ULONG),
        ("TypeIndex", wintypes.ULONG),
        ("Reserved", ctypes.c_ulonglong * 2),
        ("Index", wintypes.ULONG),
        ("Size", wintypes.ULONG),
        ("ModBase", ctypes.c_ulonglong),
        ("Flags", wintypes.ULONG),
        ("Value", ctypes.c_ulonglong),
        ("Address", ctypes.c_ulonglong),
        ("Register", wintypes.ULONG),
        ("Scope", wintypes.ULONG),
        ("Tag", wintypes.ULONG),
        ("NameLen", wintypes.ULONG),
        ("MaxNameLen", wintypes.ULONG),
        ("Name", ctypes.c_wchar * 1024),
    ]


def find_dbghelp() -> str:
    """A dbghelp.dll that reads rust-lld PDBs (the System32 copy loads them as SymNone).

    `FRAME_BENCH_DBGHELP` wins; otherwise the newest Visual Studio or Windows Kits debugger copy.
    """
    env = os.environ.get("FRAME_BENCH_DBGHELP")
    if env:
        return env
    import glob

    patterns = [
        "C:/Program Files/Microsoft Visual Studio/*/*/Common7/IDE/CommonExtensions/Platform/"
        "DiagnosticsHub/amd64/dbghelp.dll",
        "C:/Program Files (x86)/Windows Kits/10/Debuggers/x64/dbghelp.dll",
    ]
    for pat in patterns:
        hits = sorted(glob.glob(pat), reverse=True)
        if hits:
            return hits[0]
    return "dbghelp"


class Symbolizer:
    """Resolves sampled return addresses to (inline-expanded) function names."""

    def __init__(self, modules: list[tuple[int, int, str]], search_path: str) -> None:
        self.dbg = ctypes.WinDLL(find_dbghelp(), use_last_error=True)
        self.h = wintypes.HANDLE(0x7E57)
        self.dbg.SymSetOptions(0x2 | 0x4 | 0x10 | 0x200)  # UNDNAME DEFERRED LOAD_LINES FAIL_CRITICAL
        self.dbg.SymInitializeW.argtypes = [wintypes.HANDLE, wintypes.LPCWSTR, wintypes.BOOL]
        self.dbg.SymInitializeW(self.h, search_path, False)
        self.dbg.SymLoadModuleExW.restype = ctypes.c_ulonglong
        self.dbg.SymLoadModuleExW.argtypes = [
            wintypes.HANDLE, wintypes.HANDLE, wintypes.LPCWSTR, wintypes.LPCWSTR,
            ctypes.c_ulonglong, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD,
        ]
        self.dbg.SymFromAddrW.argtypes = [wintypes.HANDLE, ctypes.c_ulonglong,
                                          ctypes.POINTER(ctypes.c_ulonglong), ctypes.c_void_p]
        self.dbg.SymAddrIncludeInlineTrace.restype = wintypes.DWORD
        self.dbg.SymAddrIncludeInlineTrace.argtypes = [wintypes.HANDLE, ctypes.c_ulonglong]
        self.dbg.SymQueryInlineTrace.argtypes = [
            wintypes.HANDLE, ctypes.c_ulonglong, wintypes.DWORD, ctypes.c_ulonglong,
            ctypes.c_ulonglong, ctypes.POINTER(wintypes.DWORD), ctypes.POINTER(wintypes.DWORD),
        ]
        self.dbg.SymFromInlineContextW.argtypes = [
            wintypes.HANDLE, ctypes.c_ulonglong, wintypes.ULONG,
            ctypes.POINTER(ctypes.c_ulonglong), ctypes.c_void_p,
        ]
        self.modules = sorted(modules)
        for base, size, path in self.modules:
            self.dbg.SymLoadModuleExW(self.h, None, path, None, base, size, None, 0)
        self.cache: dict[int, list[str]] = {}

    def module_of(self, addr: int) -> str:
        for base, size, path in self.modules:
            if base <= addr < base + size:
                return Path(path).name
        return "?"

    def _sym(self, addr: int, inline_ctx: int | None) -> str | None:
        info = SYMBOL_INFOW()
        info.SizeOfStruct = 88
        info.MaxNameLen = 1024
        disp = ctypes.c_ulonglong(0)
        if inline_ctx is None:
            ok = self.dbg.SymFromAddrW(self.h, addr, ctypes.byref(disp), ctypes.byref(info))
        else:
            ok = self.dbg.SymFromInlineContextW(self.h, addr, inline_ctx, ctypes.byref(disp),
                                                ctypes.byref(info))
        return info.Name if ok else None

    def frames(self, addr: int) -> list[str]:
        """Innermost-first names for one address (inlined callees first)."""
        cached = self.cache.get(addr)
        if cached is not None:
            return cached
        names: list[str] = []
        n = self.dbg.SymAddrIncludeInlineTrace(self.h, addr)
        if n:
            ctx, idx = wintypes.DWORD(0), wintypes.DWORD(0)
            if self.dbg.SymQueryInlineTrace(self.h, addr, 0, addr, addr, ctypes.byref(ctx),
                                            ctypes.byref(idx)):
                for i in range(n):
                    s = self._sym(addr, ctx.value + i)
                    if s:
                        names.append(s)
        s = self._sym(addr, None)
        mod = self.module_of(addr)
        if s:
            names.append(s if mod.lower().startswith("rustgram") else f"{mod}!{s}")
        else:
            names.append(f"{mod}+?")
        self.cache[addr] = names
        return names


def read_stacks(path: Path) -> list[tuple[int, list[int]]]:
    data = path.read_bytes() if path.exists() else b""
    out: list[tuple[int, list[int]]] = []
    off = 0
    while off + 12 <= len(data):
        t_ns, depth = struct.unpack_from("<QI", data, off)
        off += 12
        if off + 8 * depth > len(data):
            break
        addrs = list(struct.unpack_from(f"<{depth}Q", data, off))
        off += 8 * depth
        out.append((t_ns, addrs))
    return out


def read_modules(path: Path) -> list[tuple[int, int, str]]:
    mods = []
    if path.exists():
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            parts = line.split("\t")
            if len(parts) == 3:
                mods.append((int(parts[0], 16), int(parts[1], 16), parts[2]))
    return mods


WAIT_MARKERS = (
    "swap_buffers", "SwapBuffers", "MsgWaitForMultipleObjects", "NtUserGetMessage",
    "GetMessageW", "PeekMessage", "pump_app_events", "NtUserMsgWaitForMultipleObjectsEx",
)

CATEGORIES: list[tuple[str, tuple[str, ...]]] = [
    ("network wait (sync on UI)", ("block_on", "blocking_recv", "recv_timeout", "reqwest::blocking",
                                   "ws2_32", "mswsock")),
    ("texture upload", ("set_texture", "glTexSubImage", "glTexImage", "tex_sub_image", "tex_image_2d")),
    ("lottie/sticker render", ("tg_lottie", "rlottie", "lottie")),
    ("image decode / rasterize", ("image::codecs", "image::dynimage", "image::io", "image::imageops",
                      "decode_sheet", "tg_theme::gradient", "tiny_skia", "zune_", "jpeg_decoder", "png::", "gif::", "image_webp", "decode_inline_photo",
                      "decode_rgba", "stripped", "resvg", "usvg", "tg_media::image")),
    ("video decode", ("avcodec", "avformat", "swscale", "tg_video")),
    ("disk I/O", ("NtReadFile", "NtWriteFile", "NtCreateFile", "NtQueryAttributesFile",
                  "NtQueryDirectoryFile", "std::fs::", "rusqlite", "sqlite3", "FindFirstFile")),
    ("text layout / glyphs", ("epaint::text", "Fonts::", "galley", "ab_glyph", "skrifa", "font_kit",
                              "layout_job", "tg_text::", "rasterize")),
    ("tessellation", ("tessellat",)),
    ("GL draw submit", ("egui_glow", "opengl32", "nvoglv64", "atio6axx", "ig9icd64", "igxelpicd64")),
    ("lock wait", ("NtWaitForAlertByThreadId", "WaitOnAddress", "RtlpWaitOnCriticalSection",
                   "NtWaitForSingleObject", "parking_lot::raw_mutex", "futex", "AcquireSRWLock")),
    ("allocation", ("RtlAllocateHeap", "RtlFreeHeap", "HeapAlloc", "HeapFree", "RtlReAllocateHeap",
                    "__rust_alloc", "__rdl_alloc", "memcpy", "memmove", "memset")),
]


def categorize(names: list[str]) -> str:
    """Cause of one sample: the first category (in CATEGORIES order) found anywhere in its stack.

    The order puts specific causes before generic ones: a texture upload that blocks in the GL
    driver, or a `block_on` wait on the async runtime, shows up as that cause and not as the
    park/lock primitive at the top of the stack.
    """
    for cat, needles in CATEGORIES:
        if any(n in name for name in names for n in needles):
            return cat
    return "app logic (UI thread)"


def app_frame(names: list[str]) -> str:
    """Innermost function from the app's own crates (tg_*) in a stack, innermost-first."""
    for n in names:
        if "startup::profiler" in n:  # the global allocator wrapper, not app logic
            continue
        if n.startswith("tg_app::app::run_app"):
            return "eframe/egui internals (no app frame)"
        if n.startswith("tg_") or "::tg_" in n:
            return n
    return names[0] if names else "?"


def short(name: str, n: int = 110) -> str:
    name = name.replace("::impl$", "::").replace("alloc::", "")
    return name if len(name) <= n else name[: n - 1] + "…"


def analyze_samples(run: dict[str, Any], exe: str, slow_ms: float) -> dict[str, Any] | None:
    run_dir = Path(run["run_dir"])
    trace = run_dir / "trace.tsv"
    stacks = read_stacks(Path(str(trace) + ".stacks"))
    modules = read_modules(Path(str(trace) + ".modules"))
    if not stacks or not modules:
        return None
    sym = Symbolizer(modules, str(Path(exe).parent))
    frames, _, _ = parse_trace(trace)
    sec = section_frames(frames, run["section"])
    if not sec:
        return None
    resolved: list[tuple[int, list[str]]] = []
    for t_ns, addrs in stacks:
        names: list[str] = []
        for j, a in enumerate(addrs):
            names.extend(sym.frames(a if j == 0 else a - 1))
        resolved.append((t_ns // 1000, names))

    def window_samples(f: dict[str, Any]) -> list[list[str]]:
        lo = f["start_us"] - 1500
        hi = f["start_us"] + (f["cpu_us"] or 0)
        return [n for t, n in resolved if lo <= t <= hi and not any(
            m in x for x in n for m in WAIT_MARKERS)]

    slow = sorted((f for f in sec if f["cpu_us"] / 1000.0 > slow_ms),
                  key=lambda f: -f["cpu_us"])
    agg_cat: dict[str, int] = {}
    agg_app: dict[str, int] = {}
    agg_leaf: dict[str, int] = {}
    worst: list[dict[str, Any]] = []
    total = 0
    for f in slow:
        samples = window_samples(f)
        cats: dict[str, int] = {}
        apps: dict[str, int] = {}
        for names in samples:
            c = categorize(names)
            a = app_frame(names)
            cats[c] = cats.get(c, 0) + 1
            apps[a] = apps.get(a, 0) + 1
            agg_cat[c] = agg_cat.get(c, 0) + 1
            agg_app[a] = agg_app.get(a, 0) + 1
            leaf = names[0] if names else "?"
            agg_leaf[leaf] = agg_leaf.get(leaf, 0) + 1
            total += 1
        if len(worst) < 8:
            worst.append({
                "cpu_ms": round(f["cpu_us"] / 1000, 1),
                "ui_ms": round(f["ui_us"] / 1000, 1),
                "frame": f["i"],
                "samples": len(samples),
                "causes": sorted(cats.items(), key=lambda kv: -kv[1])[:4],
                "app_functions": [(short(k), v) for k, v in
                                  sorted(apps.items(), key=lambda kv: -kv[1])[:5]],
            })
    # Whole-section profile (all frames), for context when no frame is slow.
    all_cat: dict[str, int] = {}
    all_app: dict[str, int] = {}
    t_lo = min(f["start_us"] for f in sec) - 1500
    t_hi = max(f["start_us"] + (f["cpu_us"] or 0) for f in sec)
    for t, names in resolved:
        if t_lo <= t <= t_hi and not any(m in x for x in names for m in WAIT_MARKERS):
            c = categorize(names)
            a = app_frame(names)
            all_cat[c] = all_cat.get(c, 0) + 1
            all_app[a] = all_app.get(a, 0) + 1

    def top(d: dict[str, int], n: int) -> list[tuple[str, int]]:
        return [(short(k), v) for k, v in sorted(d.items(), key=lambda kv: -kv[1])[:n]]

    return {
        "tag": run["tag"],
        "slow_frames": len(slow),
        "slow_samples": total,
        "slow_causes": top(agg_cat, 10),
        "slow_app_functions": top(agg_app, 15),
        "slow_leaf_functions": top(agg_leaf, 12),
        "worst_frames": worst,
        "section_causes": top(all_cat, 10),
        "section_app_functions": top(all_app, 15),
    }


def analyze(out_dir: Path, exe: str | None = None) -> None:
    results: dict[str, Any] = {}
    for manifest_name in ("manifest.json", "manifest_live.json"):
        mpath = out_dir / manifest_name
        if not mpath.exists():
            continue
        manifest = json.loads(mpath.read_text(encoding="utf-8"))
        exe = exe or manifest["exe"]
        by_scen: dict[str, list[dict[str, Any]]] = {}
        for run in manifest["runs"]:
            by_scen.setdefault(("live_" if run["live"] else "") + run["scenario"], []).append(run)
        for scen, runs in by_scen.items():
            timing = [r for r in runs if r["mode"] == "1"]
            pooled: list[float] = []
            ui_pooled: list[float] = []
            per_run = []
            for r in timing:
                frames, events, malformed = parse_trace(Path(r["run_dir"]) / "trace.tsv")
                sec = section_frames(frames, r["section"])
                cpu = [f["cpu_us"] / 1000.0 for f in sec]
                pooled += cpu
                ui_pooled += [f["ui_us"] / 1000.0 for f in sec]
                st = frame_stats(cpu)
                st["section_s"] = round((sec[-1]["start_us"] - sec[0]["start_us"]) / 1e6, 1) if sec else 0
                st["complete"] = any(e[1] == "trace_end" for e in events) and not r["timed_out"]
                st["cpu_before"] = r["machine_before"]["cpu_pct_3s"]
                st["rustc_before"] = r["machine_before"]["rustc"]
                st["free_ram_gib"] = r["machine_before"]["free_ram_gib"]
                st["malformed_lines"] = malformed
                st["mixed"] = trace_mixed(frames, events, malformed)
                per_run.append(st)
            sampled = [analyze_samples(r, exe, BUDGET_60) for r in runs if r["mode"] == "sample"]
            sampled_timing = []
            for r in runs:
                if r["mode"] == "sample":
                    frames, _, _ = parse_trace(Path(r["run_dir"]) / "trace.tsv")
                    sampled_timing.append(frame_stats(
                        [f["cpu_us"] / 1000.0 for f in section_frames(frames, r["section"])]))
            results[scen] = {
                "runs": len(timing),
                "pooled": frame_stats(pooled),
                "ui_pooled": frame_stats(ui_pooled),
                "per_run": per_run,
                "sampled_run_timing": sampled_timing,
                "samples": [s for s in sampled if s],
            }
    (out_dir / "results.json").write_text(json.dumps(results, indent=2, ensure_ascii=False),
                                          encoding="utf-8")
    (out_dir / "report.md").write_text(render_md(results), encoding="utf-8")
    print(render_md(results))


def render_md(results: dict[str, Any]) -> str:
    lines = ["# Frame-time benchmark", "",
             "CPU frame time = eframe's whole-frame CPU time (input, UI, tessellation, GL submit; "
             "vsync wait excluded). Pooled over all timing runs.", "",
             "| scenario | runs | frames | p50 ms | p95 ms | p99 ms | max ms | >16.7 ms | >33 ms | "
             "p95 per run (min-max) |",
             "|---|---|---|---|---|---|---|---|---|---|"]
    for scen, r in results.items():
        p = r["pooled"]
        p95s = [x["p95"] for x in r["per_run"]] or [0]
        lines.append(f"| {scen} | {r['runs']} | {p['frames']} | {p['p50']} | {p['p95']} | {p['p99']} | "
                     f"{p['max']} | {p['over_16_7']} | {p['over_33']} | {min(p95s)}-{max(p95s)} |")
    lines.append("")
    for scen, r in results.items():
        lines.append(f"## {scen}")
        for i, pr in enumerate(r["per_run"]):
            lines.append(f"- run {i}: frames {pr['frames']}, p50 {pr['p50']}, p95 {pr['p95']}, "
                         f"max {pr['max']}, >16.7 {pr['over_16_7']}, >33 {pr['over_33']}, "
                         f"complete {pr['complete']}, CPU before {pr['cpu_before']}%, "
                         f"rustc running {pr['rustc_before']}, free RAM {pr['free_ram_gib']} GiB, "
                         f"malformed lines {pr.get('malformed_lines', 0)}"
                         + (", MIXED: several processes wrote this trace, numbers invalid"
                            if pr.get("mixed") else ""))
        for s in r["samples"]:
            lines.append(f"- sampled run {s['tag']}: {s['slow_frames']} slow frames, "
                         f"{s['slow_samples']} samples in them")
            lines.append(f"  - slow-frame causes: {s['slow_causes']}")
            lines.append(f"  - slow-frame app functions: {s['slow_app_functions'][:8]}")
            lines.append(f"  - whole-section causes: {s['section_causes']}")
            lines.append(f"  - whole-section app functions: {s['section_app_functions'][:8]}")
            for w in s["worst_frames"][:5]:
                lines.append(f"  - worst frame {w['frame']}: {w['cpu_ms']} ms (ui {w['ui_ms']}), "
                             f"{w['causes']}, {w['app_functions'][:3]}")
        lines.append("")
    return "\n".join(lines)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("run", "probe"):
        p = sub.add_parser(name)
        p.add_argument("--exe", required=True)
        p.add_argument("--scratch-dir", required=True)
        p.add_argument("--out-dir", required=True)
        p.add_argument("--runs", type=int, default=5)
        p.add_argument("--sample-runs", type=int, default=1)
        p.add_argument("--scenarios", default="chatlist,history,openchats,panels")
        p.add_argument("--live-workdir", default="")
        p.add_argument("--api-creds-doc", default="")
        p.add_argument("--ffmpeg-bin", default="")
        p.add_argument("--bench-data", default="1")
        p.add_argument("--layout", default="")
        p.add_argument("--startup-wait-ms", type=int, default=5000)
        p.add_argument("--run-timeout", type=float, default=180.0)
        p.add_argument("--probe-script", default="shot {shots}/start.bmp\\nwait 300")
    a = sub.add_parser("analyze")
    a.add_argument("--out-dir", required=True)
    a.add_argument("--exe", default=None)
    args = ap.parse_args()
    if args.cmd == "run":
        cmd_run(args)
    elif args.cmd == "probe":
        cmd_probe(args)
    else:
        analyze(Path(args.out_dir), args.exe)


if __name__ == "__main__":
    main()
