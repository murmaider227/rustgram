"""Shared pieces of the scenario walk: clients, the B-side peer and OCR.

Every client runs from a scratch COPY of a test-server working directory and is driven only
through its own window handle (PostMessage input, PrintWindow captures), so the user's mouse,
keyboard and other windows are never touched. Processes are stopped only by the PIDs this
module started.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "bench"))
import win_metrics as wm  # noqa: E402
import workdir_safety as ws  # noqa: E402

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetWindowPos.argtypes = [wt.HWND, wt.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wt.UINT]
user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.IsWindowVisible.argtypes = [wt.HWND]
user32.IsIconic.argtypes = [wt.HWND]
user32.GetClientRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.ClientToScreen.argtypes = [wt.HWND, ctypes.POINTER(wt.POINT)]
user32.GetWindowRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.ShowWindow.argtypes = [wt.HWND, ctypes.c_int]

WM_MOUSEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP = 0x0200, 0x0201, 0x0202
WM_RBUTTONDOWN, WM_RBUTTONUP, WM_MOUSEWHEEL = 0x0204, 0x0205, 0x020A
WM_KEYDOWN, WM_KEYUP, WM_CHAR, WM_CLOSE = 0x0100, 0x0101, 0x0102, 0x0010
SWP_NOZORDER, SWP_NOACTIVATE = 0x0004, 0x0010

VK = {
    "enter": 0x0D, "escape": 0x1B, "esc": 0x1B, "tab": 0x09, "backspace": 0x08, "delete": 0x2E,
    "up": 0x26, "down": 0x28, "left": 0x25, "right": 0x27, "pageup": 0x21, "pagedown": 0x22,
    "home": 0x24, "end": 0x23, "ctrl": 0x11, "shift": 0x10, "alt": 0x12, "space": 0x20,
}


def lparam(x: int, y: int) -> int:
    return ((y & 0xFFFF) << 16) | (x & 0xFFFF)


@dataclass
class Client:
    """One running client (official Telegram Desktop or Rustgram) on a scratch workdir."""

    name: str  # "official" | "rustgram"
    exe: Path
    workdir: Path
    pos: tuple[int, int]
    size: tuple[int, int]
    env: dict[str, str] | None = None
    args: list[str] = field(default_factory=list)
    log: Path | None = None
    proc: subprocess.Popen[bytes] | None = None
    hwnd: int = 0

    @property
    def pid(self) -> int:
        return self.proc.pid if self.proc else 0

    def start(self, timeout_s: float = 60.0) -> None:
        cmd = [str(self.exe), *self.args, "-workdir", str(self.workdir)]
        out = open(self.log, "ab") if self.log else subprocess.DEVNULL  # noqa: SIM115
        self.proc = subprocess.Popen(cmd, cwd=str(self.exe.parent), env=self.env,
                                     stdout=out, stderr=out)
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"{self.name} exited with code {self.proc.returncode}")
            if self.find_window():
                self.place()
                return
            time.sleep(0.3)
        raise RuntimeError(f"{self.name}: no window after {timeout_s:.0f} s")

    def alive(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def find_window(self) -> int:
        wins = wm.list_visible_windows_for_pid(self.pid, min_w=300, min_h=200)
        self.hwnd = wins[0].hwnd if wins else 0
        return self.hwnd

    def window_visible(self) -> bool:
        return bool(self.find_window()) and not user32.IsIconic(self.hwnd)

    def place(self, size: tuple[int, int] | None = None) -> None:
        """Moves the window and sets its CLIENT area to `size` (no activation, no z-order change)."""
        if size:
            self.size = size
        if not self.hwnd:
            return
        w, h = self.size
        for _ in range(3):
            user32.SetWindowPos(self.hwnd, None, self.pos[0], self.pos[1], w, h, SWP_NOZORDER | SWP_NOACTIVATE)
            time.sleep(0.15)
            cw, ch = self.client_size()
            if (cw, ch) == (w, h):
                break
            wr = wt.RECT()
            user32.GetWindowRect(self.hwnd, ctypes.byref(wr))
            fw = (wr.right - wr.left) - cw
            fh = (wr.bottom - wr.top) - ch
            user32.SetWindowPos(self.hwnd, None, self.pos[0], self.pos[1], w + fw, h + fh,
                                SWP_NOZORDER | SWP_NOACTIVATE)
            time.sleep(0.15)
            if self.client_size() == (w, h):
                break

    def client_size(self) -> tuple[int, int]:
        r = wt.RECT()
        user32.GetClientRect(self.hwnd, ctypes.byref(r))
        return r.right - r.left, r.bottom - r.top

    def shot(self, path: Path | None = None) -> Image.Image:
        """Client-area capture via PrintWindow (works while the window is covered)."""
        frame = wm.capture_window_printwindow(self.hwnd)
        if frame is None:
            raise RuntimeError(f"{self.name}: capture failed")
        wr = wt.RECT()
        user32.GetWindowRect(self.hwnd, ctypes.byref(wr))
        pt = wt.POINT(0, 0)
        user32.ClientToScreen(self.hwnd, ctypes.byref(pt))
        cw, ch = self.client_size()
        ox, oy = pt.x - wr.left, pt.y - wr.top
        img = Image.fromarray(frame[oy:oy + ch, ox:ox + cw])
        if path:
            path.parent.mkdir(parents=True, exist_ok=True)
            img.save(path)
        return img

    # -- input (posted to this window only) ----------------------------------------------------
    def move(self, x: int, y: int) -> None:
        user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 0, lparam(x, y))

    def click(self, x: int, y: int, right: bool = False) -> None:
        lp = lparam(x, y)
        down, up, btn = (WM_RBUTTONDOWN, WM_RBUTTONUP, 2) if right else (WM_LBUTTONDOWN, WM_LBUTTONUP, 1)
        # Back to back: a pause lets Windows deliver WM_MOUSELEAVE (the real cursor is elsewhere)
        # and the press then lands on a window that thinks the pointer has left.
        user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 0, lp)
        user32.PostMessageW(self.hwnd, down, btn, lp)
        user32.PostMessageW(self.hwnd, up, 0, lp)

    def drag(self, x0: int, y0: int, x1: int, y1: int, steps: int = 12) -> None:
        """Left-button drag with posted messages (button state in wParam)."""
        user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 0, lparam(x0, y0))
        user32.PostMessageW(self.hwnd, WM_LBUTTONDOWN, 1, lparam(x0, y0))
        for i in range(1, steps + 1):
            x = x0 + (x1 - x0) * i // steps
            y = y0 + (y1 - y0) * i // steps
            user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 1, lparam(x, y))
            time.sleep(0.02)
        user32.PostMessageW(self.hwnd, WM_LBUTTONUP, 0, lparam(x1, y1))

    def hold(self, x: int, y: int, secs: float) -> None:
        lp = lparam(x, y)
        user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 0, lp)
        user32.PostMessageW(self.hwnd, WM_LBUTTONDOWN, 1, lp)
        end = time.time() + secs
        k = 0
        while time.time() < end:
            # Keep the pointer "inside": a fresh move every 100 ms beats the synthetic leave.
            k ^= 1
            user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 1, lparam(x + k, y))
            time.sleep(0.1)
        user32.PostMessageW(self.hwnd, WM_LBUTTONUP, 0, lp)

    def wheel(self, x: int, y: int, notches: int) -> None:
        pt = wt.POINT(x, y)
        user32.ClientToScreen(self.hwnd, ctypes.byref(pt))
        step = 120 if notches > 0 else -120
        for i in range(abs(notches)):
            # Move and wheel back to back, 1 px apart each time (winit drops repeated moves).
            jx = x + (i & 1)
            user32.PostMessageW(self.hwnd, WM_MOUSEMOVE, 0, lparam(jx, y))
            user32.PostMessageW(self.hwnd, WM_MOUSEWHEEL, (ctypes.c_uint16(step).value << 16),
                                lparam(pt.x + (i & 1), pt.y))
            time.sleep(0.05)

    def key(self, name: str) -> None:
        """Presses a key or a chord like `ctrl+c` (modifier state via the thread keyboard state)."""
        parts = name.lower().split("+")
        mods = [VK[p] for p in parts[:-1]]
        last = parts[-1]
        vk = VK[last] if last in VK else ord(last.upper())
        with KeyState(self.hwnd, mods):
            for m in mods:
                user32.PostMessageW(self.hwnd, WM_KEYDOWN, m, 1)
            scan = user32.MapVirtualKeyW(vk, 0) & 0xFF
            user32.PostMessageW(self.hwnd, WM_KEYDOWN, vk, (scan << 16) | 1)
            time.sleep(0.05)
            user32.PostMessageW(self.hwnd, WM_KEYUP, vk, 0xC0000001 | (scan << 16))
            for m in reversed(mods):
                user32.PostMessageW(self.hwnd, WM_KEYUP, m, 0xC0000001)
            time.sleep(0.15)

    def type(self, text: str) -> None:
        """Types text as WM_CHAR messages preceded by a VK_PACKET key-down, so the target's own
        keyboard layout cannot add a second (translated) character."""
        for ch in text:
            if self.name == "official":
                # Qt reads a bare WM_CHAR; a VK_PACKET key-down makes it drop the character.
                user32.PostMessageW(self.hwnd, WM_CHAR, ord(ch), 1)
            else:
                # winit only takes WM_CHAR that follows a key-down.
                user32.PostMessageW(self.hwnd, WM_KEYDOWN, 0xE7, 1)
                user32.PostMessageW(self.hwnd, WM_CHAR, ord(ch), 1)
                user32.PostMessageW(self.hwnd, WM_KEYUP, 0xE7, 0xC0000001)
            time.sleep(0.025)

    def close_window(self) -> None:
        user32.PostMessageW(self.hwnd, WM_CLOSE, 0, 0)

    def stop(self) -> None:
        """Quits the client the way a user does (Ctrl+Q; closing the window only hides it to the
        tray), and terminates it only if it does not exit: a hard kill makes Rustgram open with
        its "not closed properly" screen next time."""
        if self.proc and self.proc.poll() is None:
            # Rustgram quits on a posted Ctrl+Q; tdesktop (Qt) ignores posted chords and is
            # stopped by PID (it keeps its data consistent).
            if self.name == "rustgram" and self.find_window():
                self.key("ctrl+q")
                try:
                    self.proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    pass
            if self.proc.poll() is None:
                ws.stop_process_by_pid(self.proc.pid, graceful_timeout_s=5.0)
        self.proc = None


class KeyState:
    """Sets modifier keys as pressed in the target thread's keyboard state while a chord is posted
    (AttachThreadInput + SetKeyboardState), and restores it afterwards."""

    def __init__(self, hwnd: int, mods: list[int]) -> None:
        self.hwnd, self.mods = hwnd, mods
        self.attached = False

    def __enter__(self) -> "KeyState":
        if not self.mods:
            return self
        tid = user32.GetWindowThreadProcessId(self.hwnd, None)
        me = ctypes.windll.kernel32.GetCurrentThreadId()
        self.attached = bool(user32.AttachThreadInput(me, tid, True))
        self.old = (ctypes.c_ubyte * 256)()
        user32.GetKeyboardState(self.old)
        new = (ctypes.c_ubyte * 256)(*self.old)
        for m in self.mods:
            new[m] = 0x80
        user32.SetKeyboardState(new)
        return self

    def __exit__(self, *exc: Any) -> None:
        if not self.mods:
            return
        time.sleep(0.2)
        user32.SetKeyboardState(self.old)
        if self.attached:
            tid = user32.GetWindowThreadProcessId(self.hwnd, None)
            me = ctypes.windll.kernel32.GetCurrentThreadId()
            user32.AttachThreadInput(me, tid, False)


class Peer:
    """The `tg-walk-peer serve` process: acts as account B (or A) through the session layer."""

    def __init__(self, exe: Path, argv: list[str], env: dict[str, str], log: Path) -> None:
        self.log = open(log, "ab")  # noqa: SIM115
        self.proc = subprocess.Popen(
            [str(exe), *argv], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, env=env,
        )
        ready = self._read()
        if not ready.get("ready"):
            raise RuntimeError(f"peer did not start: {ready}")

    def _read(self) -> dict[str, Any]:
        assert self.proc.stdout
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("peer exited (see peer log)")
        return json.loads(line)

    def __call__(self, **cmd: Any) -> dict[str, Any]:
        assert self.proc.stdin
        self.proc.stdin.write((json.dumps(cmd) + "\n").encode())
        self.proc.stdin.flush()
        return self._read()

    def close(self) -> None:
        try:
            self(op="quit")
        except Exception:  # noqa: BLE001
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            ws.stop_process_by_pid(self.proc.pid)
        self.log.close()


@dataclass
class OcrLine:
    text: str
    x: int
    y: int
    w: int
    h: int


_OCR_PROC: subprocess.Popen[str] | None = None
_OCR_SEQ = [0]


def _ocr_server() -> subprocess.Popen[str]:
    global _OCR_PROC
    if _OCR_PROC is None or _OCR_PROC.poll() is not None:
        _OCR_PROC = subprocess.Popen(
            ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(HERE / "ocr.ps1"),
             "-Lang", "en-US", "-Serve"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8")
    return _OCR_PROC


def ocr(images: list[Path], lang: str = "en-US") -> dict[str, list[OcrLine]]:
    """Windows OCR of PNG files (one long-lived PowerShell); small captures are upscaled 2x."""
    result: dict[str, list[OcrLine]] = {}
    for p in images:
        img = Image.open(p)
        k = 2 if img.width < 1400 else 1
        # A unique copy: the OCR process may keep the file it read open for a while, and the
        # walk overwrites its captures.
        _OCR_SEQ[0] += 1
        tmp_dir = Path(os.environ.get("TEMP", str(p.parent))) / "walk_ocr"
        tmp_dir.mkdir(exist_ok=True)
        q = tmp_dir / f"ocr_{os.getpid()}_{_OCR_SEQ[0]}.png"
        (img.resize((img.width * 2, img.height * 2), Image.LANCZOS) if k == 2 else img).save(q)
        proc = _ocr_server()
        assert proc.stdin and proc.stdout
        proc.stdin.write(str(q) + "\n")
        proc.stdin.flush()
        line = proc.stdout.readline()
        try:
            data = json.loads(line, strict=False) if line.strip() else {}
        except json.JSONDecodeError:
            # A recognised line with a stray backslash breaks PowerShell's JSON; drop them.
            try:
                data = json.loads(line.replace("\\", "/"), strict=False)
            except json.JSONDecodeError:
                data = {}
        result[str(p)] = [OcrLine(d["text"], d["x"] // k, d["y"] // k, d["w"] // k, d["h"] // k)
                          for d in (data.get("lines") or [])]
        for old_file in sorted(tmp_dir.glob(f"ocr_{os.getpid()}_*.png"))[:-20]:
            try:
                old_file.unlink()
            except OSError:
                pass
    return result


class LoginInUse(RuntimeError):
    """Another process holds this login dir (one login = one running process)."""


def lock_login(dir_: Path) -> Any:
    """Locks a login dir for this process (`login.lock`, same file tg-walk-peer locks).

    The returned file must stay open while the login is in use; the lock goes away with the
    process. A login that two processes use at once gets its key dropped by the server
    (AUTH_KEY_DUPLICATED), so this refuses instead."""
    import msvcrt

    dir_.mkdir(parents=True, exist_ok=True)
    try:
        f = open(dir_ / "login.lock", "a+")  # noqa: SIM115
    except PermissionError as e:
        raise LoginInUse(f"{dir_} is in use by another process") from e
    try:
        f.seek(0)
        msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
    except OSError as e:
        f.close()
        raise LoginInUse(f"{dir_} is in use by another process") from e
    return f


def read_test_credentials(doc: Path) -> tuple[str, str]:
    text = doc.read_text(encoding="utf-8")
    api_id = re.search(r"\*\*api_id\*\*:\s*(\d+)", text)
    api_hash = re.search(r"\*\*api_hash\*\*:\s*([0-9a-f]+)", text)
    if not api_id or not api_hash:
        raise RuntimeError("test credentials not found in the tdesktop docs")
    return api_id.group(1), api_hash.group(1)


def text_matches(pattern: str, text: str) -> bool:
    """Regex match that tolerates common OCR slips ("Wa 1k" for "Walk", l/1/I, O/0)."""
    if re.search(pattern, text, re.I):
        return True
    fixed = re.sub(r"(?<=[A-Za-z]) ?[1|I] ?(?=[a-z])", "l", text)
    fixed = re.sub(r"(?<=[A-Za-z])0|0(?=[a-z])", "o", fixed)
    if re.search(pattern, fixed, re.I):
        return True
    literal = re.sub(r"\\(.)", r"\1", pattern.strip("^$"))
    if re.search(r"[\[\]()*+?{}|.]", literal):
        return False
    import difflib

    a = re.sub(r"\s+", "", literal.lower())
    b = re.sub(r"\s+", "", fixed.lower())
    if not a or len(b) < len(a) * 0.7:
        return False
    starts, ends = pattern.startswith("^"), pattern.endswith("$")
    if starts and ends:
        return difflib.SequenceMatcher(None, a, b).ratio() >= 0.92
    if starts:
        return difflib.SequenceMatcher(None, a, b[:len(a)]).ratio() >= 0.92
    if ends:
        return difflib.SequenceMatcher(None, a, b[-len(a):]).ratio() >= 0.92
    best = 0.0
    for i in range(0, max(1, len(b) - len(a) + 1)):
        best = max(best, difflib.SequenceMatcher(None, a, b[i:i + len(a)]).ratio())
        if best >= 0.92:
            return True
    return False


def region_crop(img: Image.Image, box: tuple[float, float, float, float]) -> Image.Image:
    """Crops by fractions (x0, y0, x1, y1) of the image."""
    w, h = img.size
    return img.crop((int(box[0] * w), int(box[1] * h), int(box[2] * w), int(box[3] * h)))


def diff_ratio(a: Image.Image, b: Image.Image) -> float:
    if a.size != b.size:
        return 1.0
    x = np.asarray(a.convert("RGB"), dtype=np.int16)
    y = np.asarray(b.convert("RGB"), dtype=np.int16)
    return float((np.abs(x - y).max(axis=2) > 12).mean())


def window_class(hwnd: int) -> str:
    buf = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(wt.HWND(hwnd), buf, 256)
    return buf.value


def find_menu_window(pid: int) -> int:
    """A visible popup menu (#32768) owned by `pid`."""
    for w in wm.list_all_windows_for_pid(pid):
        if window_class(w.hwnd) == "#32768" and user32.IsWindowVisible(w.hwnd):
            return w.hwnd
    return 0


def capture_screen_window(hwnd: int, path: Path) -> Image.Image:
    r = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    frame = wm.capture_screen_rect(r.left, r.top, r.right - r.left, r.bottom - r.top)
    img = Image.fromarray(frame)
    img.save(path)
    return img


def monitor_rect(hwnd: int) -> tuple[int, int, int, int]:
    class MONITORINFO(ctypes.Structure):
        _fields_ = [("cbSize", wt.DWORD), ("rcMonitor", wt.RECT), ("rcWork", wt.RECT), ("dwFlags", wt.DWORD)]

    user32.MonitorFromWindow.restype = wt.HANDLE
    h = user32.MonitorFromWindow(wt.HWND(hwnd), 2)
    mi = MONITORINFO()
    mi.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(h, ctypes.byref(mi))
    r = mi.rcMonitor
    return (r.left, r.top, r.right, r.bottom)


CF_UNICODETEXT = 13


def clipboard_get() -> str:
    """Text on the clipboard (the walk saves and restores the user's clipboard around copy checks)."""
    k32 = ctypes.windll.kernel32
    k32.GlobalLock.restype = ctypes.c_void_p
    k32.GlobalLock.argtypes = [ctypes.c_void_p]
    user32.GetClipboardData.restype = ctypes.c_void_p
    for _ in range(10):
        if user32.OpenClipboard(None):
            break
        time.sleep(0.1)
    else:
        return ""
    try:
        h = user32.GetClipboardData(CF_UNICODETEXT)
        if not h:
            return ""
        p = k32.GlobalLock(h)
        try:
            return ctypes.wstring_at(p) if p else ""
        finally:
            k32.GlobalUnlock(ctypes.c_void_p(h))
    finally:
        user32.CloseClipboard()


def clipboard_set(text: str) -> None:
    k32 = ctypes.windll.kernel32
    k32.GlobalAlloc.restype = ctypes.c_void_p
    k32.GlobalLock.restype = ctypes.c_void_p
    k32.GlobalLock.argtypes = [ctypes.c_void_p]
    user32.SetClipboardData.argtypes = [wt.UINT, ctypes.c_void_p]
    data = (text + "\0").encode("utf-16-le")
    for _ in range(10):
        if user32.OpenClipboard(None):
            break
        time.sleep(0.1)
    else:
        return
    try:
        user32.EmptyClipboard()
        if text:
            h = k32.GlobalAlloc(0x0002, len(data))
            p = k32.GlobalLock(h)
            ctypes.memmove(p, data, len(data))
            k32.GlobalUnlock(ctypes.c_void_p(h))
            user32.SetClipboardData(CF_UNICODETEXT, h)
    finally:
        user32.CloseClipboard()
