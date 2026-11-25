#!/usr/bin/env python3
"""Shared Windows measurement utilities for Rustgram vs Telegram Desktop benchmarks.

Uses Win32 APIs via ctypes (GetSystemTimes, GetProcessTimes, GetProcessMemoryInfo,
EnumWindows, PrintWindow, BitBlt, SendInput) plus NumPy/Pillow for frame diffing.
Contains no hardcoded machine or local paths.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import os
import re
import shutil
import statistics
import subprocess
import time
import winreg
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image

try:
    from .workdir_safety import (
        ProcessJob,
        ScratchSession,
        WorkdirCleanupError,
        copy_workdir_safely,
        ensure_outside_git_repo,
        get_process_creation_time,
        is_inside_git_repo,
        list_descendant_pids,
        remove_workdir_verified,
        stop_process_by_pid,
    )
except ImportError:
    from workdir_safety import (  # type: ignore[no-redef]
        ProcessJob,
        ScratchSession,
        WorkdirCleanupError,
        copy_workdir_safely,
        ensure_outside_git_repo,
        get_process_creation_time,
        is_inside_git_repo,
        list_descendant_pids,
        remove_workdir_verified,
        stop_process_by_pid,
    )

# ---------------------------------------------------------------------------
# Win32 constants & structs
# ---------------------------------------------------------------------------

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_VM_READ = 0x0010
PROCESS_TERMINATE = 0x0001
SYNCHRONIZE = 0x00100000

WM_CLOSE = 0x0010
GW_OWNER = 4
PW_RENDERFULLCONTENT = 0x00000002
SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0
BI_RGB = 0
SW_RESTORE = 9
SW_SHOW = 5

INPUT_MOUSE = 0
INPUT_KEYBOARD = 1
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_WHEEL = 0x0800
MOUSEEVENTF_ABSOLUTE = 0x8000
MOUSEEVENTF_VIRTUALDESK = 0x4000

KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004

SM_XVIRTUALSCREEN = 76
SM_YVIRTUALSCREEN = 77
SM_CXVIRTUALSCREEN = 78
SM_CYVIRTUALSCREEN = 79

TH32CS_SNAPPROCESS = 0x00000002
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value


class FILETIME(ctypes.Structure):
    _fields_ = [("dwLowDateTime", wt.DWORD), ("dwHighDateTime", wt.DWORD)]

    def to_u64(self) -> int:
        return (self.dwHighDateTime << 32) | self.dwLowDateTime


class PROCESS_MEMORY_COUNTERS_EX(ctypes.Structure):
    _fields_ = [
        ("cb", wt.DWORD),
        ("PageFaultCount", wt.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


class MEMORYSTATUSEX(ctypes.Structure):
    _fields_ = [
        ("dwLength", wt.DWORD),
        ("dwMemoryLoad", wt.DWORD),
        ("ullTotalPhys", ctypes.c_uint64),
        ("ullAvailPhys", ctypes.c_uint64),
        ("ullTotalPageFile", ctypes.c_uint64),
        ("ullAvailPageFile", ctypes.c_uint64),
        ("ullTotalVirtual", ctypes.c_uint64),
        ("ullAvailVirtual", ctypes.c_uint64),
        ("ullAvailExtendedVirtual", ctypes.c_uint64),
    ]


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD),
        ("biWidth", wt.LONG),
        ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD),
        ("biBitCount", wt.WORD),
        ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD),
        ("biXPelsPerMeter", wt.LONG),
        ("biYPelsPerMeter", wt.LONG),
        ("biClrUsed", wt.DWORD),
        ("biClrImportant", wt.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wt.DWORD * 3)]


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", wt.DWORD),
        ("cntUsage", wt.DWORD),
        ("th32ProcessID", wt.DWORD),
        ("th32DefaultHeapID", ctypes.POINTER(ctypes.c_ulong)),
        ("th32ModuleID", wt.DWORD),
        ("cntThreads", wt.DWORD),
        ("th32ParentProcessID", wt.DWORD),
        ("pcPriClassBase", wt.LONG),
        ("dwFlags", wt.DWORD),
        ("szExeFile", wt.WCHAR * 260),
    ]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wt.LONG),
        ("dy", wt.LONG),
        ("mouseData", wt.DWORD),
        ("dwFlags", wt.DWORD),
        ("time", wt.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wt.WORD),
        ("wScan", wt.WORD),
        ("dwFlags", wt.DWORD),
        ("time", wt.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [
        ("uMsg", wt.DWORD),
        ("wParamL", wt.WORD),
        ("wParamH", wt.WORD),
    ]


class INPUT_UNION(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT)]


class INPUT(ctypes.Structure):
    _fields_ = [("type", wt.DWORD), ("u", INPUT_UNION)]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
ntdll = ctypes.WinDLL("ntdll", use_last_error=True)

# Make process DPI-aware so GetWindowRect / PrintWindow / SendInput use real pixels.
try:
    user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PER_MONITOR_AWARE_V2
except Exception:
    try:
        user32.SetProcessDPIAware()
    except Exception:
        pass

kernel32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
kernel32.OpenProcess.restype = wt.HANDLE
kernel32.CloseHandle.argtypes = [wt.HANDLE]
kernel32.CloseHandle.restype = wt.BOOL
kernel32.GetSystemTimes.argtypes = [
    ctypes.POINTER(FILETIME),
    ctypes.POINTER(FILETIME),
    ctypes.POINTER(FILETIME),
]
kernel32.GetSystemTimes.restype = wt.BOOL
kernel32.GetProcessTimes.argtypes = [
    wt.HANDLE,
    ctypes.POINTER(FILETIME),
    ctypes.POINTER(FILETIME),
    ctypes.POINTER(FILETIME),
    ctypes.POINTER(FILETIME),
]
kernel32.GetProcessTimes.restype = wt.BOOL
kernel32.TerminateProcess.argtypes = [wt.HANDLE, wt.UINT]
kernel32.TerminateProcess.restype = wt.BOOL
kernel32.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
kernel32.WaitForSingleObject.restype = wt.DWORD
kernel32.CreateToolhelp32Snapshot.argtypes = [wt.DWORD, wt.DWORD]
kernel32.CreateToolhelp32Snapshot.restype = wt.HANDLE
kernel32.Process32FirstW.argtypes = [wt.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
kernel32.Process32FirstW.restype = wt.BOOL
kernel32.Process32NextW.argtypes = [wt.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
kernel32.Process32NextW.restype = wt.BOOL
kernel32.GlobalMemoryStatusEx.argtypes = [ctypes.POINTER(MEMORYSTATUSEX)]
kernel32.GlobalMemoryStatusEx.restype = wt.BOOL

psapi.GetProcessMemoryInfo.argtypes = [
    wt.HANDLE,
    ctypes.POINTER(PROCESS_MEMORY_COUNTERS_EX),
    wt.DWORD,
]
psapi.GetProcessMemoryInfo.restype = wt.BOOL

WNDENUMPROC = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
user32.EnumWindows.argtypes = [WNDENUMPROC, wt.LPARAM]
user32.EnumWindows.restype = wt.BOOL
user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]
user32.GetWindowThreadProcessId.restype = wt.DWORD
user32.IsWindowVisible.argtypes = [wt.HWND]
user32.IsWindowVisible.restype = wt.BOOL
user32.IsIconic.argtypes = [wt.HWND]
user32.IsIconic.restype = wt.BOOL
user32.GetWindow.argtypes = [wt.HWND, wt.UINT]
user32.GetWindow.restype = wt.HWND
user32.GetWindowRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.GetWindowRect.restype = wt.BOOL
user32.GetClientRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.GetClientRect.restype = wt.BOOL
user32.GetWindowTextW.argtypes = [wt.HWND, wt.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.restype = ctypes.c_int
user32.PrintWindow.argtypes = [wt.HWND, wt.HDC, wt.UINT]
user32.PrintWindow.restype = wt.BOOL
user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.PostMessageW.restype = wt.BOOL
user32.ShowWindow.argtypes = [wt.HWND, ctypes.c_int]
user32.ShowWindow.restype = wt.BOOL
user32.SetForegroundWindow.argtypes = [wt.HWND]
user32.SetForegroundWindow.restype = wt.BOOL
user32.BringWindowToTop.argtypes = [wt.HWND]
user32.BringWindowToTop.restype = wt.BOOL
user32.MoveWindow.argtypes = [wt.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wt.BOOL]
user32.MoveWindow.restype = wt.BOOL
user32.GetDC.argtypes = [wt.HWND]
user32.GetDC.restype = wt.HDC
user32.ReleaseDC.argtypes = [wt.HWND, wt.HDC]
user32.ReleaseDC.restype = ctypes.c_int
user32.GetSystemMetrics.argtypes = [ctypes.c_int]
user32.GetSystemMetrics.restype = ctypes.c_int
user32.SendInput.argtypes = [wt.UINT, ctypes.POINTER(INPUT), ctypes.c_int]
user32.SendInput.restype = wt.UINT
user32.GetForegroundWindow.argtypes = []
user32.GetForegroundWindow.restype = wt.HWND

gdi32.CreateCompatibleDC.argtypes = [wt.HDC]
gdi32.CreateCompatibleDC.restype = wt.HDC
gdi32.DeleteDC.argtypes = [wt.HDC]
gdi32.DeleteDC.restype = wt.BOOL
gdi32.CreateDIBSection.argtypes = [
    wt.HDC,
    ctypes.POINTER(BITMAPINFO),
    wt.UINT,
    ctypes.POINTER(ctypes.c_void_p),
    wt.HANDLE,
    wt.DWORD,
]
gdi32.CreateDIBSection.restype = wt.HBITMAP
gdi32.SelectObject.argtypes = [wt.HDC, wt.HGDIOBJ]
gdi32.SelectObject.restype = wt.HGDIOBJ
gdi32.DeleteObject.argtypes = [wt.HGDIOBJ]
gdi32.DeleteObject.restype = wt.BOOL
gdi32.BitBlt.argtypes = [
    wt.HDC,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    wt.HDC,
    ctypes.c_int,
    ctypes.c_int,
    wt.DWORD,
]
gdi32.BitBlt.restype = wt.BOOL


# ---------------------------------------------------------------------------
# Machine summary (privacy-safe: CPU model, RAM, Windows build only)
# ---------------------------------------------------------------------------


def get_machine_summary() -> dict[str, Any]:
    """Collects non-identifying hardware and OS info (CPU model, RAM, Windows build)."""
    cpu_model = "Unknown"
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        ) as key:
            cpu_model = str(winreg.QueryValueEx(key, "ProcessorNameString")[0]).strip()
    except OSError:
        pass

    logical_cpus = os.cpu_count() or 1

    mem = MEMORYSTATUSEX()
    mem.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
    total_ram_bytes = 0
    if kernel32.GlobalMemoryStatusEx(ctypes.byref(mem)):
        total_ram_bytes = int(mem.ullTotalPhys)

    win_build = ""
    win_display_version = ""
    win_product = "Windows"
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        ) as key:
            cur_build = str(winreg.QueryValueEx(key, "CurrentBuildNumber")[0])
            try:
                ubr = int(winreg.QueryValueEx(key, "UBR")[0])
                win_build = f"{cur_build}.{ubr}"
            except OSError:
                win_build = cur_build
            try:
                win_display_version = str(winreg.QueryValueEx(key, "DisplayVersion")[0])
            except OSError:
                pass
            try:
                pn = str(winreg.QueryValueEx(key, "ProductName")[0])
                # Windows 11 still reports "Windows 10 ..." in registry for builds >= 22000.
                if int(cur_build) >= 22000 and pn.startswith("Windows 10"):
                    pn = pn.replace("Windows 10", "Windows 11", 1)
                win_product = pn
            except (OSError, ValueError):
                pass
    except OSError:
        pass

    return {
        "cpu_model": cpu_model,
        "logical_processors": logical_cpus,
        "ram_bytes": total_ram_bytes,
        "ram_gib": round(total_ram_bytes / (1024**3), 2),
        "os": win_product,
        "windows_display_version": win_display_version,
        "windows_build": win_build,
    }


# ---------------------------------------------------------------------------
# System CPU idle check
# ---------------------------------------------------------------------------


def _read_system_times() -> tuple[int, int, int]:
    idle = FILETIME()
    kernel = FILETIME()
    user = FILETIME()
    if not kernel32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kernel), ctypes.byref(user)):
        raise OSError(f"GetSystemTimes failed: {ctypes.get_last_error()}")
    return idle.to_u64(), kernel.to_u64(), user.to_u64()


def check_system_idle(
    duration_s: float = 30.0,
    max_cpu_pct: float = 15.0,
    interval_s: float = 1.0,
    max_wait_s: float = 180.0,
) -> dict[str, Any]:
    """Wait until a full `duration_s` window has system CPU < `max_cpu_pct`.

    Returns a summary dict with samples, mean, median, max, and whether the gate passed.
    """
    deadline = time.monotonic() + max(max_wait_s, duration_s + 5.0)
    needed = max(1, int(round(duration_s / interval_s)))
    samples: list[float] = []

    prev = _read_system_times()
    while True:
        time.sleep(interval_s)
        cur = _read_system_times()
        d_idle = cur[0] - prev[0]
        d_kernel = cur[1] - prev[1]
        d_user = cur[2] - prev[2]
        prev = cur
        total = d_kernel + d_user
        busy = max(0, total - d_idle)
        pct = (100.0 * busy / total) if total > 0 else 0.0
        samples.append(round(pct, 2))
        if len(samples) > needed:
            samples = samples[-needed:]

        if len(samples) == needed and max(samples) < max_cpu_pct:
            return {
                "passed": True,
                "duration_s": round(needed * interval_s, 1),
                "threshold_pct": max_cpu_pct,
                "mean_pct": round(statistics.mean(samples), 2),
                "median_pct": round(statistics.median(samples), 2),
                "max_pct": round(max(samples), 2),
                "samples_pct": list(samples),
            }
        if time.monotonic() >= deadline:
            return {
                "passed": len(samples) == needed and max(samples) < max_cpu_pct,
                "duration_s": round(len(samples) * interval_s, 1),
                "threshold_pct": max_cpu_pct,
                "mean_pct": round(statistics.mean(samples), 2) if samples else 0.0,
                "median_pct": round(statistics.median(samples), 2) if samples else 0.0,
                "max_pct": round(max(samples), 2) if samples else 0.0,
                "samples_pct": list(samples),
            }


# ---------------------------------------------------------------------------
# Process tree, memory, CPU
# ---------------------------------------------------------------------------


def get_process_memory(pid: int) -> dict[str, int] | None:
    """Return private_bytes, working_set_bytes, peak_working_set_bytes for one PID."""
    h = kernel32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, pid)
    if not h:
        h = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    try:
        pmc = PROCESS_MEMORY_COUNTERS_EX()
        pmc.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS_EX)
        if not psapi.GetProcessMemoryInfo(h, ctypes.byref(pmc), pmc.cb):
            return None
        return {
            "private_bytes": int(pmc.PrivateUsage),
            "working_set_bytes": int(pmc.WorkingSetSize),
            "peak_working_set_bytes": int(pmc.PeakWorkingSetSize),
        }
    finally:
        kernel32.CloseHandle(h)


def get_process_tree_memory(root_pid: int) -> dict[str, Any]:
    """Return main-process memory and process-tree totals (including child helpers if any)."""
    pids = list_descendant_pids(root_pid)
    main_mem = get_process_memory(root_pid) or {
        "private_bytes": 0,
        "working_set_bytes": 0,
        "peak_working_set_bytes": 0,
    }
    tree_priv = 0
    tree_ws = 0
    live_count = 0
    for p in pids:
        m = get_process_memory(p)
        if m is not None:
            tree_priv += m["private_bytes"]
            tree_ws += m["working_set_bytes"]
            live_count += 1
    return {
        "private_bytes": main_mem["private_bytes"],
        "working_set_bytes": main_mem["working_set_bytes"],
        "peak_working_set_bytes": main_mem["peak_working_set_bytes"],
        "tree_private_bytes": tree_priv,
        "tree_working_set_bytes": tree_ws,
        "tree_process_count": live_count,
    }


def get_process_cpu_ticks(pid: int) -> int | None:
    """Return (kernel + user) CPU time in 100-ns ticks for one PID."""
    h = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    try:
        ct, et, kt, ut = FILETIME(), FILETIME(), FILETIME(), FILETIME()
        if not kernel32.GetProcessTimes(
            h, ctypes.byref(ct), ctypes.byref(et), ctypes.byref(kt), ctypes.byref(ut)
        ):
            return None
        return kt.to_u64() + ut.to_u64()
    finally:
        kernel32.CloseHandle(h)


def get_tree_cpu_ticks(root_pid: int) -> int:
    total = 0
    for p in list_descendant_pids(root_pid):
        t = get_process_cpu_ticks(p)
        if t is not None:
            total += t
    return total


def sample_process_cpu_pct(root_pid: int, window_s: float = 5.0) -> dict[str, float]:
    """Measure process-tree CPU % over `window_s` seconds (normalized to total logical CPUs)."""
    ncpu = os.cpu_count() or 1
    t0 = time.perf_counter()
    ticks0 = get_tree_cpu_ticks(root_pid)
    time.sleep(window_s)
    t1 = time.perf_counter()
    ticks1 = get_tree_cpu_ticks(root_pid)
    wall_s = max(1e-6, t1 - t0)
    cpu_s = max(0.0, (ticks1 - ticks0) * 1e-7)
    total_pct = (cpu_s / (wall_s * ncpu)) * 100.0
    one_core_pct = (cpu_s / wall_s) * 100.0
    return {
        "cpu_pct_total": round(total_pct, 3),
        "cpu_pct_single_core": round(one_core_pct, 3),
        "cpu_seconds": round(cpu_s, 4),
        "window_s": round(wall_s, 3),
    }


# ---------------------------------------------------------------------------
# Window discovery & capture
# ---------------------------------------------------------------------------


@dataclass
class WindowInfo:
    hwnd: int
    pid: int
    title: str
    rect: tuple[int, int, int, int]  # left, top, right, bottom

    @property
    def width(self) -> int:
        return self.rect[2] - self.rect[0]

    @property
    def height(self) -> int:
        return self.rect[3] - self.rect[1]


def list_visible_windows_for_pid(pid: int, min_w: int = 200, min_h: int = 150) -> list[WindowInfo]:
    """Return visible top-level windows owned by `pid` meeting minimum dimensions."""
    found: list[WindowInfo] = []

    def _cb(hwnd: int, _lparam: int) -> bool:
        win_pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(win_pid))
        if int(win_pid.value) != pid:
            return True
        if not user32.IsWindowVisible(hwnd):
            return True
        if user32.GetWindow(hwnd, GW_OWNER):
            return True
        rect = wt.RECT()
        if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
            return True
        w = rect.right - rect.left
        h = rect.bottom - rect.top
        if w < min_w or h < min_h:
            return True
        buf = ctypes.create_unicode_buffer(256)
        user32.GetWindowTextW(hwnd, buf, 256)
        found.append(WindowInfo(hwnd=hwnd, pid=pid, title=buf.value, rect=(rect.left, rect.top, rect.right, rect.bottom)))
        return True

    user32.EnumWindows(WNDENUMPROC(_cb), 0)
    found.sort(key=lambda wi: wi.width * wi.height, reverse=True)
    return found


def list_all_windows_for_pid(pid: int) -> list[WindowInfo]:
    """Return all top-level windows (visible or hidden) owned by `pid`."""
    found: list[WindowInfo] = []

    def _cb(hwnd: int, _lparam: int) -> bool:
        win_pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(win_pid))
        if int(win_pid.value) != pid:
            return True
        if user32.GetWindow(hwnd, GW_OWNER):
            return True
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        buf = ctypes.create_unicode_buffer(256)
        user32.GetWindowTextW(hwnd, buf, 256)
        found.append(WindowInfo(hwnd=hwnd, pid=pid, title=buf.value, rect=(rect.left, rect.top, rect.right, rect.bottom)))
        return True

    user32.EnumWindows(WNDENUMPROC(_cb), 0)
    return found


def capture_window_printwindow(hwnd: int) -> np.ndarray | None:
    """Capture a window using PrintWindow(PW_RENDERFULLCONTENT). Returns HxWx3 uint8 RGB array."""
    rect = wt.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        return None
    w = rect.right - rect.left
    h = rect.bottom - rect.top
    if w <= 0 or h <= 0:
        return None

    screen_dc = user32.GetDC(0)
    if not screen_dc:
        return None
    mem_dc = gdi32.CreateCompatibleDC(screen_dc)
    if not mem_dc:
        user32.ReleaseDC(0, screen_dc)
        return None

    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = w
    bmi.bmiHeader.biHeight = -h  # top-down DIB
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = BI_RGB

    bits_ptr = ctypes.c_void_p()
    hbmp = gdi32.CreateDIBSection(
        mem_dc, ctypes.byref(bmi), DIB_RGB_COLORS, ctypes.byref(bits_ptr), None, 0
    )
    if not hbmp or not bits_ptr.value:
        gdi32.DeleteDC(mem_dc)
        user32.ReleaseDC(0, screen_dc)
        return None

    old = gdi32.SelectObject(mem_dc, hbmp)
    try:
        ok = user32.PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT)
        if not ok:
            ok = user32.PrintWindow(hwnd, mem_dc, 0)
        if not ok:
            return None
        buf = (ctypes.c_ubyte * (w * h * 4)).from_address(bits_ptr.value)
        arr = np.frombuffer(buf, dtype=np.uint8).reshape((h, w, 4))
        # BGRA -> RGB copy (detached from DIB memory before DeleteObject).
        return arr[:, :, 2::-1].copy()
    finally:
        gdi32.SelectObject(mem_dc, old)
        gdi32.DeleteObject(hbmp)
        gdi32.DeleteDC(mem_dc)
        user32.ReleaseDC(0, screen_dc)


def capture_screen_rect(left: int, top: int, width: int, height: int) -> np.ndarray | None:
    """Capture a desktop rectangle via BitBlt from screen DC (fast, zero target-window WM_PRINT)."""
    if width <= 0 or height <= 0:
        return None
    screen_dc = user32.GetDC(0)
    if not screen_dc:
        return None
    mem_dc = gdi32.CreateCompatibleDC(screen_dc)
    if not mem_dc:
        user32.ReleaseDC(0, screen_dc)
        return None

    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = width
    bmi.bmiHeader.biHeight = -height
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = BI_RGB

    bits_ptr = ctypes.c_void_p()
    hbmp = gdi32.CreateDIBSection(
        mem_dc, ctypes.byref(bmi), DIB_RGB_COLORS, ctypes.byref(bits_ptr), None, 0
    )
    if not hbmp or not bits_ptr.value:
        gdi32.DeleteDC(mem_dc)
        user32.ReleaseDC(0, screen_dc)
        return None

    old = gdi32.SelectObject(mem_dc, hbmp)
    try:
        if not gdi32.BitBlt(mem_dc, 0, 0, width, height, screen_dc, left, top, SRCCOPY):
            return None
        buf = (ctypes.c_ubyte * (width * height * 4)).from_address(bits_ptr.value)
        arr = np.frombuffer(buf, dtype=np.uint8).reshape((height, width, 4))
        return arr[:, :, 2::-1].copy()
    finally:
        gdi32.SelectObject(mem_dc, old)
        gdi32.DeleteObject(hbmp)
        gdi32.DeleteDC(mem_dc)
        user32.ReleaseDC(0, screen_dc)


def frame_diff_ratio(a: np.ndarray, b: np.ndarray, channel_threshold: int = 8) -> float:
    """Fraction of pixels whose max RGB channel diff exceeds `channel_threshold`."""
    if a.shape != b.shape:
        return 1.0
    diff = np.abs(a.astype(np.int16) - b.astype(np.int16))
    changed = np.any(diff > channel_threshold, axis=2)
    return float(np.mean(changed))


def frame_is_non_blank(frame: np.ndarray) -> bool:
    """True if the inner 80% of the frame is not a single flat/uniform color.

    Cropping 10% margins avoids false positives from DWM black borders around an
    unpainted or non-composited solid-white client surface.
    """
    h, w, _ = frame.shape
    my = max(1, h // 10)
    mx = max(1, w // 10)
    inner = frame[my : h - my, mx : w - mx, :]
    if inner.size == 0:
        inner = frame
    sub = inner[::4, ::4, :]
    std = float(np.std(sub))
    return std > 4.0


def save_png(frame: np.ndarray, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    Image.fromarray(frame, mode="RGB").save(path)


# ---------------------------------------------------------------------------
# Process lifecycle & workdir helpers (re-exported from workdir_safety)
# ---------------------------------------------------------------------------


def read_test_api_credentials(doc_path: Path) -> tuple[str, str]:
    """Read Telegram Desktop's public test API credentials from its docs file at runtime."""
    text = doc_path.read_text(encoding="utf-8", errors="ignore")
    m_id = re.search(r"api_id\*\*:\s*(\d+)", text)
    m_hash = re.search(r"api_hash\*\*:\s*([0-9a-fA-F]+)", text)
    if not m_id or not m_hash:
        raise RuntimeError(f"Could not parse test api_id/api_hash from {doc_path}")
    return m_id.group(1), m_hash.group(1)


