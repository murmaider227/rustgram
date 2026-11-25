#!/usr/bin/env python3
"""Process-tree termination, verified workdir cleanup, and git-tree guards.

Pure Python stdlib + ctypes (no NumPy/Pillow dependency) so both tools/bench
and tools/uiparity can share one hardened lifecycle implementation.
"""

from __future__ import annotations

import atexit
import ctypes
import ctypes.wintypes as wt
import os
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable


class WorkdirCleanupError(RuntimeError):
    """Raised when a temporary workdir copy could not be deleted from disk."""

    def __init__(self, paths: list[Path], message: str | None = None) -> None:
        self.paths = [Path(p) for p in paths]
        if message is None:
            joined = ", ".join(str(p) for p in self.paths)
            message = f"Failed to remove temporary workdir copy (auth keys may remain on disk): {joined}"
        super().__init__(message)


# ---------------------------------------------------------------------------
# Git working-tree guard
# ---------------------------------------------------------------------------

_REPO_ROOT = Path(__file__).resolve().parents[2]


def _has_git_marker_ancestor(path: Path) -> Path | None:
    """Return the ancestor containing a .git file/dir, or None."""
    for candidate in (path, *path.parents):
        if (candidate / ".git").exists():
            return candidate
    return None


def is_inside_git_repo(path: Path) -> bool:
    """True if `path` (or its symlink-resolved target) is inside a git working tree."""
    candidates: list[Path] = []
    abs_path = Path(os.path.abspath(str(path)))
    candidates.append(abs_path)
    try:
        resolved = path.resolve(strict=False)
        if resolved != abs_path:
            candidates.append(resolved)
    except OSError:
        pass

    repo_root_norm = os.path.normcase(os.path.normpath(str(_REPO_ROOT)))
    for cand in candidates:
        if _has_git_marker_ancestor(cand) is not None:
            return True
        cand_norm = os.path.normcase(os.path.normpath(str(cand)))
        if cand_norm == repo_root_norm or cand_norm.startswith(repo_root_norm + os.sep):
            return True
    return False


def ensure_outside_git_repo(path: Path, label: str = "scratch-dir") -> Path:
    """Refuse any path inside a git working tree (prevents committing auth keys or PII captures)."""
    p = Path(path)
    if is_inside_git_repo(p):
        raise ValueError(
            f"Refusing to use {label} inside a git working tree: {os.path.abspath(str(p))}. "
            f"Choose a private directory outside the repository."
        )
    return p


# ---------------------------------------------------------------------------
# Verified workdir copy & deletion
# ---------------------------------------------------------------------------


def _on_rmtree_error(func: Callable[..., Any], path: str, _exc_info: Any) -> None:
    """Clear read-only attributes and retry once during shutil.rmtree."""
    try:
        os.chmod(path, stat.S_IWRITE | stat.S_IREAD)
        func(path)
    except OSError:
        pass


def remove_workdir_verified(
    path: Path,
    retries: int = 8,
    delay_s: float = 0.5,
) -> None:
    """Delete `path` with retries and raise WorkdirCleanupError if it still exists."""
    target = Path(path)
    if not target.exists() and not target.is_symlink():
        return

    attempts = max(1, int(retries))
    for attempt in range(attempts):
        try:
            if target.is_symlink() or target.is_file():
                try:
                    os.chmod(target, stat.S_IWRITE | stat.S_IREAD)
                except OSError:
                    pass
                target.unlink(missing_ok=True)
            elif target.exists():
                shutil.rmtree(target, onerror=_on_rmtree_error)
        except FileNotFoundError:
            pass
        except OSError:
            pass

        if not target.exists() and not target.is_symlink():
            return
        if attempt + 1 < attempts:
            time.sleep(delay_s)

    if target.exists() or target.is_symlink():
        raise WorkdirCleanupError([target])


