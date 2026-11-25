"""Win32 window capture and input helper for UI parity comparisons.

Pure stdlib (ctypes) + Pillow; works under `python -I`.
No hardcoded paths: configure targets via CLI arguments or environment variables.
"""

from __future__ import annotations

import argparse
import ctypes
import os
import sys
import time
from ctypes import wintypes
from dataclasses import dataclass
from typing import Callable, Optional

from PIL import Image

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
dwmapi = ctypes.WinDLL("dwmapi", use_last_error=True)

# Enable per-monitor DPI awareness v2 so rects match physical pixels.
try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)
except Exception:
    pass

# Win32 types & prototypes
DESKTOP_ALL = 0x0001 | 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x0020 | 0x0040 | 0x0080 | 0x0100
EnumWindowsProc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

user32.OpenDesktopW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
user32.OpenDesktopW.restype = wintypes.HANDLE
user32.SetThreadDesktop.argtypes = [wintypes.HANDLE]
user32.SetThreadDesktop.restype = wintypes.BOOL
user32.CloseDesktop.argtypes = [wintypes.HANDLE]
user32.CloseDesktop.restype = wintypes.BOOL
user32.EnumWindows.argtypes = [EnumWindowsProc, wintypes.LPARAM]
user32.EnumWindows.restype = wintypes.BOOL
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowRect.restype = wintypes.BOOL
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetClientRect.restype = wintypes.BOOL
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
user32.ClientToScreen.restype = wintypes.BOOL
user32.SetWindowPos.argtypes = [
    wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
    ctypes.c_int, ctypes.c_int, wintypes.UINT,
]
user32.SetWindowPos.restype = wintypes.BOOL
user32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
user32.ShowWindow.restype = wintypes.BOOL
user32.SetForegroundWindow.argtypes = [wintypes.HWND]
user32.SetForegroundWindow.restype = wintypes.BOOL
user32.BringWindowToTop.argtypes = [wintypes.HWND]
user32.BringWindowToTop.restype = wintypes.BOOL
user32.PrintWindow.argtypes = [wintypes.HWND, wintypes.HDC, wintypes.UINT]
user32.PrintWindow.restype = wintypes.BOOL
user32.GetWindowDC.argtypes = [wintypes.HWND]
user32.GetWindowDC.restype = wintypes.HDC
user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
user32.ReleaseDC.restype = ctypes.c_int
user32.SetCursorPos.argtypes = [ctypes.c_int, ctypes.c_int]
user32.SetCursorPos.restype = wintypes.BOOL
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.restype = ctypes.c_int
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetClassNameW.restype = ctypes.c_int
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.IsWindowVisible.restype = wintypes.BOOL
user32.IsIconic.argtypes = [wintypes.HWND]
user32.IsIconic.restype = wintypes.BOOL
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.PostMessageW.restype = wintypes.BOOL
user32.SendMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.SendMessageW.restype = ctypes.c_ssize_t
user32.MapVirtualKeyW.argtypes = [wintypes.UINT, wintypes.UINT]
user32.MapVirtualKeyW.restype = wintypes.UINT
user32.VkKeyScanW.argtypes = [wintypes.WCHAR]
user32.VkKeyScanW.restype = ctypes.c_short
user32.GetSystemMetrics.argtypes = [ctypes.c_int]
user32.GetSystemMetrics.restype = ctypes.c_int

kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.CloseHandle.restype = wintypes.BOOL
kernel32.QueryFullProcessImageNameW.argtypes = [
    wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)
]
kernel32.QueryFullProcessImageNameW.restype = wintypes.BOOL

gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
gdi32.CreateCompatibleDC.restype = wintypes.HDC
gdi32.CreateCompatibleBitmap.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int]
gdi32.CreateCompatibleBitmap.restype = wintypes.HBITMAP
gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
gdi32.SelectObject.restype = wintypes.HGDIOBJ
gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]
gdi32.DeleteObject.restype = wintypes.BOOL
gdi32.DeleteDC.argtypes = [wintypes.HDC]
gdi32.DeleteDC.restype = wintypes.BOOL
gdi32.BitBlt.argtypes = [
    wintypes.HDC, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
    wintypes.HDC, ctypes.c_int, ctypes.c_int, wintypes.DWORD,
]
gdi32.BitBlt.restype = wintypes.BOOL

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
SW_RESTORE = 9
SW_SHOW = 5
SWP_NOZORDER = 0x0004
SWP_NOACTIVATE = 0x0010
SWP_SHOWWINDOW = 0x0040
PW_RENDERFULLCONTENT = 0x00000002
PW_CLIENTONLY = 0x00000001
SRCCOPY = 0x00CC0020

INPUT_MOUSE = 0
INPUT_KEYBOARD = 1
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010
MOUSEEVENTF_WHEEL = 0x0800
MOUSEEVENTF_ABSOLUTE = 0x8000
MOUSEEVENTF_VIRTUALDESK = 0x4000
KEYEVENTF_EXTENDEDKEY = 0x0001
KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004

SM_XVIRTUALSCREEN = 76
SM_YVIRTUALSCREEN = 77
SM_CXVIRTUALSCREEN = 78
SM_CYVIRTUALSCREEN = 79

VK_MAP = {
    "BACK": 0x08,
    "TAB": 0x09,
    "ENTER": 0x0D,
    "RETURN": 0x0D,
    "SHIFT": 0x10,
    "CTRL": 0x11,
    "ALT": 0x12,
    "ESC": 0x1B,
    "ESCAPE": 0x1B,
    "SPACE": 0x20,
    "PGUP": 0x21,
    "PGDN": 0x22,
    "END": 0x23,
    "HOME": 0x24,
    "LEFT": 0x25,
    "UP": 0x26,
    "RIGHT": 0x27,
    "DOWN": 0x28,
    "DEL": 0x2E,
    "DELETE": 0x2E,
    "F1": 0x70,
    "F2": 0x71,
    "F3": 0x72,
    "F4": 0x73,
    "F5": 0x74,
    "F11": 0x7A,
    "COMMA": 0xBC,
    "PERIOD": 0xBE,
    "SLASH": 0xBF,
}


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [
        ("uMsg", wintypes.DWORD),
        ("wParamL", wintypes.WORD),
        ("wParamH", wintypes.WORD),
    ]


class _INPUT_UNION(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT)]


class INPUT(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [("type", wintypes.DWORD), ("u", _INPUT_UNION)]


user32.SendInput.argtypes = [wintypes.UINT, ctypes.POINTER(INPUT), ctypes.c_int]
user32.SendInput.restype = wintypes.UINT
gdi32.GetDIBits.argtypes = [
    wintypes.HDC, wintypes.HBITMAP, wintypes.UINT, wintypes.UINT,
    ctypes.c_void_p, ctypes.POINTER(BITMAPINFOHEADER), wintypes.UINT,
]
gdi32.GetDIBits.restype = ctypes.c_int


@dataclass
class WindowInfo:
    hwnd: int
    pid: int
    exe_path: str
    class_name: str
    title: str
    visible: bool
    window_rect: tuple[int, int, int, int]
    client_size: tuple[int, int]


def attach_desktop(name: str = "Default") -> int:
    """Attach the calling thread to a WinSta0 desktop (e.g. 'Default')."""
    hdesk = user32.OpenDesktopW(name, 0, False, DESKTOP_ALL)
    if not hdesk:
        raise OSError(f"OpenDesktopW({name!r}) failed: err={ctypes.get_last_error()}")
    if not user32.SetThreadDesktop(hdesk):
        err = ctypes.get_last_error()
        user32.CloseDesktop(hdesk)
        raise OSError(f"SetThreadDesktop({name!r}) failed: err={err}")
    return hdesk


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("lpReserved", wintypes.LPWSTR),
        ("lpDesktop", wintypes.LPWSTR),
        ("lpTitle", wintypes.LPWSTR),
        ("dwX", wintypes.DWORD),
        ("dwY", wintypes.DWORD),
        ("dwXSize", wintypes.DWORD),
        ("dwYSize", wintypes.DWORD),
        ("dwXCountChars", wintypes.DWORD),
        ("dwYCountChars", wintypes.DWORD),
        ("dwFillAttribute", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("wShowWindow", wintypes.WORD),
        ("cbReserved2", wintypes.WORD),
        ("lpReserved2", ctypes.POINTER(wintypes.BYTE)),
        ("hStdInput", wintypes.HANDLE),
        ("hStdOutput", wintypes.HANDLE),
        ("hStdError", wintypes.HANDLE),
    ]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("hProcess", wintypes.HANDLE),
        ("hThread", wintypes.HANDLE),
        ("dwProcessId", wintypes.DWORD),
        ("dwThreadId", wintypes.DWORD),
    ]