# ---------------------------------------------------------------------------
# Window positioning & Win32 input helpers (for Part 2 interactive benchmarks)
# ---------------------------------------------------------------------------


def focus_window(hwnd: int) -> None:
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
    else:
        user32.ShowWindow(hwnd, SW_SHOW)
    user32.BringWindowToTop(hwnd)
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.15)


def move_window(hwnd: int, x: int, y: int, w: int, h: int) -> None:
    user32.MoveWindow(hwnd, x, y, w, h, True)
    time.sleep(0.15)


def _abs_mouse_coords(x: int, y: int) -> tuple[int, int]:
    vx = user32.GetSystemMetrics(SM_XVIRTUALSCREEN)
    vy = user32.GetSystemMetrics(SM_YVIRTUALSCREEN)
    vw = max(1, user32.GetSystemMetrics(SM_CXVIRTUALSCREEN))
    vh = max(1, user32.GetSystemMetrics(SM_CYVIRTUALSCREEN))
    ax = int(round((x - vx) * 65535.0 / (vw - 1 if vw > 1 else 1)))
    ay = int(round((y - vy) * 65535.0 / (vh - 1 if vh > 1 else 1)))
    return ax, ay


def send_mouse_move(x: int, y: int) -> None:
    ax, ay = _abs_mouse_coords(x, y)
    inp = INPUT(
        type=INPUT_MOUSE,
        u=INPUT_UNION(
            mi=MOUSEINPUT(
                dx=ax,
                dy=ay,
                mouseData=0,
                dwFlags=MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time=0,
                dwExtraInfo=0,
            )
        ),
    )
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT))