def copy_workdir_safely(src: Path, dst: Path) -> None:
    """Copy a Telegram/Rustgram workdir into `dst`, failing closed on any copy error."""
    src_path = Path(src)
    dst_path = Path(dst)
    if not src_path.is_dir():
        raise FileNotFoundError(f"Source workdir does not exist or is not a directory: {src_path}")

    if dst_path.exists() or dst_path.is_symlink():
        remove_workdir_verified(dst_path)
    dst_path.mkdir(parents=True, exist_ok=True)

    skip_names = {"working", "log.txt", "debuglogs"}
    skip_dirs = {"debuglogs", "dumps", "temp"}
    for root, dirs, files in os.walk(src_path):
        rel = Path(root).relative_to(src_path)
        dirs[:] = [d for d in dirs if d.lower() not in skip_dirs]
        target_dir = dst_path / rel
        target_dir.mkdir(parents=True, exist_ok=True)
        for fname in files:
            low = fname.lower()
            if low in skip_names or low.startswith("log_start"):
                continue
            s_file = Path(root) / fname
            d_file = target_dir / fname
            try:
                shutil.copy2(s_file, d_file)
            except OSError:
                # Fallback when source is held open with restrictive sharing; fail closed if this fails too.
                with open(s_file, "rb") as rf, open(d_file, "wb") as wf:
                    shutil.copyfileobj(rf, wf)


# ---------------------------------------------------------------------------
# Win32 process tree & Job Object helpers
# ---------------------------------------------------------------------------

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_TERMINATE = 0x0001
PROCESS_SET_QUOTA = 0x0100
SYNCHRONIZE = 0x00100000

WM_CLOSE = 0x0010
GW_OWNER = 4
TH32CS_SNAPPROCESS = 0x00000002
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value
WAIT_OBJECT_0 = 0x00000000

JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS = 9
JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000


class FILETIME(ctypes.Structure):
    _fields_ = [("dwLowDateTime", wt.DWORD), ("dwHighDateTime", wt.DWORD)]

    def to_u64(self) -> int:
        return (self.dwHighDateTime << 32) | self.dwLowDateTime


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


class IO_COUNTERS(ctypes.Structure):
    _fields_ = [
        ("ReadOperationCount", ctypes.c_uint64),
        ("WriteOperationCount", ctypes.c_uint64),
        ("OtherOperationCount", ctypes.c_uint64),
        ("ReadTransferCount", ctypes.c_uint64),
        ("WriteTransferCount", ctypes.c_uint64),
        ("OtherTransferCount", ctypes.c_uint64),
    ]


class JOBOBJECT_BASIC_LIMIT_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("PerProcessUserTimeLimit", ctypes.c_int64),
        ("PerJobUserTimeLimit", ctypes.c_int64),
        ("LimitFlags", wt.DWORD),
        ("MinimumWorkingSetSize", ctypes.c_size_t),
        ("MaximumWorkingSetSize", ctypes.c_size_t),
        ("ActiveProcessLimit", wt.DWORD),
        ("Affinity", ctypes.c_size_t),
        ("PriorityClass", wt.DWORD),
        ("SchedulingClass", wt.DWORD),
    ]


class JOBOBJECT_EXTENDED_LIMIT_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("BasicLimitInformation", JOBOBJECT_BASIC_LIMIT_INFORMATION),
        ("IoInfo", IO_COUNTERS),
        ("ProcessMemoryLimit", ctypes.c_size_t),
        ("JobMemoryLimit", ctypes.c_size_t),
        ("PeakProcessMemoryUsed", ctypes.c_size_t),
        ("PeakJobMemoryUsed", ctypes.c_size_t),
    ]


_IS_WINDOWS = sys.platform == "win32"