def launch_on_desktop(
    exe_path: str,
    args: list[str],
    env: dict[str, str],
    desktop: str = r"WinSta0\Default",
) -> int:
    """Spawn a Windows GUI process on the specified desktop and return its PID."""
    import subprocess

    cmdline = subprocess.list2cmdline([exe_path, *args])
    env_block = "\0".join(f"{k}={v}" for k, v in env.items()) + "\0\0"
    env_buf = ctypes.create_unicode_buffer(env_block)
    si = STARTUPINFOW()
    si.cb = ctypes.sizeof(STARTUPINFOW)
    if desktop:
        si.lpDesktop = desktop
    pi = PROCESS_INFORMATION()
    CREATE_UNICODE_ENVIRONMENT = 0x00000400
    ok = kernel32.CreateProcessW(
        None,
        ctypes.create_unicode_buffer(cmdline),
        None,
        None,
        False,
        CREATE_UNICODE_ENVIRONMENT,
        env_buf,
        None,
        ctypes.byref(si),
        ctypes.byref(pi),
    )
    if not ok:
        raise OSError(f"CreateProcessW({exe_path!r}) failed: err={ctypes.get_last_error()}")
    pid = int(pi.dwProcessId)
    kernel32.CloseHandle(pi.hProcess)
    kernel32.CloseHandle(pi.hThread)
    return pid


def kill_pid(pid: int) -> None:
    """Terminate a process strictly by PID."""
    import subprocess

    subprocess.run(
        ["taskkill", "/PID", str(pid), "/F"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )


def get_process_exe(pid: int) -> str:
    h = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return ""
    try:
        size = wintypes.DWORD(1024)
        buf = ctypes.create_unicode_buffer(size.value)
        if kernel32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
            return buf.value
        return ""
    finally:
        kernel32.CloseHandle(h)


def enum_windows(filter_fn: Optional[Callable[[WindowInfo], bool]] = None) -> list[WindowInfo]:
    out: list[WindowInfo] = []

    def cb(hwnd: int, _lparam: int) -> bool:
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        cls = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, cls, 256)
        title = ctypes.create_unicode_buffer(512)
        user32.GetWindowTextW(hwnd, title, 512)
        wrect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(wrect))
        crect = wintypes.RECT()
        user32.GetClientRect(hwnd, ctypes.byref(crect))
        vis = bool(user32.IsWindowVisible(hwnd))
        info = WindowInfo(
            hwnd=hwnd,
            pid=pid.value,
            exe_path="",
            class_name=cls.value,
            title=title.value,
            visible=vis,
            window_rect=(wrect.left, wrect.top, wrect.right, wrect.bottom),
            client_size=(crect.right - crect.left, crect.bottom - crect.top),
        )
        if filter_fn is None or filter_fn(info):
            out.append(info)
        return True

    user32.EnumWindows(EnumWindowsProc(cb), 0)
    return out