def send_mouse_click(x: int, y: int) -> float:
    """Move to screen coordinates (x, y) and left-click. Returns perf_counter timestamp of click."""
    ax, ay = _abs_mouse_coords(x, y)
    move_flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
    arr = (INPUT * 3)(
        INPUT(type=INPUT_MOUSE, u=INPUT_UNION(mi=MOUSEINPUT(dx=ax, dy=ay, mouseData=0, dwFlags=move_flags, time=0, dwExtraInfo=0))),
        INPUT(type=INPUT_MOUSE, u=INPUT_UNION(mi=MOUSEINPUT(dx=ax, dy=ay, mouseData=0, dwFlags=move_flags | MOUSEEVENTF_LEFTDOWN, time=0, dwExtraInfo=0))),
        INPUT(type=INPUT_MOUSE, u=INPUT_UNION(mi=MOUSEINPUT(dx=ax, dy=ay, mouseData=0, dwFlags=move_flags | MOUSEEVENTF_LEFTUP, time=0, dwExtraInfo=0))),
    )
    ts = time.perf_counter()
    user32.SendInput(3, arr, ctypes.sizeof(INPUT))
    return ts


def send_mouse_wheel(delta: int) -> None:
    """Send one mouse wheel notch (+120 up, -120 down) at current cursor position."""
    inp = INPUT(
        type=INPUT_MOUSE,
        u=INPUT_UNION(
            mi=MOUSEINPUT(
                dx=0,
                dy=0,
                mouseData=ctypes.c_uint32(delta).value,
                dwFlags=MOUSEEVENTF_WHEEL,
                time=0,
                dwExtraInfo=0,
            )
        ),
    )
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT))


