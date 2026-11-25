#!/usr/bin/env python3
"""Helper to seed short A<->B test messages for scroll benchmarking and clean them up afterward.

All paths come from CLI arguments; target link comes strictly from a private file
(--target-link-file / BENCH_TARGET_LINK_FILE) or environment variable BENCH_TARGET_LINK
(never accepted on the CLI and never passed on Rustgram's command line).

Cleanup locates the A<->B chat by its visible title first, verifies the open chat header,
and verifies the DeleteMessagesBox confirmation modal before confirming deletion.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable
from urllib.parse import parse_qs, urlparse

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import win_metrics as wm

VK_RETURN = 0x0D
VK_ESCAPE = 0x1B

DEFAULT_LIST_RECT = (72, 70, 380, 790)
DEFAULT_HEADER_RECT = (380, 0, 1080, 68)
DEFAULT_MODAL_RECT = (350, 200, 850, 620)
DEFAULT_SEARCH_BOX_XY = (220, 46)
DEFAULT_FIRST_SEARCH_RESULT_XY = (220, 125)
CONFIRM_KEYWORDS = ("delete", "clear", "leave", "cancel", "are you sure", "also delete")


@dataclass(frozen=True)
class OcrLine:
    text: str
    bbox: tuple[int, int, int, int]  # (x, y, width, height) in cropped-image coordinates


OcrReader = Callable[[np.ndarray], list[OcrLine]]


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
        allow_abbrev=False,
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    sp_seed = sub.add_parser(
        "seed",
        help="Send N short test messages in the A<->B chat via Rustgram UI",
        allow_abbrev=False,
    )
    sp_seed.add_argument("--rustgram-exe", type=Path, required=True)
    sp_seed.add_argument("--workdir-a", type=Path, required=True)
    sp_seed.add_argument("--scratch-dir", type=Path, required=True)
    sp_seed.add_argument("--api-creds-doc", type=Path, default=None)
    sp_seed.add_argument("--target-title", default=os.environ.get("BENCH_TARGET_TITLE", ""),
                         help="Expected visible title of the A<->B chat (or set BENCH_TARGET_TITLE)")
    sp_seed.add_argument("--target-link-file", type=Path, default=None,
                         help="File containing BENCH_TARGET_LINK (never pass the link on the command line)")
    sp_seed.add_argument("--count", type=int, default=200)
    sp_seed.add_argument("--delay-s", type=float, default=0.45)
    sp_seed.add_argument("--diag-png", type=Path, default=None)

    sp_menu = sub.add_parser(
        "inspect-delete-menu",
        help="Right-click the matched A<->B chat row and save PNG",
        allow_abbrev=False,
    )
    sp_menu.add_argument("--rustgram-exe", type=Path, required=True)
    sp_menu.add_argument("--workdir-a", type=Path, required=True)
    sp_menu.add_argument("--scratch-dir", type=Path, required=True)
    sp_menu.add_argument("--api-creds-doc", type=Path, default=None)
    sp_menu.add_argument("--target-title", default=os.environ.get("BENCH_TARGET_TITLE", ""),
                         help="Visible title of the A<->B chat to locate before right-clicking")
    sp_menu.add_argument("--target-link-file", type=Path, default=None,
                         help="Optional file containing BENCH_TARGET_LINK for in-app search")
    sp_menu.add_argument("--diag-dir", type=Path, required=True)

    sp_clean = sub.add_parser(
        "cleanup",
        help="Delete the A<->B chat history after verifying title and confirmation box",
        allow_abbrev=False,
    )
    sp_clean.add_argument("--rustgram-exe", type=Path, required=True)
    sp_clean.add_argument("--workdir-a", type=Path, required=True)
    sp_clean.add_argument("--scratch-dir", type=Path, required=True)
    sp_clean.add_argument("--api-creds-doc", type=Path, default=None)
    sp_clean.add_argument("--target-title", default=os.environ.get("BENCH_TARGET_TITLE", ""),
                          help="Required visible title of the A<->B chat to delete (or set BENCH_TARGET_TITLE)")
    sp_clean.add_argument("--target-link-file", type=Path, default=None,
                          help="Optional file containing BENCH_TARGET_LINK for in-app search if chat is off-screen")
    sp_clean.add_argument("--menu-delete-xy", type=int, nargs=2, default=None,
                          help="Explicit window-client (x, y) of Delete item in context menu")
    sp_clean.add_argument("--menu-delete-offset", type=int, nargs=2, default=[60, 180],
                          help="Offset (dx, dy) from matched chat row to Delete menu item when --menu-delete-xy is omitted")
    sp_clean.add_argument("--confirm-xy", type=int, nargs=2, required=True,
                          help="Window-client (x, y) of the Delete confirm button inside DeleteMessagesBox")
    sp_clean.add_argument("--revoke-checkbox-xy", type=int, nargs=2, default=None,
                          help="Optional window-client (x, y) of 'Also delete for ...' checkbox inside DeleteMessagesBox")
    sp_clean.add_argument("--diag-png", type=Path, default=None)

    return p.parse_args(argv)


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


def load_target_link(link_file: Path | None = None) -> str:
    """Read BENCH_TARGET_LINK strictly from a file or environment variable (never CLI args)."""
    file_path = link_file
    if file_path is None:
        env_file = os.environ.get("BENCH_TARGET_LINK_FILE", "").strip()
        if env_file:
            file_path = Path(env_file)

    if file_path is not None:
        raw = Path(file_path).read_text(encoding="utf-8")
        for line in raw.splitlines():
            s = line.strip()
            if s and not s.startswith("#"):
                return s
        return ""

    return os.environ.get("BENCH_TARGET_LINK", "").strip()


def extract_search_query(link: str) -> str:
    """Convert a t.me / tg:// link or handle into an in-app search query without exposing it on argv."""
    s = link.strip()
    if not s:
        return ""
    if s.startswith("@") or re.fullmatch(r"\+\d{5,16}", s):
        return s

    lower = s.lower()
    if lower.startswith("tg://"):
        parsed = urlparse(s)
        qs = parse_qs(parsed.query)
        domain = (qs.get("domain") or [""])[0].strip().lstrip("@")
        if domain:
            return f"@{domain}"
        phone = (qs.get("phone") or [""])[0].strip()
        if phone:
            return phone if phone.startswith("+") else f"+{phone}"
        return ""

    normalized = s if "://" in s else f"https://{s}"
    parsed = urlparse(normalized)
    host = (parsed.hostname or "").lower()
    if host in ("t.me", "telegram.me", "telegram.dog"):
        seg = parsed.path.strip("/").split("/")[0] if parsed.path.strip("/") else ""
        if not seg:
            return ""
        if re.fullmatch(r"\+\d{5,16}", seg):
            return seg
        if seg.startswith("+"):
            return ""
        return f"@{seg.lstrip('@')}"
    return s