if _IS_WINDOWS:
    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _user32 = ctypes.WinDLL("user32", use_last_error=True)

    _kernel32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
    _kernel32.OpenProcess.restype = wt.HANDLE
    _kernel32.CloseHandle.argtypes = [wt.HANDLE]
    _kernel32.CloseHandle.restype = wt.BOOL
    _kernel32.TerminateProcess.argtypes = [wt.HANDLE, wt.UINT]
    _kernel32.TerminateProcess.restype = wt.BOOL
    _kernel32.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
    _kernel32.WaitForSingleObject.restype = wt.DWORD
    _kernel32.GetProcessTimes.argtypes = [
        wt.HANDLE,
        ctypes.POINTER(FILETIME),
        ctypes.POINTER(FILETIME),
        ctypes.POINTER(FILETIME),
        ctypes.POINTER(FILETIME),
    ]
    _kernel32.GetProcessTimes.restype = wt.BOOL
    _kernel32.CreateToolhelp32Snapshot.argtypes = [wt.DWORD, wt.DWORD]
    _kernel32.CreateToolhelp32Snapshot.restype = wt.HANDLE
    _kernel32.Process32FirstW.argtypes = [wt.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    _kernel32.Process32FirstW.restype = wt.BOOL
    _kernel32.Process32NextW.argtypes = [wt.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    _kernel32.Process32NextW.restype = wt.BOOL

    _kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wt.LPCWSTR]
    _kernel32.CreateJobObjectW.restype = wt.HANDLE
    _kernel32.SetInformationJobObject.argtypes = [wt.HANDLE, ctypes.c_int, ctypes.c_void_p, wt.DWORD]
    _kernel32.SetInformationJobObject.restype = wt.BOOL
    _kernel32.AssignProcessToJobObject.argtypes = [wt.HANDLE, wt.HANDLE]
    _kernel32.AssignProcessToJobObject.restype = wt.BOOL
    _kernel32.TerminateJobObject.argtypes = [wt.HANDLE, wt.UINT]
    _kernel32.TerminateJobObject.restype = wt.BOOL

    _HANDLER_ROUTINE = ctypes.WINFUNCTYPE(wt.BOOL, wt.DWORD)
    _kernel32.SetConsoleCtrlHandler.argtypes = [_HANDLER_ROUTINE, wt.BOOL]
    _kernel32.SetConsoleCtrlHandler.restype = wt.BOOL

    _WNDENUMPROC = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    _user32.EnumWindows.argtypes = [_WNDENUMPROC, wt.LPARAM]
    _user32.EnumWindows.restype = wt.BOOL
    _user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]
    _user32.GetWindowThreadProcessId.restype = wt.DWORD
    _user32.GetWindow.argtypes = [wt.HWND, wt.UINT]
    _user32.GetWindow.restype = wt.HWND
    _user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
    _user32.PostMessageW.restype = wt.BOOL


class ProcessJob:
    """Windows Job Object configured with KILL_ON_JOB_CLOSE so descendants never outlive cleanup."""

    def __init__(self) -> None:
        self._handle: int | None = None
        if not _IS_WINDOWS:
            return
        h = _kernel32.CreateJobObjectW(None, None)
        if not h:
            return
        info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION()
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        ok = _kernel32.SetInformationJobObject(
            h,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            ctypes.byref(info),
            ctypes.sizeof(info),
        )
        if not ok:
            _kernel32.CloseHandle(h)
            return
        self._handle = int(h)

    @property
    def active(self) -> bool:
        return self._handle is not None

    def assign_pid(self, pid: int) -> bool:
        if not _IS_WINDOWS or self._handle is None or pid <= 0:
            return False
        hp = _kernel32.OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, False, pid)
        if not hp:
            return False
        try:
            return bool(_kernel32.AssignProcessToJobObject(self._handle, hp))
        finally:
            _kernel32.CloseHandle(hp)

    def terminate(self, exit_code: int = 1) -> bool:
        if not _IS_WINDOWS or self._handle is None:
            return False
        return bool(_kernel32.TerminateJobObject(self._handle, exit_code))

    def close(self) -> None:
        if _IS_WINDOWS and self._handle is not None:
            h = self._handle
            self._handle = None
            _kernel32.CloseHandle(h)


def list_descendant_pids(root_pid: int) -> list[int]:
    """Return [root_pid] plus all live descendant PIDs."""
    if not _IS_WINDOWS or root_pid <= 0:
        return [root_pid] if root_pid > 0 else []
    snap = _kernel32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if snap == INVALID_HANDLE_VALUE:
        return [root_pid]
    try:
        children: dict[int, list[int]] = {}
        entry = PROCESSENTRY32W()
        entry.dwSize = ctypes.sizeof(PROCESSENTRY32W)
        ok = _kernel32.Process32FirstW(snap, ctypes.byref(entry))
        while ok:
            children.setdefault(int(entry.th32ParentProcessID), []).append(int(entry.th32ProcessID))
            ok = _kernel32.Process32NextW(snap, ctypes.byref(entry))

        result: list[int] = [root_pid]
        queue = [root_pid]
        seen = {root_pid}
        while queue:
            cur = queue.pop(0)
            for ch in children.get(cur, []):
                if ch not in seen:
                    seen.add(ch)
                    result.append(ch)
                    queue.append(ch)
        return result
    finally:
        _kernel32.CloseHandle(snap)