def find_window_by_exe(exe_path_substr: str, pid: Optional[int] = None) -> Optional[WindowInfo]:
    needle = os.path.normcase(os.path.normpath(exe_path_substr)) if exe_path_substr else ""
    candidates: list[WindowInfo] = []
    for w in enum_windows(lambda info: info.visible and (pid is None or info.pid == pid)):
        if w.client_size[0] < 100 or w.client_size[1] < 100:
            continue
        exe = get_process_exe(w.pid)
        w.exe_path = exe
        if not needle or needle in os.path.normcase(os.path.normpath(exe)):
            candidates.append(w)
    if not candidates:
        return None
    # Prefer the largest visible top-level window for that exe.
    candidates.sort(key=lambda w: w.client_size[0] * w.client_size[1], reverse=True)
    return candidates[0]


def get_client_origin_and_size(hwnd: int) -> tuple[int, int, int, int]:
    pt = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    crect = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(crect))
    return pt.x, pt.y, crect.right - crect.left, crect.bottom - crect.top


def focus_window(hwnd: int) -> None:
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
    else:
        user32.ShowWindow(hwnd, SW_SHOW)
    # Alt key trick to allow SetForegroundWindow cleanly if needed
    user32.BringWindowToTop(hwnd)
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.12)


def resize_client_area(
    hwnd: int,
    client_w: int,
    client_h: int,
    x: Optional[int] = None,
    y: Optional[int] = None,
) -> tuple[int, int]:
    """Resize window so its client area is exactly (client_w, client_h)."""
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
        time.sleep(0.15)
    for _ in range(3):
        wrect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(wrect))
        crect = wintypes.RECT()
        user32.GetClientRect(hwnd, ctypes.byref(crect))
        frame_w = (wrect.right - wrect.left) - (crect.right - crect.left)
        frame_h = (wrect.bottom - wrect.top) - (crect.bottom - crect.top)
        target_x = wrect.left if x is None else x
        target_y = wrect.top if y is None else y
        user32.SetWindowPos(
            hwnd,
            0,
            target_x,
            target_y,
            client_w + frame_w,
            client_h + frame_h,
            SWP_NOZORDER | SWP_SHOWWINDOW,
        )
        time.sleep(0.18)
        crect2 = wintypes.RECT()
        user32.GetClientRect(hwnd, ctypes.byref(crect2))
        cw, ch = crect2.right - crect2.left, crect2.bottom - crect2.top
        if cw == client_w and ch == client_h:
            return cw, ch
    crect2 = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(crect2))
    return crect2.right - crect2.left, crect2.bottom - crect2.top


def capture_window_client(hwnd: int, out_path: str, method: str = "printwindow") -> Image.Image:
    """Capture the client area of `hwnd` and save as PNG."""
    wrect = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(wrect))
    cx, cy, cw, ch = get_client_origin_and_size(hwnd)
    if cw <= 0 or ch <= 0:
        raise RuntimeError(f"Invalid client size {cw}x{ch} for hwnd={hwnd}")

    if method == "screen":
        from PIL import ImageGrab

        img = ImageGrab.grab(bbox=(cx, cy, cx + cw, cy + ch), all_screens=True)
    else:
        ww = wrect.right - wrect.left
        wh = wrect.bottom - wrect.top
        hwnd_dc = user32.GetWindowDC(hwnd)
        mfc_dc = gdi32.CreateCompatibleDC(hwnd_dc)
        bmp = gdi32.CreateCompatibleBitmap(hwnd_dc, ww, wh)
        old = gdi32.SelectObject(mfc_dc, bmp)
        ok = user32.PrintWindow(hwnd, mfc_dc, PW_RENDERFULLCONTENT)
        if not ok:
            user32.PrintWindow(hwnd, mfc_dc, 0)
        bmi = BITMAPINFOHEADER()
        bmi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bmi.biWidth = ww
        bmi.biHeight = -wh
        bmi.biPlanes = 1
        bmi.biBitCount = 32
        bmi.biCompression = 0
        buf = ctypes.create_string_buffer(ww * wh * 4)
        gdi32.GetDIBits(mfc_dc, bmp, 0, wh, buf, ctypes.byref(bmi), 0)
        gdi32.SelectObject(mfc_dc, old)
        gdi32.DeleteObject(bmp)
        gdi32.DeleteDC(mfc_dc)
        user32.ReleaseDC(hwnd, hwnd_dc)
        full = Image.frombuffer("RGBA", (ww, wh), buf, "raw", "BGRA", 0, 1).convert("RGB")
        ox = max(0, cx - wrect.left)
        oy = max(0, cy - wrect.top)
        img = full.crop((ox, oy, min(ww, ox + cw), min(wh, oy + ch)))

    os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
    img.save(out_path)
    return img


