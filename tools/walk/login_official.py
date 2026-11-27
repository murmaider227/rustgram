"""Signs a fresh copy of the official Telegram Desktop into a TEST-server account, with no phone:
the client's login QR is read from its window and confirmed by an approver login of the account
(`tg-walk-peer approveqr`). Its working dir becomes that client's own login (never copy it and run
two copies).

    python -I -X utf8 login_official.py --exe <Telegram.exe> --workdir <empty dir> \
        --approver <approver login dir> --peer <tg-walk-peer.exe> --tdesktop <tdesktop checkout> \
        [--shots <dir>]

Steps (tdesktop 7.4): intro -> Start Messaging -> QR screen -> SETTINGS -> type "testmode" in the
settings box (settings/settings_codes.cpp switches the empty account to the test environment) ->
close -> the QR now belongs to the test servers -> read it (qr_clean.py) and approve. Input is posted to the client's own window only.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import walklib as wl  # noqa: E402
from PIL import Image  # noqa: E402
from qr_clean import clean_qr  # noqa: E402


def find(c: wl.Client, shots: Path, pattern: str, name: str) -> wl.OcrLine | None:
    path = shots / f"login_{name}.png"
    c.shot(path)
    for ln in wl.ocr([path]).get(str(path), []):
        if wl.text_matches(pattern, ln.text):
            return ln
    return None


def click_text(c: wl.Client, shots: Path, pattern: str, name: str, tries: int = 6) -> bool:
    for _ in range(tries):
        ln = find(c, shots, pattern, name)
        if ln:
            c.click(ln.x + ln.w // 2, ln.y + ln.h // 2)
            time.sleep(1.5)
            return True
        time.sleep(1.5)
    return False


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True)
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--approver", required=True)
    ap.add_argument("--peer", required=True)
    ap.add_argument("--tdesktop", required=True)
    ap.add_argument("--shots", default="")
    a = ap.parse_args()
    wd = Path(a.workdir)
    if (wd / "tdata").exists():
        print("workdir already holds a login")
        return 2
    wd.mkdir(parents=True, exist_ok=True)
    shots = Path(a.shots) if a.shots else wd.parent / (wd.name + "_loginshots")
    shots.mkdir(parents=True, exist_ok=True)
    lock = wl.lock_login(wd)  # noqa: F841  held until the client is closed
    c = wl.Client(name="official", exe=Path(a.exe), workdir=wd, pos=(-1910, 20), size=(1000, 760),
                  args=["-noupdate"], log=shots / "official_login.log")
    try:
        c.start()
        time.sleep(6)
        c.place()
        # English intro if the system language was picked.
        if find(c, shots, r"Continue in English", "lang"):
            click_text(c, shots, r"Continue in English", "lang")
        if not click_text(c, shots, r"^Start Messaging", "start"):
            print("no Start Messaging button")
            return 3
        time.sleep(3)
        # The QR screen has a SETTINGS link; "testmode" typed in that box switches the empty
        # account to the test servers (settings/settings_codes.cpp).
        if not click_text(c, shots, r"^SETTINGS$|^Settings$", "settings"):
            print("no Settings link on the QR screen")
            return 3
        time.sleep(1.5)
        c.type("testmode")
        time.sleep(1.5)
        print("test environment toast:", bool(find(c, shots, r"test environment", "toast")))
        c.key("escape")
        time.sleep(4)
        if not find(c, shots, r"Scan From Mobile", "qrscreen"):
            click_text(c, shots, r"^Start Messaging", "start2")
            time.sleep(3)
        for attempt in range(3):
            label = find(c, shots, r"Scan From Mobile", f"qr{attempt}")
            img = Image.open(shots / f"login_qr{attempt}.png")
            if label is None:
                print("QR screen not found")
                time.sleep(3)
                continue
            box = (max(0, label.x + label.w // 2 - 160), max(0, label.y - 260),
                   min(img.width, label.x + label.w // 2 + 160), label.y - 4)
            clean = clean_qr(img.crop(box))
            if clean is None:
                print("QR not found in the capture")
                time.sleep(3)
                continue
            grey = shots / "qr.grey"
            grey.write_bytes(clean.tobytes())
            env = os.environ.copy()
            env["TDESKTOP_DIR"] = a.tdesktop
            r = subprocess.run([a.peer, "approveqr", "--approver", a.approver, "--grey", str(grey),
                                "--width", str(clean.width), "--height", str(clean.height)],
                               capture_output=True, text=True, env=env, timeout=120)
            grey.unlink(missing_ok=True)
            print("approveqr:", (r.stdout or r.stderr).strip()[-200:])
            if r.returncode == 0:
                break
            time.sleep(3)
        done = None
        for _ in range(12):
            time.sleep(5)
            done = find(c, shots, r"^Saved Messages|^Search$|All chats|Select a chat", "after")
            if done:
                break
        print("signed in:", bool(done))
        return 0 if done else 4
    finally:
        c.stop()


if __name__ == "__main__":
    sys.exit(main())