def get_process_creation_time(pid: int) -> int | None:
    """Return process creation timestamp (100-ns FILETIME ticks) to guard against PID reuse."""
    if not _IS_WINDOWS or pid <= 0:
        return None
    h = _kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    try:
        ct, et, kt, ut = FILETIME(), FILETIME(), FILETIME(), FILETIME()
        if not _kernel32.GetProcessTimes(
            h, ctypes.byref(ct), ctypes.byref(et), ctypes.byref(kt), ctypes.byref(ut)
        ):
            return None
        return ct.to_u64()
    finally:
        _kernel32.CloseHandle(h)


def _post_close_windows_for_pid(pid: int) -> None:
    if not _IS_WINDOWS or pid <= 0:
        return
    hwnds: list[int] = []

    def _cb(hwnd: int, _lparam: int) -> bool:
        win_pid = wt.DWORD()
        _user32.GetWindowThreadProcessId(hwnd, ctypes.byref(win_pid))
        if int(win_pid.value) == pid and not _user32.GetWindow(hwnd, GW_OWNER):
            hwnds.append(hwnd)
        return True

    _user32.EnumWindows(_WNDENUMPROC(_cb), 0)
    for hwnd in hwnds:
        _user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)


def _terminate_and_wait_pid(pid: int, expected_ctime: int | None = None, wait_ms: int = 3000) -> None:
    if not _IS_WINDOWS or pid <= 0:
        return
    if expected_ctime is not None:
        cur_ctime = get_process_creation_time(pid)
        if cur_ctime is None or cur_ctime != expected_ctime:
            return
    h = _kernel32.OpenProcess(SYNCHRONIZE | PROCESS_TERMINATE, False, pid)
    if not h:
        return
    try:
        if _kernel32.WaitForSingleObject(h, 0) == WAIT_OBJECT_0:
            return
        _kernel32.TerminateProcess(h, 0)
        _kernel32.WaitForSingleObject(h, wait_ms)
    finally:
        _kernel32.CloseHandle(h)


def stop_process_by_pid(
    pid: int,
    graceful_timeout_s: float = 5.0,
    job: ProcessJob | None = None,
) -> None:
    """Gracefully close windows of `pid`, then ALWAYS terminate and wait on the entire process tree.

    Descendants are snapshotted before WM_CLOSE (with creation times to prevent PID-reuse collisions)
    and again afterward, so child processes (e.g. WebView2 / widget helpers) that outlive a clean root
    exit are still terminated and waited on before callers delete temporary workdirs.
    """
    if pid <= 0:
        if job is not None:
            job.terminate()
            job.close()
        return

    if not _IS_WINDOWS:
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            pass
        return

    # 1. Snapshot root + descendants (and creation times) BEFORE sending WM_CLOSE.
    pre_pids = list_descendant_pids(pid)
    pre_ctimes: dict[int, int | None] = {}
    for p in pre_pids:
        pre_ctimes[p] = get_process_creation_time(p)

    # 2. Ask the root process to close cleanly.
    _post_close_windows_for_pid(pid)

    h_root = _kernel32.OpenProcess(SYNCHRONIZE | PROCESS_TERMINATE, False, pid)
    try:
        if h_root:
            wait_ms = max(0, int(graceful_timeout_s * 1000))
            _kernel32.WaitForSingleObject(h_root, wait_ms)

        # 3. Collect any remaining live descendants (both newly-seen and pre-snapshotted).
        post_pids = list_descendant_pids(pid)
        # Terminate job object first if active (kills all processes in the job atomically).
        if job is not None:
            job.terminate()

        # Terminate children in reverse order (deepest first), then root.
        ordered_targets: list[tuple[int, int | None]] = []
        seen: set[int] = set()
        for p in reversed(post_pids):
            if p != pid and p not in seen:
                seen.add(p)
                ordered_targets.append((p, pre_ctimes.get(p)))
        for p in reversed(pre_pids):
            if p != pid and p not in seen:
                seen.add(p)
                ordered_targets.append((p, pre_ctimes.get(p)))

        for child_pid, expected_ct in ordered_targets:
            _terminate_and_wait_pid(child_pid, expected_ctime=expected_ct, wait_ms=3000)

        # Ensure root itself is terminated and exited.
        if h_root:
            if _kernel32.WaitForSingleObject(h_root, 0) != WAIT_OBJECT_0:
                _kernel32.TerminateProcess(h_root, 0)
                _kernel32.WaitForSingleObject(h_root, 3000)
    finally:
        if h_root:
            _kernel32.CloseHandle(h_root)
        if job is not None:
            job.close()