def build_rustgram_cmd(rustgram_exe: Path, workdir: Path) -> list[str]:
    """Build Rustgram launch argv without ever placing BENCH_TARGET_LINK on the command line."""
    return [str(rustgram_exe), "--live", "-testmode", "-workdir", str(workdir)]


def focus_window_messages(hwnd: int) -> None:
    wm.user32.PostMessageW(hwnd, wm.WM_ACTIVATE, 1, 0)
    wm.user32.PostMessageW(hwnd, wm.WM_SETFOCUS, 0, 0)


def _normalize_text(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip().casefold()


def _crop_frame(frame: np.ndarray, rect: tuple[int, int, int, int]) -> np.ndarray:
    h, w, _ = frame.shape
    x0 = max(0, min(rect[0], w - 1))
    y0 = max(0, min(rect[1], h - 1))
    x1 = max(x0 + 1, min(rect[2], w))
    y1 = max(y0 + 1, min(rect[3], h))
    return frame[y0:y1, x0:x1, :]


def ocr_image_windows(image_rgb: np.ndarray, scratch_dir: Path) -> list[OcrLine]:
    """Run Windows built-in Windows.Media.Ocr on an RGB numpy array; fails closed (returns []) on error."""
    if sys.platform != "win32":
        return []
    wm.ensure_outside_git_repo(scratch_dir, label="ocr-scratch")
    scratch_dir.mkdir(parents=True, exist_ok=True)
    tmp_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            prefix="ab_ocr_",
            suffix=".png",
            dir=scratch_dir,
            delete=False,
        ) as tf:
            tmp_path = Path(tf.name)
        wm.save_png(image_rgb, tmp_path)

        ps_script = r"""
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Storage.StorageFile, Windows.Storage, ContentType=WindowsRuntime]
$null = [Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics.Imaging, ContentType=WindowsRuntime]
$null = [Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType=WindowsRuntime]
$asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
})[0]
function Await($op, $type) {
    $m = $asTaskGeneric.MakeGenericMethod($type)
    $t = $m.Invoke($null, @($op))
    $t.Wait()
    return $t.Result
}
$file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($env:AB_OCR_PNG)) ([Windows.Storage.StorageFile])
$stream = Await ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
$decoder = Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
$bmp = Await ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
if ($null -eq $engine) { '[]'; exit 0 }
$res = Await ($engine.RecognizeAsync($bmp)) ([Windows.Media.Ocr.OcrResult])
$out = @()
foreach ($line in $res.Lines) {
    $minX = [double]::PositiveInfinity; $minY = [double]::PositiveInfinity
    $maxX = 0.0; $maxY = 0.0
    foreach ($w in $line.Words) {
        $r = $w.BoundingRect
        if ($r.X -lt $minX) { $minX = $r.X }
        if ($r.Y -lt $minY) { $minY = $r.Y }
        if (($r.X + $r.Width) -gt $maxX) { $maxX = $r.X + $r.Width }
        if (($r.Y + $r.Height) -gt $maxY) { $maxY = $r.Y + $r.Height }
    }
    if ([double]::IsInfinity($minX)) { continue }
    $out += [PSCustomObject]@{
        text = $line.Text
        x = [int][Math]::Round($minX)
        y = [int][Math]::Round($minY)
        w = [int][Math]::Max(1, [Math]::Round($maxX - $minX))
        h = [int][Math]::Max(1, [Math]::Round($maxY - $minY))
    }
}
ConvertTo-Json -InputObject $out -Compress
"""
        env = os.environ.copy()
        env["AB_OCR_PNG"] = str(tmp_path)
        res = subprocess.run(
            ["powershell", "-NoProfile", "-NonInteractive", "-Command", ps_script],
            capture_output=True,
            text=True,
            env=env,
            timeout=15,
            check=False,
        )
        if res.returncode != 0 or not res.stdout.strip():
            return []
        parsed = json.loads(res.stdout.strip())
        if isinstance(parsed, dict):
            parsed = [parsed]
        lines: list[OcrLine] = []
        for item in parsed:
            text = str(item.get("text", "")).strip()
            if not text:
                continue
            lines.append(
                OcrLine(
                    text=text,
                    bbox=(int(item["x"]), int(item["y"]), int(item["w"]), int(item["h"])),
                )
            )
        return lines
    except Exception:
        return []
    finally:
        if tmp_path is not None:
            try:
                tmp_path.unlink(missing_ok=True)
            except OSError:
                pass