def send_unicode_text(text: str) -> None:
    """Type Unicode text via SendInput KEYEVENTF_UNICODE."""
    inputs: list[INPUT] = []
    for ch in text:
        code = ord(ch)
        inputs.append(
            INPUT(type=INPUT_KEYBOARD, u=INPUT_UNION(ki=KEYBDINPUT(wVk=0, wScan=code, dwFlags=KEYEVENTF_UNICODE, time=0, dwExtraInfo=0)))
        )
        inputs.append(
            INPUT(type=INPUT_KEYBOARD, u=INPUT_UNION(ki=KEYBDINPUT(wVk=0, wScan=code, dwFlags=KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, time=0, dwExtraInfo=0)))
        )
    if inputs:
        arr = (INPUT * len(inputs))(*inputs)
        user32.SendInput(len(inputs), arr, ctypes.sizeof(INPUT))


def send_vk(vk: int) -> float:
    """Tap a virtual key (e.g. VK_RETURN=0x0D, VK_ESCAPE=0x1B). Returns perf_counter timestamp."""
    arr = (INPUT * 2)(
        INPUT(type=INPUT_KEYBOARD, u=INPUT_UNION(ki=KEYBDINPUT(wVk=vk, wScan=0, dwFlags=0, time=0, dwExtraInfo=0))),
        INPUT(type=INPUT_KEYBOARD, u=INPUT_UNION(ki=KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYEVENTF_KEYUP, time=0, dwExtraInfo=0))),
    )
    ts = time.perf_counter()
    user32.SendInput(2, arr, ctypes.sizeof(INPUT))
    return ts