# ---------------------------------------------------------------------------
# Global cleanup registry (atexit + signals + Win32 console control)
# ---------------------------------------------------------------------------

_REGISTRY_LOCK = threading.RLock()
_ACTIVE_SESSIONS: list[ScratchSession] = []
_HANDLERS_INSTALLED = False
_PREV_SIGNAL_HANDLERS: dict[int, Any] = {}
_WIN_CTRL_HANDLER_REF: Any = None


def _cleanup_all_active_sessions() -> None:
    with _REGISTRY_LOCK:
        sessions = list(reversed(_ACTIVE_SESSIONS))
    for sess in sessions:
        try:
            sess.cleanup(raise_on_failure=False)
        except Exception:
            pass


def _signal_handler(signum: int, frame: Any) -> None:
    _cleanup_all_active_sessions()
    prev = _PREV_SIGNAL_HANDLERS.get(signum)
    if signum == signal.SIGINT:
        if callable(prev):
            prev(signum, frame)
            return
        raise KeyboardInterrupt
    if callable(prev):
        prev(signum, frame)
        return
    sys.exit(128 + signum)


def _install_cleanup_handlers() -> None:
    global _HANDLERS_INSTALLED, _WIN_CTRL_HANDLER_REF
    with _REGISTRY_LOCK:
        if _HANDLERS_INSTALLED:
            return
        _HANDLERS_INSTALLED = True
        atexit.register(_cleanup_all_active_sessions)
        if threading.current_thread() is threading.main_thread():
            sigs = [signal.SIGINT, signal.SIGTERM]
            if hasattr(signal, "SIGBREAK"):
                sigs.append(signal.SIGBREAK)  # type: ignore[attr-defined]
            for sig in sigs:
                try:
                    _PREV_SIGNAL_HANDLERS[sig] = signal.getsignal(sig)
                    signal.signal(sig, _signal_handler)
                except (ValueError, OSError):
                    pass
        if _IS_WINDOWS:
            def _win_ctrl_cb(ctrl_type: int) -> bool:
                # CTRL_CLOSE_EVENT(2), CTRL_LOGOFF_EVENT(5), CTRL_SHUTDOWN_EVENT(6)
                if ctrl_type in (2, 5, 6):
                    _cleanup_all_active_sessions()
                return False  # Let default / Python signal handler continue

            _WIN_CTRL_HANDLER_REF = _HANDLER_ROUTINE(_win_ctrl_cb)
            try:
                _kernel32.SetConsoleCtrlHandler(_WIN_CTRL_HANDLER_REF, True)
            except Exception:
                pass


# ---------------------------------------------------------------------------
# Managed scratch-workdir + process-tree session
# ---------------------------------------------------------------------------