def _screen_to_abs(sx: int, sy: int) -> tuple[int, int]:
    vx = user32.GetSystemMetrics(SM_XVIRTUALSCREEN)
    vy = user32.GetSystemMetrics(SM_YVIRTUALSCREEN)
    vw = user32.GetSystemMetrics(SM_CXVIRTUALSCREEN)
    vh = user32.GetSystemMetrics(SM_CYVIRTUALSCREEN)
    ax = int(round((sx - vx) * 65535.0 / max(1, vw - 1)))
    ay = int(round((sy - vy) * 65535.0 / max(1, vh - 1)))
    return ax, ay


def move_client(hwnd: int, rel_x: int, rel_y: int) -> None:
    cx, cy, _, _ = get_client_origin_and_size(hwnd)
    sx, sy = cx + rel_x, cy + rel_y
    ax, ay = _screen_to_abs(sx, sy)
    inp = INPUT(type=INPUT_MOUSE)
    inp.mi = MOUSEINPUT(
        dx=ax, dy=ay, mouseData=0,
        dwFlags=MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        time=0, dwExtraInfo=0,
    )
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT))
    user32.SetCursorPos(sx, sy)


def click_client(hwnd: int, rel_x: int, rel_y: int, button: str = "left", double: bool = False) -> None:
    move_client(hwnd, rel_x, rel_y)
    time.sleep(0.05)
    down_flag = MOUSEEVENTF_LEFTDOWN if button == "left" else MOUSEEVENTF_RIGHTDOWN
    up_flag = MOUSEEVENTF_LEFTUP if button == "left" else MOUSEEVENTF_RIGHTUP
    clicks = 2 if double else 1
    for _ in range(clicks):
        inps = (INPUT * 2)()
        inps[0].type = INPUT_MOUSE
        inps[0].mi = MOUSEINPUT(0, 0, 0, down_flag, 0, 0)
        inps[1].type = INPUT_MOUSE
        inps[1].mi = MOUSEINPUT(0, 0, 0, up_flag, 0, 0)
        user32.SendInput(2, inps, ctypes.sizeof(INPUT))
        time.sleep(0.06)


def scroll_client(hwnd: int, rel_x: int, rel_y: int, delta: int) -> None:
    move_client(hwnd, rel_x, rel_y)
    time.sleep(0.04)
    inp = INPUT(type=INPUT_MOUSE)
    inp.mi = MOUSEINPUT(0, 0, ctypes.c_uint32(delta).value, MOUSEEVENTF_WHEEL, 0, 0)
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT))


def _vk_from_name(name: str) -> int:
    up = name.upper()
    if up in VK_MAP:
        return VK_MAP[up]
    if len(up) == 1:
        vk = user32.VkKeyScanW(up) & 0xFF
        if vk != 0xFF:
            return vk
        return ord(up)
    raise ValueError(f"Unknown key name: {name}")