# ---------------------------------------------------------------------------
# Window-message input helpers (work even on non-active/isolated desktops)
# ---------------------------------------------------------------------------

WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
WM_RBUTTONDOWN = 0x0204
WM_RBUTTONUP = 0x0205
WM_MOUSEWHEEL = 0x020A
WM_KEYDOWN = 0x0100
WM_KEYUP = 0x0101
WM_CHAR = 0x0102
WM_ACTIVATE = 0x0006
WM_SETFOCUS = 0x0007


def post_click(hwnd: int, client_x: int, client_y: int) -> float:
    """Deliver a left-click to `hwnd` at client coordinates via back-to-back PostMessageW."""
    lp = ((client_y & 0xFFFF) << 16) | (client_x & 0xFFFF)
    ts = time.perf_counter()
    user32.PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp)
    user32.PostMessageW(hwnd, WM_LBUTTONDOWN, 1, lp)
    user32.PostMessageW(hwnd, WM_LBUTTONUP, 0, lp)
    return ts


def post_right_click(hwnd: int, client_x: int, client_y: int) -> float:
    """Deliver a right-click to `hwnd` at client coordinates via back-to-back PostMessageW."""
    lp = ((client_y & 0xFFFF) << 16) | (client_x & 0xFFFF)
    ts = time.perf_counter()
    user32.PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp)
    user32.PostMessageW(hwnd, WM_RBUTTONDOWN, 2, lp)
    user32.PostMessageW(hwnd, WM_RBUTTONUP, 0, lp)
    return ts


