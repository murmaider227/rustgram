"""Automated 30-screen UI parity capture for Rustgram (Windows).

Launches Rustgram in live test-DC mode against a fresh copy of Account A's workdir,
drives it through the exact same 30 screens as Official Telegram Desktop (Day 1440x900,
Night 1440x900, and Narrow 390x844), saves `<nn>_<screen>_<size>_rustgram.png`, and
terminates strictly its own PID on exit.

No hardcoded local paths: configure via CLI arguments or UIPARITY_* / RUSTGRAM_* env vars.
Run with: python -I tools/uiparity/capture_rustgram.py --exe <EXE> --workdir <COPY> --snap-dir <DIR>
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "bench"))
import win_capture as wc  # noqa: E402
import workdir_safety as ws  # noqa: E402


def wait_ui(sec: float = 0.5) -> None:
    time.sleep(sec)


def row_y(row_idx: int) -> int:
    """Center Y of dialog row `row_idx` (0-based) in 1440x900 Rustgram."""
    return 104 + row_idx * 62


# Click targets in the 1440x900 client area (layout after the stage-2 parity fixes).
ROW_X = 300
HAMBURGER = (36, 52)
SEARCH = (300, 46)
COMPOSER = (900, 870)
EMOJI_BUTTON = (1372, 876)
EMOJI_BUTTON_DOCKED = (1080, 876)
PANEL_TABS = {"emoji": (1196, 46), "stickers": (1293, 46), "gifs": (1391, 46)}
INFO_TOGGLE = (1381, 52)
INFO_CLOSE = (1413, 52)
FORWARD_OFFSET = (79, 161)
MENU_NEW_GROUP = (96, 213)
MENU_SETTINGS = (86, 413)
MENU_NIGHT_MODE = (236, 453)
SETTINGS_ROWS = {"notifications": (665, 301), "privacy": (665, 341), "chat": (665, 381), "advanced": (665, 461)}
SETTINGS_BACK = (551, 89)
SETTINGS_CLOSE = (888, 89)


def bring_top(hwnd: int) -> None:
    wc.user32.SetWindowPos(hwnd, -1, 0, 0, 0, 0, 0x0001 | 0x0002)
    wc.user32.SetWindowPos(hwnd, -2, 0, 0, 0, 0, 0x0001 | 0x0002)
    wc.focus_window(hwnd)


def ensure_size(hwnd: int, w: int, h: int, pos_x: int, pos_y: int) -> None:
    wc.resize_client_area(hwnd, w, h, pos_x, pos_y)
    bring_top(hwnd)
    wait_ui(0.3)


def snap(hwnd: int, snap_dir: Path, name: str, size_tag: str = "1440x900") -> None:
    out = snap_dir / f"{name}_{size_tag}_rustgram.png"
    img = wc.capture_window_client(hwnd, str(out), method="screen")
    print(f"[RUSTGRAM] {out.name}  ({img.size[0]}x{img.size[1]})")


def load_credentials(creds_doc: str) -> tuple[str, str]:
    api_id = os.environ.get("RUSTGRAM_API_ID") or os.environ.get("TGFORK_API_ID") or ""
    api_hash = os.environ.get("RUSTGRAM_API_HASH") or os.environ.get("TGFORK_API_HASH") or ""
    if api_id and api_hash:
        return api_id, api_hash
    if not creds_doc:
        raise RuntimeError("Set RUSTGRAM_API_ID/RUSTGRAM_API_HASH or pass --credentials-doc")
    txt = Path(creds_doc).read_text(encoding="utf-8")
    m_id = re.search(r"api_id\*\*:\s*(\d+)", txt)
    m_hash = re.search(r"api_hash\*\*:\s*([0-9a-f]+)", txt)
    if not m_id or not m_hash:
        raise RuntimeError(f"Could not parse test API credentials from {creds_doc}")
    return m_id.group(1), m_hash.group(1)


def toggle_night_mode(hwnd: int) -> None:
    wc.click_client(hwnd, *HAMBURGER)
    wait_ui(0.45)
    wc.click_client(hwnd, *MENU_NIGHT_MODE)
    wait_ui(0.8)
    wc.send_hotkey(hwnd, "ESC")
    wait_ui(0.3)


def open_settings(hwnd: int) -> None:
    wc.click_client(hwnd, *HAMBURGER)
    wait_ui(0.45)
    wc.click_client(hwnd, *MENU_SETTINGS)
    wait_ui(0.8)


def main() -> int:
    ap = argparse.ArgumentParser(description="Capture 30 UI parity screens from Rustgram.")
    ap.add_argument("--exe", default=os.environ.get("UIPARITY_RUSTGRAM_EXE", ""))
    ap.add_argument("--workdir", default=os.environ.get("UIPARITY_RUSTGRAM_WORKDIR", ""))
    ap.add_argument("--scratch-dir", default=os.environ.get("UIPARITY_SCRATCH_DIR", ""),
                    help="Private directory outside the git repo for temporary workdir copies (required when launching)")
    ap.add_argument("--snap-dir", default=os.environ.get("UIPARITY_SNAP_DIR", ""))
    ap.add_argument("--credentials-doc", default=os.environ.get("UIPARITY_CREDS_DOC", ""))
    ap.add_argument("--ffmpeg-bin", default=os.environ.get("UIPARITY_FFMPEG_BIN", ""))
    ap.add_argument("--desktop", default="Default")
    ap.add_argument("--attach-pid", type=int, default=0,
                    help="Drive an already running Rustgram (live, test DC) instead of launching one")
    ap.add_argument("--test-account", action="store_true",
                    help="Confirm the target Rustgram window/workdir belongs to a disposable test account (required)")
    ap.add_argument("--pos", default="1980,60")
    ap.add_argument("--row-dm", type=int, default=0)
    ap.add_argument("--row-group", type=int, default=1)
    ap.add_argument("--row-channel", type=int, default=2)
    ap.add_argument("--row-saved", type=int, default=3)
    ap.add_argument("--search-query", default="Parity")
    ap.add_argument("--msg-x", type=int, default=1200, help="X inside an outgoing text bubble")
    ap.add_argument("--msg-y", type=int, default=385, help="Y inside an outgoing text bubble")
    ap.add_argument("--photo-x", type=int, default=1200)
    ap.add_argument("--photo-y", type=int, default=560)
    args = ap.parse_args()

    try:
        wc.require_test_account_confirmation(args.test_account, "capture_rustgram.py")
    except PermissionError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    if not args.snap_dir or not (args.attach_pid or (args.exe and args.workdir)):
        print("Pass --snap-dir and either --attach-pid or --exe with --workdir", file=sys.stderr)
        return 2

    launched = not args.attach_pid
    if launched and not args.scratch_dir:
        print("Pass --scratch-dir (or set UIPARITY_SCRATCH_DIR) outside the git repo when launching Rustgram", file=sys.stderr)
        return 2

    try:
        snap_dir = ws.ensure_outside_git_repo(Path(args.snap_dir), label="--snap-dir")
        if launched:
            scratch_dir = ws.ensure_outside_git_repo(Path(args.scratch_dir), label="--scratch-dir")
        else:
            scratch_dir = None
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    snap_dir.mkdir(parents=True, exist_ok=True)
    pos_x, pos_y = (int(v) for v in args.pos.split(","))

    session_cm = (
        ws.ScratchSession(scratch_dir, graceful_timeout_s=5.0, post_stop_delay_s=1.5)
        if launched and scratch_dir is not None
        else None
    )
    hdesk = None
    try:
        if session_cm is not None:
            session_cm.__enter__()
            wd_copy = session_cm.copy_workdir(Path(args.workdir), "uiparity_rustgram_wd")
            for stale in [wd_copy / "tdata" / "working", wd_copy / "shell_state.json"]:
                if stale.exists():
                    stale.unlink()

            api_id, api_hash = load_credentials(args.credentials_doc)
            env = os.environ.copy()
            env["RUSTGRAM_API_ID"] = api_id
            env["RUSTGRAM_API_HASH"] = api_hash
            env["RUSTGRAM_LIVE"] = "1"
            env["TGFORK_HEADLESS_AUDIO"] = "1"
            if args.ffmpeg_bin:
                env["PATH"] = args.ffmpeg_bin + os.pathsep + env.get("PATH", "")

            desktop_arg = rf"WinSta0\{args.desktop}" if args.desktop.lower() != "current" else ""
            pid = wc.launch_on_desktop(
                args.exe,
                ["--live", "-testmode", "-many", "-noupdate", "-workdir", str(wd_copy)],
                env,
                desktop=desktop_arg,
            )
            session_cm.track_process(pid)
            print(f"Launched Rustgram PID={pid}")
        else:
            pid = args.attach_pid
            print(f"Attaching to Rustgram PID={pid}")
        hdesk = wc.attach_desktop(args.desktop) if args.desktop.lower() != "current" else None
        win = None
        for _ in range(50):
            time.sleep(0.4)
            win = wc.find_window_by_exe(Path(args.exe).name if args.exe else "", pid=pid)
            if win:
                break
        if not win:
            print("Rustgram window did not appear", file=sys.stderr)
            return 1
        hwnd = win.hwnd
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        wait_ui(5.0 if launched else 0.5)  # Initial MTProto sync + dialogs + folders load

        # 01. Chat list (all chats) with folders bar (no active chat selected)
        wc.click_client(hwnd, 36, 110)  # All chats folder
        wait_ui(0.3)
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.5)
        snap(hwnd, snap_dir, "01_chat_list_folders")

        # 02. One-to-one chat (wait for media downloads)
        wc.click_client(hwnd, ROW_X, row_y(args.row_dm))
        wait_ui(4.0)
        snap(hwnd, snap_dir, "02_one_to_one_chat")

        # 05. Composer focused with text
        wc.click_client(hwnd, *COMPOSER)
        wait_ui(0.25)
        wc.send_text(hwnd, "Checking composer focus, caret, and send button styling")
        wait_ui(0.35)
        snap(hwnd, snap_dir, "05_composer_text")
        wc.send_hotkey(hwnd, "CTRL", "A")
        wc.send_hotkey(hwnd, "BACK")
        wait_ui(0.25)

        # 06-08. Emoji / Stickers / GIFs in the docked third column
        wc.click_client(hwnd, *EMOJI_BUTTON)
        wait_ui(0.8)
        wc.click_client(hwnd, *PANEL_TABS["emoji"])
        wait_ui(0.6)
        snap(hwnd, snap_dir, "06_emoji_panel")
        wc.click_client(hwnd, *PANEL_TABS["stickers"])
        wait_ui(4.0)
        snap(hwnd, snap_dir, "07_sticker_panel")
        wc.click_client(hwnd, *PANEL_TABS["gifs"])
        wait_ui(1.5)
        snap(hwnd, snap_dir, "08_gif_panel")
        wc.click_client(hwnd, *EMOJI_BUTTON_DOCKED)  # close the docked panel
        wait_ui(0.6)

        # 09. Message context menu (outgoing reply bubble, same message as the official run)
        wc.click_client(hwnd, args.msg_x, args.msg_y, button="right")
        wait_ui(0.6)
        snap(hwnd, snap_dir, "09_message_context_menu")

        # 20. Forward box
        wc.click_client(hwnd, args.msg_x + FORWARD_OFFSET[0], args.msg_y + FORWARD_OFFSET[1])
        wait_ui(0.8)
        snap(hwnd, snap_dir, "20_forward_box")
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.4)

        # 10. Chat list context menu (right-click DM row)
        wc.click_client(hwnd, ROW_X, row_y(args.row_dm), button="right")
        wait_ui(0.6)
        snap(hwnd, snap_dir, "10_chat_list_context_menu")
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.4)

        # 11. User profile (third column)
        wc.click_client(hwnd, *INFO_TOGGLE)
        wait_ui(1.0)
        snap(hwnd, snap_dir, "11_user_profile")
        wc.click_client(hwnd, *INFO_CLOSE)
        wait_ui(0.5)

        # 19. Media viewer on photo
        wc.click_client(hwnd, args.photo_x, args.photo_y)
        wait_ui(1.2)
        snap(hwnd, snap_dir, "19_media_viewer_photo")
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.5)

        # 03. Group chat & 12. Group info
        wc.click_client(hwnd, ROW_X, row_y(args.row_group))
        wait_ui(1.5)
        snap(hwnd, snap_dir, "03_group_chat")
        wc.click_client(hwnd, *INFO_TOGGLE)
        wait_ui(1.0)
        snap(hwnd, snap_dir, "12_group_info")
        wc.click_client(hwnd, *INFO_CLOSE)
        wait_ui(0.5)

        # 04. Channel
        wc.click_client(hwnd, ROW_X, row_y(args.row_channel))
        wait_ui(3.0)
        snap(hwnd, snap_dir, "04_channel")

        # 23. Saved Messages
        wc.click_client(hwnd, ROW_X, row_y(args.row_saved))
        wait_ui(2.5)
        snap(hwnd, snap_dir, "23_saved_messages")

        # 18. Search (global, with results)
        wc.click_client(hwnd, *SEARCH)
        wait_ui(0.3)
        wc.send_text(hwnd, args.search_query)
        wait_ui(2.0)
        snap(hwnd, snap_dir, "18_search_global")
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.4)

        # 22. Main menu
        wc.click_client(hwnd, *HAMBURGER)
        wait_ui(0.6)
        snap(hwnd, snap_dir, "22_main_menu")

        # 21. New group box
        wc.click_client(hwnd, *MENU_NEW_GROUP)
        wait_ui(0.8)
        snap(hwnd, snap_dir, "21_new_group_box")
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.4)

        # 13-17. Settings layer + sub-pages
        open_settings(hwnd)
        snap(hwnd, snap_dir, "13_settings_main")
        for key, name in (
            ("notifications", "14_settings_notifications"),
            ("privacy", "15_settings_privacy"),
            ("chat", "16_settings_chat"),
            ("advanced", "17_settings_advanced"),
        ):
            wc.click_client(hwnd, *SETTINGS_ROWS[key])
            wait_ui(0.9)
            snap(hwnd, snap_dir, name)
            wc.click_client(hwnd, *SETTINGS_BACK)
            wait_ui(0.6)
        wc.click_client(hwnd, *SETTINGS_CLOSE)
        wait_ui(0.5)

        # 29-30. Narrow 390x844: chat and chat list (Day theme, 1:1 DM)
        wc.click_client(hwnd, ROW_X, row_y(args.row_dm))
        wait_ui(0.8)
        ensure_size(hwnd, 390, 844, pos_x, pos_y)
        wait_ui(0.8)
        snap(hwnd, snap_dir, "30_narrow_chat", "390x844")
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.6)
        snap(hwnd, snap_dir, "29_narrow_chat_list", "390x844")

        # Night theme screens (24-28)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        wait_ui(0.5)
        toggle_night_mode(hwnd)
        wc.send_hotkey(hwnd, "ESC")
        wait_ui(0.5)
        snap(hwnd, snap_dir, "24_night_chat_list")

        wc.click_client(hwnd, ROW_X, row_y(args.row_dm))
        wait_ui(1.0)
        snap(hwnd, snap_dir, "25_night_chat")

        wc.click_client(hwnd, *INFO_TOGGLE)
        wait_ui(1.0)
        snap(hwnd, snap_dir, "27_night_profile")
        wc.click_client(hwnd, *INFO_CLOSE)
        wait_ui(0.5)

        wc.click_client(hwnd, *EMOJI_BUTTON)
        wait_ui(0.8)
        wc.click_client(hwnd, *PANEL_TABS["emoji"])
        wait_ui(0.6)
        snap(hwnd, snap_dir, "28_night_emoji_panel")
        wc.click_client(hwnd, *EMOJI_BUTTON_DOCKED)
        wait_ui(0.5)

        open_settings(hwnd)
        snap(hwnd, snap_dir, "26_night_settings")
        wc.click_client(hwnd, *SETTINGS_CLOSE)
        wait_ui(0.5)

        # Back to the Day theme
        toggle_night_mode(hwnd)
        print("Rustgram 30-screen capture complete.")
        if not launched:
            return 0

        wc.user32.PostMessageW(hwnd, 0x0010, 0, 0)
        wait_ui(1.0)
        return 0
    except wc.ForegroundWindowError as exc:
        print(f"[capture_rustgram] {exc}", file=sys.stderr)
        return 3
    finally:
        if hdesk:
            wc.user32.CloseDesktop(hdesk)
        if session_cm is not None:
            session_cm.__exit__(None, None, None)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ws.WorkdirCleanupError as exc:
        print(f"[capture_rustgram] {exc}", file=sys.stderr)
        raise SystemExit(4)