def send_hotkey(*keys: str) -> None:
    """Press and release a chord like ('CTRL', 'F') or ('ESC',)."""
    vks = [_vk_from_name(k) for k in keys]
    inps = (INPUT * (len(vks) * 2))()
    idx = 0
    for vk in vks:
        inps[idx].type = INPUT_KEYBOARD
        inps[idx].ki = KEYBDINPUT(vk, user32.MapVirtualKeyW(vk, 0), 0, 0, 0)
        idx += 1
    for vk in reversed(vks):
        inps[idx].type = INPUT_KEYBOARD
        inps[idx].ki = KEYBDINPUT(vk, user32.MapVirtualKeyW(vk, 0), KEYEVENTF_KEYUP, 0, 0)
        idx += 1
    user32.SendInput(len(inps), inps, ctypes.sizeof(INPUT))
    time.sleep(0.08)


def send_text(text: str) -> None:
    """Type unicode text via SendInput KEYEVENTF_UNICODE."""
    if not text:
        return
    inps = (INPUT * (len(text) * 2))()
    idx = 0
    for ch in text:
        code = ord(ch)
        inps[idx].type = INPUT_KEYBOARD
        inps[idx].ki = KEYBDINPUT(0, code, KEYEVENTF_UNICODE, 0, 0)
        idx += 1
        inps[idx].type = INPUT_KEYBOARD
        inps[idx].ki = KEYBDINPUT(0, code, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, 0, 0)
        idx += 1
    user32.SendInput(len(inps), inps, ctypes.sizeof(INPUT))
    time.sleep(0.08)


def main() -> int:
    ap = argparse.ArgumentParser(description="Win32 capture/list helper for UI parity.")
    ap.add_argument("--desktop", default="Default", help="WinSta0 desktop name (or 'current')")
    ap.add_argument("--exe", default="", help="Substring of executable path to match")
    ap.add_argument("--pid", type=int, default=None, help="Filter by PID")
    ap.add_argument("--resize", default="", help="Resize client area WxH (e.g. 1440x900)")
    ap.add_argument("--pos", default="", help="Window top-left X,Y (e.g. -1860,60)")
    ap.add_argument("--capture", default="", help="Output PNG path")
    ap.add_argument("--method", choices=["printwindow", "screen"], default="printwindow")
    ap.add_argument("--list", action="store_true", help="List visible windows")
    args = ap.parse_args()

    hdesk = None
    if args.desktop and args.desktop.lower() != "current":
        hdesk = attach_desktop(args.desktop)
    try:
        if args.list:
            for w in enum_windows(lambda i: i.visible):
                exe = get_process_exe(w.pid)
                print(f"hwnd={w.hwnd} pid={w.pid} cls={w.class_name!r} title={w.title!r} "
                      f"client={w.client_size} rect={w.window_rect} exe={exe}")
            return 0
        win = find_window_by_exe(args.exe, pid=args.pid)
        if not win:
            print(f"No matching visible window for exe={args.exe!r} pid={args.pid}", file=sys.stderr)
            return 1
        print(f"Found hwnd={win.hwnd} pid={win.pid} title={win.title!r} "
              f"client={win.client_size} rect={win.window_rect}")
        if args.resize:
            w_s, h_s = args.resize.lower().split("x")
            pos_x = pos_y = None
            if args.pos:
                px, py = args.pos.split(",")
                pos_x, pos_y = int(px), int(py)
            cw, ch = resize_client_area(win.hwnd, int(w_s), int(h_s), pos_x, pos_y)
            print(f"Resized client area to {cw}x{ch}")
        if args.capture:
            img = capture_window_client(win.hwnd, args.capture, method=args.method)
            print(f"Captured {img.size} -> {args.capture}")
        return 0
    finally:
        if hdesk:
            user32.CloseDesktop(hdesk)


if __name__ == "__main__":
    raise SystemExit(main())