_WHEEL_JIGGLE = [0]


def post_wheel(hwnd: int, client_x: int, client_y: int, delta: int) -> None:
    """Deliver a mouse wheel tick (+120 up, -120 down) to `hwnd` at client coordinates.

    Alternates client_x by 1 px so winit's duplicate-WM_MOUSEMOVE filter never drops the
    hover position after a synthetic WM_MOUSELEAVE on an isolated desktop.
    """
    _WHEEL_JIGGLE[0] ^= 1
    cx = client_x + _WHEEL_JIGGLE[0]
    lp_client = ((client_y & 0xFFFF) << 16) | (cx & 0xFFFF)
    rect = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    sx = rect.left + cx
    sy = rect.top + client_y
    lp_screen = ((sy & 0xFFFF) << 16) | (sx & 0xFFFF)
    wp = (ctypes.c_uint16(delta).value << 16)
    user32.PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp_client)
    user32.PostMessageW(hwnd, WM_MOUSEWHEEL, wp, lp_screen)


user32.VkKeyScanW.argtypes = [wt.WCHAR]
user32.VkKeyScanW.restype = ctypes.c_short
user32.MapVirtualKeyW.argtypes = [wt.UINT, wt.UINT]
user32.MapVirtualKeyW.restype = wt.UINT