def find_chat_row_by_title(
    frame: np.ndarray,
    target_title: str,
    *,
    ocr_reader: OcrReader,
    list_rect: tuple[int, int, int, int] = DEFAULT_LIST_RECT,
) -> tuple[int, int] | None:
    """Locate the dialog row matching `target_title` in `frame` and return client (x, y) to click, or None."""
    needle = _normalize_text(target_title)
    if not needle:
        return None
    crop = _crop_frame(frame, list_rect)
    lines = ocr_reader(crop)
    lx0, ly0, _, _ = list_rect
    for line in lines:
        hay = _normalize_text(line.text)
        if not hay:
            continue
        if hay == needle or needle in hay:
            bx, by, bw, bh = line.bbox
            return (lx0 + bx + bw // 2, ly0 + by + bh // 2)
    return None


def verify_chat_header_title(
    frame: np.ndarray,
    target_title: str,
    *,
    ocr_reader: OcrReader,
    header_rect: tuple[int, int, int, int] = DEFAULT_HEADER_RECT,
) -> bool:
    """Verify the currently open chat header displays `target_title`."""
    needle = _normalize_text(target_title)
    if not needle:
        return False
    crop = _crop_frame(frame, header_rect)
    lines = ocr_reader(crop)
    combined = _normalize_text(" ".join(l.text for l in lines))
    return bool(combined) and needle in combined


def verify_delete_confirmation_box(
    before_frame: np.ndarray,
    after_frame: np.ndarray,
    *,
    ocr_reader: OcrReader,
    modal_rect: tuple[int, int, int, int] = DEFAULT_MODAL_RECT,
    min_modal_diff: float = 0.02,
) -> tuple[bool, str]:
    """Verify the UIP4-F DeleteMessagesBox confirmation modal appeared before confirming deletion."""
    before_modal = _crop_frame(before_frame, modal_rect)
    after_modal = _crop_frame(after_frame, modal_rect)
    diff = wm.frame_diff_ratio(before_modal, after_modal, channel_threshold=8)
    if diff < min_modal_diff:
        return False, f"confirmation modal did not appear (modal diff={diff:.4f} < {min_modal_diff:.4f})"

    lines = ocr_reader(after_modal)
    text = _normalize_text(" ".join(l.text for l in lines))
    if not text:
        return False, "confirmation modal text could not be read (failing closed)"

    matched_kw = [kw for kw in CONFIRM_KEYWORDS if kw in text]
    if not matched_kw:
        return False, f"confirmation modal lacks expected delete/clear/leave/cancel text (saw: {text[:80]!r})"
    return True, f"confirmation modal verified (diff={diff:.3f}, keywords={matched_kw})"


def _search_in_app(hwnd: int, query: str) -> None:
    """Type `query` into Rustgram's search box via PostMessageW (never via argv)."""
    focus_window_messages(hwnd)
    wm.post_click(hwnd, *DEFAULT_SEARCH_BOX_XY)
    time.sleep(0.25)
    wm.post_unicode_text(hwnd, query)
    time.sleep(1.5)


def locate_and_open_target_chat(
    hwnd: int,
    target_title: str,
    target_link: str,
    *,
    ocr_reader: OcrReader,
) -> tuple[int, int] | None:
    """Find `target_title` in the chat list (or via in-app search), open it, and verify the header."""
    frame = wm.capture_window_printwindow(hwnd)
    if frame is None:
        return None
    row_xy = find_chat_row_by_title(frame, target_title, ocr_reader=ocr_reader)
    if row_xy is None and target_link:
        query = extract_search_query(target_link)
        if query:
            _search_in_app(hwnd, query)
            frame2 = wm.capture_window_printwindow(hwnd)
            if frame2 is not None:
                row_xy = find_chat_row_by_title(frame2, target_title, ocr_reader=ocr_reader)
    if row_xy is None:
        return None

    wm.post_click(hwnd, row_xy[0], row_xy[1])
    time.sleep(1.2)
    open_frame = wm.capture_window_printwindow(hwnd)
    if open_frame is None:
        return None
    if not verify_chat_header_title(open_frame, target_title, ocr_reader=ocr_reader):
        return None
    return row_xy


def cmd_seed(args: argparse.Namespace) -> int:
    wm.ensure_outside_git_repo(args.scratch_dir, label="--scratch-dir")
    if args.diag_png is not None:
        wm.ensure_outside_git_repo(args.diag_png, label="--diag-png")
    link = load_target_link(args.target_link_file)
    target_title = args.target_title.strip()
    if not link and not target_title:
        raise ValueError("Provide --target-title (or BENCH_TARGET_TITLE) or --target-link-file / BENCH_TARGET_LINK")
    env = build_env(args.api_creds_doc)

    with wm.ScratchSession(args.scratch_dir, graceful_timeout_s=5.0, post_stop_delay_s=1.5) as sess:
        wd = sess.copy_workdir(args.workdir_a, "ab_seed_wd")
        cmd = build_rustgram_cmd(args.rustgram_exe, wd)

        flood_hit = [0]
        p = subprocess.Popen(
            cmd,
            cwd=str(args.rustgram_exe.parent),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        sess.track_process(p)

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

        ocr_fn: OcrReader = lambda img: ocr_image_windows(img, args.scratch_dir)  # noqa: E731
        if target_title:
            matched = locate_and_open_target_chat(hwnd, target_title, link, ocr_reader=ocr_fn)
            if matched is None:
                sys.stderr.write(f"[seed] Aborting: could not locate/verify chat titled {target_title!r}.\n")
                return 3
        else:
            query = extract_search_query(link)
            if not query:
                sys.stderr.write("[seed] Aborting: could not extract search query from target link.\n")
                return 2
            _search_in_app(hwnd, query)
            wm.post_click(hwnd, *DEFAULT_FIRST_SEARCH_RESULT_XY)
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


def cmd_inspect_delete_menu(args: argparse.Namespace) -> int:
    wm.ensure_outside_git_repo(args.scratch_dir, label="--scratch-dir")
    wm.ensure_outside_git_repo(args.diag_dir, label="--diag-dir")
    target_title = args.target_title.strip()
    if not target_title:
        raise ValueError("inspect-delete-menu requires --target-title (or BENCH_TARGET_TITLE)")
    link = load_target_link(args.target_link_file)
    env = build_env(args.api_creds_doc)
    with wm.ScratchSession(args.scratch_dir, graceful_timeout_s=5.0, post_stop_delay_s=1.5) as sess:
        wd = sess.copy_workdir(args.workdir_a, "ab_del_inspect")
        p = subprocess.Popen(
            build_rustgram_cmd(args.rustgram_exe, wd),
            cwd=str(args.rustgram_exe.parent),
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        sess.track_process(p)
        time.sleep(5.0)
        hwnd = wm.list_visible_windows_for_pid(p.pid)[0].hwnd
        wm.move_window(hwnd, 60, 40, 1200, 800)
        time.sleep(2.0)
        ocr_fn: OcrReader = lambda img: ocr_image_windows(img, args.scratch_dir)  # noqa: E731
        row_xy = locate_and_open_target_chat(hwnd, target_title, link, ocr_reader=ocr_fn)
        if row_xy is None:
            sys.stderr.write(f"[inspect-delete-menu] Aborting: chat titled {target_title!r} not found.\n")
            return 3
        wm.post_right_click(hwnd, row_xy[0], row_xy[1])
        time.sleep(0.6)
        f = wm.capture_window_printwindow(hwnd)
        if f is not None:
            args.diag_dir.mkdir(parents=True, exist_ok=True)
            wm.save_png(f, args.diag_dir / "ab_ctx_menu.png")
            from bench_part2 import draw_grid_overlay

            wm.save_png(draw_grid_overlay(f, step=50), args.diag_dir / "ab_ctx_menu_grid.png")
        return 0


def run_cleanup_on_window(
    hwnd: int,
    *,
    target_title: str,
    target_link: str,
    menu_delete_xy: tuple[int, int] | None,
    menu_delete_offset: tuple[int, int],
    confirm_xy: tuple[int, int],
    revoke_checkbox_xy: tuple[int, int] | None,
    ocr_reader: OcrReader,
    diag_png: Path | None = None,
    sleep_fn: Callable[[float], None] = time.sleep,
) -> int:
    """Locate the target chat by title, verify its header and UIP4-F confirmation box, then confirm delete."""
    row_xy = locate_and_open_target_chat(hwnd, target_title, target_link, ocr_reader=ocr_reader)
    if row_xy is None:
        sys.stderr.write(
            f"[cleanup] Aborting: could not find and verify chat titled {target_title!r}; nothing deleted.\n"
        )
        return 3

    wm.post_right_click(hwnd, row_xy[0], row_xy[1])
    sleep_fn(0.6)
    before_delete = wm.capture_window_printwindow(hwnd)
    if before_delete is None:
        sys.stderr.write("[cleanup] Aborting: could not capture window before clicking delete menu item.\n")
        return 3

    if menu_delete_xy is not None:
        del_x, del_y = menu_delete_xy
    else:
        del_x, del_y = row_xy[0] + menu_delete_offset[0], row_xy[1] + menu_delete_offset[1]

    wm.post_click(hwnd, del_x, del_y)
    sleep_fn(0.8)
    after_delete = wm.capture_window_printwindow(hwnd)
    if after_delete is None:
        sys.stderr.write("[cleanup] Aborting: could not capture confirmation box frame.\n")
        return 3

    ok, reason = verify_delete_confirmation_box(before_delete, after_delete, ocr_reader=ocr_reader)
    if not ok:
        wm.post_vk(hwnd, VK_ESCAPE)
        sys.stderr.write(f"[cleanup] Aborting before confirm click: {reason}.\n")
        return 3

    if revoke_checkbox_xy is not None:
        wm.post_click(hwnd, revoke_checkbox_xy[0], revoke_checkbox_xy[1])
        sleep_fn(0.4)
    wm.post_click(hwnd, confirm_xy[0], confirm_xy[1])
    sleep_fn(4.0)
    if diag_png is not None:
        f = wm.capture_window_printwindow(hwnd)
        if f is not None:
            wm.save_png(f, diag_png)
    print(f"[cleanup] Delete chat confirmed ({reason}).", flush=True)
    return 0


def cmd_cleanup(args: argparse.Namespace) -> int:
    wm.ensure_outside_git_repo(args.scratch_dir, label="--scratch-dir")
    if args.diag_png is not None:
        wm.ensure_outside_git_repo(args.diag_png, label="--diag-png")
    target_title = args.target_title.strip()
    if not target_title:
        raise ValueError(
            "cleanup requires --target-title (or BENCH_TARGET_TITLE); refusing to blindly delete row 0"
        )
    link = load_target_link(args.target_link_file)
    env = build_env(args.api_creds_doc)
    with wm.ScratchSession(args.scratch_dir, graceful_timeout_s=5.0, post_stop_delay_s=1.5) as sess:
        wd = sess.copy_workdir(args.workdir_a, "ab_cleanup_wd")
        p = subprocess.Popen(
            build_rustgram_cmd(args.rustgram_exe, wd),
            cwd=str(args.rustgram_exe.parent),
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        sess.track_process(p)
        time.sleep(5.0)
        wins = wm.list_visible_windows_for_pid(p.pid)
        if not wins:
            raise RuntimeError("Rustgram window did not appear")
        hwnd = wins[0].hwnd
        wm.move_window(hwnd, 60, 40, 1200, 800)
        time.sleep(2.0)
        ocr_fn: OcrReader = lambda img: ocr_image_windows(img, args.scratch_dir)  # noqa: E731
        return run_cleanup_on_window(
            hwnd,
            target_title=target_title,
            target_link=link,
            menu_delete_xy=(args.menu_delete_xy[0], args.menu_delete_xy[1]) if args.menu_delete_xy else None,
            menu_delete_offset=(args.menu_delete_offset[0], args.menu_delete_offset[1]),
            confirm_xy=(args.confirm_xy[0], args.confirm_xy[1]),
            revoke_checkbox_xy=(args.revoke_checkbox_xy[0], args.revoke_checkbox_xy[1]) if args.revoke_checkbox_xy else None,
            ocr_reader=ocr_fn,
            diag_png=args.diag_png,
        )


def main() -> int:
    args = parse_args()
    try:
        if args.cmd == "seed":
            return cmd_seed(args)
        if args.cmd == "inspect-delete-menu":
            return cmd_inspect_delete_menu(args)
        if args.cmd == "cleanup":
            return cmd_cleanup(args)
    except ValueError as exc:
        sys.stderr.write(f"[ab_chat_ops] {exc}\n")
        return 2
    except wm.WorkdirCleanupError as exc:
        sys.stderr.write(f"[ab_chat_ops] {exc}\n")
        return 4
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