class ScratchSession:
    """Context manager owning temporary workdir copies and spawned client process trees.

    Guarantees:
    - Refuses any scratch_dir inside a git working tree.
    - Tracks each workdir BEFORE copying/creating it, so partial copies on error are cleaned up.
    - Tracks multiple workdirs and processes (e.g. delivery A and B): if the second copy or
      launch fails, the first process tree is stopped and BOTH copies are deleted.
    - Stops the entire process tree (Job Object + descendant snapshot) before deleting.
    - Verifies every tracked workdir is gone after rmtree (retrying, then raising
      WorkdirCleanupError naming the leftover path(s)).
    """

    def __init__(
        self,
        scratch_dir: Path,
        *,
        graceful_timeout_s: float = 5.0,
        post_stop_delay_s: float = 1.0,
        rm_retries: int = 8,
        rm_delay_s: float = 0.5,
    ) -> None:
        self.scratch_dir = ensure_outside_git_repo(Path(scratch_dir), label="scratch-dir")
        self.graceful_timeout_s = graceful_timeout_s
        self.post_stop_delay_s = post_stop_delay_s
        self.rm_retries = rm_retries
        self.rm_delay_s = rm_delay_s
        self._lock = threading.RLock()
        self._workdirs: list[Path] = []
        self._processes: list[tuple[int, ProcessJob | None, subprocess.Popen[Any] | None]] = []
        self._files: list[Any] = []
        self._cleaned = False

    def __enter__(self) -> ScratchSession:
        _install_cleanup_handlers()
        self.scratch_dir.mkdir(parents=True, exist_ok=True)
        with _REGISTRY_LOCK:
            _ACTIVE_SESSIONS.append(self)
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.cleanup(raise_on_failure=True)

    def register_workdir(self, path: Path) -> Path:
        p = ensure_outside_git_repo(Path(path), label="workdir-copy")
        with self._lock:
            if p not in self._workdirs:
                self._workdirs.append(p)
        return p

    def copy_workdir(self, src: Path, name: str) -> Path:
        dst = self.register_workdir(self.scratch_dir / name)
        copy_workdir_safely(src, dst)
        return dst

    def make_empty_workdir(self, name: str) -> Path:
        dst = self.register_workdir(self.scratch_dir / name)
        if dst.exists() or dst.is_symlink():
            remove_workdir_verified(dst, retries=self.rm_retries, delay_s=self.rm_delay_s)
        dst.mkdir(parents=True, exist_ok=True)
        return dst

    def track_process(self, proc_or_pid: subprocess.Popen[Any] | int) -> ProcessJob | None:
        if isinstance(proc_or_pid, subprocess.Popen):
            pid = int(proc_or_pid.pid)
            proc: subprocess.Popen[Any] | None = proc_or_pid
        else:
            pid = int(proc_or_pid)
            proc = None
        job = ProcessJob()
        if job.active:
            if not job.assign_pid(pid):
                job.close()
                job = None  # type: ignore[assignment]
        else:
            job = None  # type: ignore[assignment]
        with self._lock:
            self._processes.append((pid, job, proc))
        return job

    def track_file(self, fh: Any) -> Any:
        with self._lock:
            self._files.append(fh)
        return fh

    def cleanup(self, raise_on_failure: bool = True) -> None:
        with self._lock:
            if self._cleaned:
                return
            self._cleaned = True
            procs = list(reversed(self._processes))
            files = list(reversed(self._files))
            workdirs = list(reversed(self._workdirs))
            self._processes.clear()
            self._files.clear()
            self._workdirs.clear()

        with _REGISTRY_LOCK:
            try:
                _ACTIVE_SESSIONS.remove(self)
            except ValueError:
                pass

        had_procs = bool(procs)
        for pid, job, proc in procs:
            try:
                stop_process_by_pid(pid, graceful_timeout_s=self.graceful_timeout_s, job=job)
            except Exception:
                if job is not None:
                    try:
                        job.terminate()
                        job.close()
                    except Exception:
                        pass
            if proc is not None:
                try:
                    proc.wait(timeout=2.0)
                except Exception:
                    pass

        for fh in files:
            try:
                fh.close()
            except Exception:
                pass

        if had_procs and workdirs and self.post_stop_delay_s > 0:
            time.sleep(self.post_stop_delay_s)

        leftovers: list[Path] = []
        for wd in workdirs:
            try:
                remove_workdir_verified(wd, retries=self.rm_retries, delay_s=self.rm_delay_s)
            except WorkdirCleanupError as err:
                leftovers.extend(err.paths)

        if leftovers:
            err = WorkdirCleanupError(leftovers)
            sys.stderr.write(f"[cleanup-error] {err}\n")
            sys.stderr.flush()
            if raise_on_failure:
                raise err