def post_unicode_text(hwnd: int, text: str) -> None:
    """Deliver Unicode text to `hwnd` as winit-compatible WM_KEYDOWN + WM_CHAR + WM_KEYUP triplets."""
    for ch in text:
        vk_res = user32.VkKeyScanW(ch)
        vk = (vk_res & 0xFF) if vk_res != -1 else 0xE7  # VK_PACKET fallback
        scan = user32.MapVirtualKeyW(vk, 0) & 0xFF
        lp_down = (scan << 16) | 1
        lp_up = 0xC0000001 | (scan << 16)
        user32.PostMessageW(hwnd, WM_KEYDOWN, vk, lp_down)
        user32.PostMessageW(hwnd, WM_CHAR, ord(ch), lp_down)
        user32.PostMessageW(hwnd, WM_KEYUP, vk, lp_up)


def post_vk(hwnd: int, vk: int) -> float:
    """Deliver a virtual key press+release to `hwnd` via WM_KEYDOWN/WM_KEYUP with real scan code."""
    scan = user32.MapVirtualKeyW(vk, 0) & 0xFF
    lp_down = (scan << 16) | 1
    lp_up = 0xC0000001 | (scan << 16)
    ts = time.perf_counter()
    user32.PostMessageW(hwnd, WM_KEYDOWN, vk, lp_down)
    user32.PostMessageW(hwnd, WM_KEYUP, vk, lp_up)
    return ts


