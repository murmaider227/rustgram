"""Automated 30-screen UI parity capture for Official Telegram Desktop (Windows).

Drives an already-running Official Telegram Desktop instance (never starts a second
copy, never logs out) through all 25 screens (expanded to 30 captures including
5 Night-theme screens and 2 narrow 390x844 screens).

No hardcoded local paths: supply --snap-dir (or UIPARITY_SNAP_DIR) and --exe.
Run with: python -I tools/uiparity/capture_official.py --snap-dir <DIR>
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import win_capture as wc  # noqa: E402


def wait_ui(sec: float = 0.5) -> None:
    time.sleep(sec)


def row_y(row_idx: int) -> int:
    """Center Y of dialog row `row_idx` (0-based) in 1440x900 Official Telegram."""
    return 107 + row_idx * 62


def ensure_size(hwnd: int, w: int, h: int, pos_x: int, pos_y: int) -> None:
    wc.focus_window(hwnd)
    wc.resize_client_area(hwnd, w, h, pos_x, pos_y)
    wait_ui(0.25)


def snap(hwnd: int, snap_dir: Path, name: str, size_tag: str = "1440x900") -> None:
    out = snap_dir / f"{name}_{size_tag}_official.png"
    img = wc.capture_window_client(hwnd, str(out), method="screen")
    print(f"[OFFICIAL] {out.name}  ({img.size[0]}x{img.size[1]})")


def close_overlays(hwnd: int, pos_x: int, pos_y: int) -> None:
    for _ in range(3):
        wc.send_hotkey("ESC")
        wait_ui(0.18)
    ensure_size(hwnd, 1440, 900, pos_x, pos_y)


def toggle_night_mode(hwnd: int, pos_x: int, pos_y: int) -> None:
    """Toggle Night Mode via Main Menu (assumes Auto-Night Mode is already Off)."""
    ensure_size(hwnd, 1440, 900, pos_x, pos_y)
    wc.click_client(hwnd, 36, 52)
    wait_ui(0.45)
    wc.click_client(hwnd, 238, 517)
    wait_ui(0.6)
    wc.send_hotkey("ESC")
    wait_ui(0.3)


def main() -> int:
    ap = argparse.ArgumentParser(description="Capture 30 UI parity screens from Official Telegram.")
    ap.add_argument("--snap-dir", default=os.environ.get("UIPARITY_SNAP_DIR", ""))
    ap.add_argument("--exe", default=os.environ.get("UIPARITY_OFFICIAL_EXE", "official_tg"))
    ap.add_argument("--desktop", default="Default")
    ap.add_argument("--pos", default="-1860,60")
    ap.add_argument("--row-dm", type=int, default=0, help="0-based row of 1:1 DM chat")
    ap.add_argument("--row-group", type=int, default=1, help="0-based row of Group chat")
    ap.add_argument("--row-channel", type=int, default=2, help="0-based row of Channel")
    ap.add_argument("--row-saved", type=int, default=3, help="0-based row of Saved Messages")
    ap.add_argument("--search-query", default="Parity")
    ap.add_argument("--msg-x", type=int, default=1080, help="X coord inside a text message bubble")
    ap.add_argument("--msg-y", type=int, default=380, help="Y coord inside a text message bubble")
    ap.add_argument("--photo-x", type=int, default=1120, help="X coord inside photo message")
    ap.add_argument("--photo-y", type=int, default=620, help="Y coord inside photo message")
    args = ap.parse_args()

    if not args.snap_dir:
        print("Pass --snap-dir or set UIPARITY_SNAP_DIR", file=sys.stderr)
        return 2

    snap_dir = Path(args.snap_dir)
    snap_dir.mkdir(parents=True, exist_ok=True)
    pos_x, pos_y = (int(v) for v in args.pos.split(","))

    hdesk = wc.attach_desktop(args.desktop) if args.desktop.lower() != "current" else None
    try:
        win = wc.find_window_by_exe(args.exe)
        if not win:
            print(f"Official Telegram window not found for exe={args.exe!r}", file=sys.stderr)
            return 1
        hwnd = win.hwnd
        close_overlays(hwnd, pos_x, pos_y)

        # 01. Chat list (all chats) with folders bar (no active chat selected)
        wc.click_client(hwnd, 36, 118)  # All chats folder
        wait_ui(0.3)
        wc.send_hotkey("ESC")
        wait_ui(0.3)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "01_chat_list_folders")

        # 02. One-to-one chat with text, reply, photo, file, reactions
        wc.click_client(hwnd, 220, row_y(args.row_dm))
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "02_one_to_one_chat")

        # 05. Composer focused with text (on 1:1 chat)
        wc.click_client(hwnd, 860, 875)
        wait_ui(0.2)
        wc.send_text("Checking composer focus, caret, and send button styling")
        wait_ui(0.3)
        snap(hwnd, snap_dir, "05_composer_text")
        wc.send_hotkey("CTRL", "A")
        wc.send_hotkey("BACK")
        wait_ui(0.2)

        # 06. Emoji panel, 07. Sticker panel, 08. GIF panel (docked right column resized back to 1440x900)
        wc.click_client(hwnd, 1367, 875)  # Open emoji/sticker/GIF sidebar
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        wc.click_client(hwnd, 1196, 52)  # Emoji tab
        wait_ui(0.4)
        snap(hwnd, snap_dir, "06_emoji_panel")
        wc.click_client(hwnd, 1294, 52)  # Stickers tab
        wait_ui(0.5)
        snap(hwnd, snap_dir, "07_sticker_panel")
        wc.click_client(hwnd, 1392, 52)  # GIFs tab
        wait_ui(0.6)
        snap(hwnd, snap_dir, "08_gif_panel")
        # Close emoji sidebar
        wc.click_client(hwnd, 1080, 875)
        wait_ui(0.4)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)

        # 09. Message context menu (on 1:1 chat)
        wc.click_client(hwnd, args.msg_x, args.msg_y, button="right")
        wait_ui(0.55)
        snap(hwnd, snap_dir, "09_message_context_menu")

        # 20. Forward box (click 'Forward' item in the open context menu)
        wc.click_client(hwnd, args.msg_x + 75, args.msg_y + 125)
        wait_ui(0.6)
        snap(hwnd, snap_dir, "20_forward_box")
        wc.send_hotkey("ESC")
        wait_ui(0.25)

        # 10. Chat list context menu (right-click DM row)
        wc.click_client(hwnd, 220, row_y(args.row_dm), button="right")
        wait_ui(0.5)
        snap(hwnd, snap_dir, "10_chat_list_context_menu")
        wc.send_hotkey("ESC")
        wait_ui(0.25)

        # 11. Profile of a user (third column / info) on 1:1 chat
        wc.click_client(hwnd, 1372, 52)  # Toggle right info column
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "11_user_profile")
        # Close third column
        wc.click_client(hwnd, 1412, 52)
        wait_ui(0.4)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)

        # 19. Media viewer on a photo (click photo in 1:1 chat)
        wc.click_client(hwnd, args.photo_x, args.photo_y)
        wait_ui(0.8)
        mv_wins = [
            w for w in wc.enum_windows(lambda i: i.visible and i.pid == win.pid and "media viewer" in i.title.lower())
        ]
        if mv_wins:
            mv_hwnd = mv_wins[0].hwnd
            wc.resize_client_area(mv_hwnd, 1440, 900, pos_x, pos_y)
            wait_ui(0.4)
            snap(mv_hwnd, snap_dir, "19_media_viewer_photo")
        else:
            snap(hwnd, snap_dir, "19_media_viewer_photo")
        wc.send_hotkey("ESC")
        wait_ui(0.4)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)

        # 03. Group chat & 12. Group info
        wc.click_client(hwnd, 220, row_y(args.row_group))
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "03_group_chat")
        wc.click_client(hwnd, 1372, 52)  # Open Group Info third column
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "12_group_info")
        wc.click_client(hwnd, 1412, 52)  # Close Info column
        wait_ui(0.4)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)

        # 04. Channel
        wc.click_client(hwnd, 220, row_y(args.row_channel))
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "04_channel")

        # 23. Saved Messages
        wc.click_client(hwnd, 220, row_y(args.row_saved))
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "23_saved_messages")

        # 18. Search (global, with results)
        wc.click_client(hwnd, 220, 52)  # Search field in dialogs column
        wait_ui(0.3)
        wc.send_text(args.search_query)
        wait_ui(0.9)
        snap(hwnd, snap_dir, "18_search_global")
        wc.send_hotkey("ESC")
        wait_ui(0.3)

        # 22. Main menu (hamburger)
        wc.click_client(hwnd, 36, 52)
        wait_ui(0.5)
        snap(hwnd, snap_dir, "22_main_menu")

        # 21. New group box (from Main Menu -> New Group at y=275)
        wc.click_client(hwnd, 120, 275)
        wait_ui(0.5)
        snap(hwnd, snap_dir, "21_new_group_box")
        wc.send_hotkey("ESC")
        wait_ui(0.3)

        # 13-17. Settings main + Notifications + Privacy + Chat Settings + Advanced
        wc.click_client(hwnd, 36, 52)
        wait_ui(0.45)
        wc.click_client(hwnd, 120, 476)  # Settings
        wait_ui(0.55)
        snap(hwnd, snap_dir, "13_settings_main")

        wc.click_client(hwnd, 650, 285)  # Notifications and Sounds
        wait_ui(0.5)
        snap(hwnd, snap_dir, "14_settings_notifications")
        wc.click_client(hwnd, 565, 86)  # Back
        wait_ui(0.4)

        wc.click_client(hwnd, 650, 326)  # Privacy and Security
        wait_ui(0.5)
        snap(hwnd, snap_dir, "15_settings_privacy")
        wc.click_client(hwnd, 565, 86)  # Back
        wait_ui(0.4)

        wc.click_client(hwnd, 650, 366)  # Chat Settings
        wait_ui(0.5)
        snap(hwnd, snap_dir, "16_settings_chat")
        wc.click_client(hwnd, 565, 86)  # Back
        wait_ui(0.4)

        wc.click_client(hwnd, 650, 446)  # Advanced
        wait_ui(0.5)
        snap(hwnd, snap_dir, "17_settings_advanced")
        wc.send_hotkey("ESC")
        wait_ui(0.25)
        wc.send_hotkey("ESC")
        wait_ui(0.25)
        wc.send_hotkey("ESC")
        wait_ui(0.3)

        # 29-30. Narrow 390x844: chat list and chat (Day theme, 1:1 DM)
        wc.click_client(hwnd, 36, 118)  # Ensure All chats tab (exit any search)
        wait_ui(0.3)
        wc.click_client(hwnd, 220, row_y(args.row_dm))
        wait_ui(0.5)
        ensure_size(hwnd, 390, 844, pos_x, pos_y)
        snap(hwnd, snap_dir, "30_narrow_chat", "390x844")
        wc.send_hotkey("ESC")
        wait_ui(0.4)
        snap(hwnd, snap_dir, "29_narrow_chat_list", "390x844")

        # Restore 1440x900 and switch to Night theme for 5 key screens (24-28)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        toggle_night_mode(hwnd, pos_x, pos_y)

        # 24. Night chat list
        wc.send_hotkey("ESC")
        wait_ui(0.3)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "24_night_chat_list")

        # 25. Night chat (1:1 DM)
        wc.click_client(hwnd, 220, row_y(args.row_dm))
        wait_ui(0.5)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "25_night_chat")

        # 27. Night profile (third column)
        wc.click_client(hwnd, 1372, 52)
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        snap(hwnd, snap_dir, "27_night_profile")
        wc.click_client(hwnd, 1412, 52)
        wait_ui(0.4)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)

        # 28. Night emoji panel
        wc.click_client(hwnd, 1367, 875)
        wait_ui(0.6)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)
        wc.click_client(hwnd, 1196, 52)
        wait_ui(0.4)
        snap(hwnd, snap_dir, "28_night_emoji_panel")
        wc.click_client(hwnd, 1080, 875)
        wait_ui(0.4)
        ensure_size(hwnd, 1440, 900, pos_x, pos_y)

        # 26. Night settings
        wc.click_client(hwnd, 36, 52)
        wait_ui(0.45)
        wc.click_client(hwnd, 120, 476)
        wait_ui(0.55)
        snap(hwnd, snap_dir, "26_night_settings")
        wc.send_hotkey("ESC")
        wait_ui(0.3)

        # Return to Day theme cleanly
        toggle_night_mode(hwnd, pos_x, pos_y)
        print("Official Telegram 30-screen capture complete.")
        return 0
    finally:
        if hdesk:
            wc.user32.CloseDesktop(hdesk)


if __name__ == "__main__":
    raise SystemExit(main())