# ---------------------------------------------------------------------------
# Folder / file size measurement & statistics
# ---------------------------------------------------------------------------


def measure_install_size(exe_path: Path, folder_path: Path, ignore_zero_logs: bool = True) -> dict[str, Any]:
    """Measure main executable size and total installed folder size with file breakdown."""
    exe_bytes = exe_path.stat().st_size
    files: list[dict[str, Any]] = []
    total_bytes = 0
    for root, _dirs, fnames in os.walk(folder_path):
        for fname in sorted(fnames):
            fp = Path(root) / fname
            sz = fp.stat().st_size
            if ignore_zero_logs and sz == 0 and fname.lower().startswith("log"):
                continue
            rel = str(fp.relative_to(folder_path)).replace("\\", "/")
            files.append({"name": rel, "bytes": sz})
            total_bytes += sz
    files.sort(key=lambda x: x["bytes"], reverse=True)
    return {
        "exe_name": exe_path.name,
        "exe_bytes": exe_bytes,
        "exe_mib": round(exe_bytes / (1024 * 1024), 2),
        "folder_bytes": total_bytes,
        "folder_mib": round(total_bytes / (1024 * 1024), 2),
        "file_count": len(files),
        "files": files,
    }


def summarize_values(vals: list[float]) -> dict[str, Any]:
    if not vals:
        return {"count": 0, "median": None, "min": None, "max": None, "mean": None, "raw": []}
    return {
        "count": len(vals),
        "median": round(statistics.median(vals), 3),
        "min": round(min(vals), 3),
        "max": round(max(vals), 3),
        "mean": round(statistics.mean(vals), 3),
        "raw": [round(v, 3) for v in vals],
    }
