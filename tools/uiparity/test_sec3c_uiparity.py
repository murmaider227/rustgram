#!/usr/bin/env python3
"""Regression tests for SEC3-C uiparity protections (C2 foreground & --test-account, C3 metrics.json)."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

UIPARITY_DIR = Path(__file__).resolve().parent
BENCH_DIR = UIPARITY_DIR.parent / "bench"
REPO_ROOT = UIPARITY_DIR.parent.parent

for _p in (str(UIPARITY_DIR), str(BENCH_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import win_capture as wc  # noqa: E402
import workdir_safety as ws  # noqa: E402


def _make_temp_base() -> Path:
    env_root = os.environ.get("SEC3C_SCRATCH_ROOT", "").strip()
    if env_root:
        root = Path(env_root)
        root.mkdir(parents=True, exist_ok=True)
        return Path(tempfile.mkdtemp(prefix="sec3c_uip_", dir=root))
    return Path(tempfile.mkdtemp(prefix="sec3c_uip_"))


class TestC2ForegroundAndTestAccountGuards(unittest.TestCase):
    def setUp(self) -> None:
        self.base = _make_temp_base()

    def tearDown(self) -> None:
        ws.remove_workdir_verified(self.base, retries=5, delay_s=0.2)

    def test_c2_send_hotkey_aborts_when_foreground_mismatch(self) -> None:
        target_hwnd = 0x1111
        other_hwnd = 0x2222
        with mock.patch.object(wc.user32, "GetForegroundWindow", return_value=other_hwnd):
            with mock.patch.object(wc.user32, "SendInput") as mock_send:
                with self.assertRaises(wc.ForegroundWindowError):
                    wc.send_hotkey(target_hwnd, "CTRL", "A")
                mock_send.assert_not_called()

    def test_c2_send_text_checks_foreground_before_every_key_batch(self) -> None:
        target_hwnd = 0x1234
        other_hwnd = 0x9999
        # First batch sees target_hwnd; focus shifts before second batch.
        fg_sequence = [target_hwnd, other_hwnd]
        with mock.patch.object(wc.user32, "GetForegroundWindow", side_effect=fg_sequence):
            with mock.patch.object(wc.user32, "SendInput", return_value=32) as mock_send:
                with self.assertRaises(wc.ForegroundWindowError):
                    wc.send_text(target_hwnd, "x" * 40, batch_size=16)
                self.assertEqual(mock_send.call_count, 1, "Must stop immediately before second batch on focus loss")

    def test_c2_click_and_scroll_abort_when_foreground_mismatch(self) -> None:
        target_hwnd = 0xAAAA
        other_hwnd = 0xBBBB
        with mock.patch.object(wc.user32, "GetForegroundWindow", return_value=other_hwnd):
            with mock.patch.object(wc.user32, "SendInput") as mock_send:
                with self.assertRaises(wc.ForegroundWindowError):
                    wc.click_client(target_hwnd, 100, 100)
                with self.assertRaises(wc.ForegroundWindowError):
                    wc.scroll_client(target_hwnd, 100, 100, -120)
                mock_send.assert_not_called()

    def test_c2_refuses_to_drive_without_test_account_flag(self) -> None:
        snap_dir = self.base / "snaps"
        scratch_dir = self.base / "scratch"
        dummy_wd = self.base / "wd"
        dummy_wd.mkdir(parents=True, exist_ok=True)
        dummy_exe = self.base / "noop.exe"
        dummy_exe.write_bytes(b"")

        cases = [
            [
                sys.executable, "-I", str(UIPARITY_DIR / "capture_official.py"),
                "--snap-dir", str(snap_dir), "--desktop", "current",
            ],
            [
                sys.executable, "-I", str(UIPARITY_DIR / "capture_rustgram.py"),
                "--snap-dir", str(snap_dir), "--scratch-dir", str(scratch_dir),
                "--exe", str(dummy_exe), "--workdir", str(dummy_wd), "--desktop", "current",
            ],
            [
                sys.executable, "-I", str(UIPARITY_DIR / "capture_rustgram.py"),
                "--snap-dir", str(snap_dir), "--attach-pid", "999999", "--desktop", "current",
            ],
            [
                sys.executable, "-I", str(UIPARITY_DIR / "win_capture.py"),
                "--desktop", "current", "--resize", "1440x900",
            ],
        ]
        for cmd in cases:
            res = subprocess.run(cmd, capture_output=True, text=True, check=False)
            self.assertEqual(res.returncode, 2, f"Expected exit 2 without --test-account for {cmd}; stderr={res.stderr}")
            self.assertIn("--test-account", res.stderr, f"Missing --test-account message: {res.stderr}")

    def test_c2_refuses_in_repo_snap_dir_with_test_account(self) -> None:
        in_repo_snap = REPO_ROOT / "tools" / "uiparity" / "_tmp_in_repo_snaps"
        res = subprocess.run(
            [
                sys.executable, "-I", str(UIPARITY_DIR / "capture_official.py"),
                "--test-account", "--snap-dir", str(in_repo_snap), "--desktop", "current",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(res.returncode, 2)
        self.assertIn("git working tree", res.stderr.lower())
        self.assertFalse(in_repo_snap.exists())


class TestC3MakePairsRelativeMetrics(unittest.TestCase):
    def setUp(self) -> None:
        self.base = _make_temp_base()

    def tearDown(self) -> None:
        ws.remove_workdir_verified(self.base, retries=5, delay_s=0.2)

    def test_c3_metrics_json_contains_only_relative_names(self) -> None:
        from PIL import Image

        snap_dir = self.base / "snaps"
        snap_dir.mkdir(parents=True, exist_ok=True)
        off_name = "01_chat_list_folders_1440x900_official.png"
        rg_name = "01_chat_list_folders_1440x900_rustgram.png"
        Image.new("RGB", (64, 64), (30, 40, 50)).save(snap_dir / off_name)
        Image.new("RGB", (64, 64), (32, 42, 52)).save(snap_dir / rg_name)

        res = subprocess.run(
            [sys.executable, "-B", "-I", str(UIPARITY_DIR / "make_pairs.py"), "--snap-dir", str(snap_dir)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(res.returncode, 0, f"make_pairs.py failed: stdout={res.stdout} stderr={res.stderr}")
        metrics_path = snap_dir / "metrics.json"
        self.assertTrue(metrics_path.is_file())
        raw = metrics_path.read_text(encoding="utf-8")
        data = json.loads(raw)
        self.assertEqual(len(data["pairs"]), 1)
        entry = data["pairs"][0]
        self.assertEqual(entry["official"], off_name)
        self.assertEqual(entry["rustgram"], rg_name)
        self.assertEqual(entry["pair"], "01_chat_list_folders_1440x900_pair.png")
        for key in ("official", "rustgram", "pair"):
            self.assertFalse(Path(entry[key]).is_absolute(), f"{key} must be relative: {entry[key]}")
            self.assertNotIn("\\", entry[key])
            self.assertNotIn(":", entry[key])
        # Full serialized JSON must also contain no absolute path back to snap_dir.
        self.assertNotIn(str(snap_dir), raw)

    def test_c3_make_pairs_refuses_in_repo_snap_dir(self) -> None:
        in_repo = REPO_ROOT / "tools" / "uiparity" / "_tmp_pairs_in_repo"
        res = subprocess.run(
            [sys.executable, "-B", "-I", str(UIPARITY_DIR / "make_pairs.py"), "--snap-dir", str(in_repo)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(res.returncode, 2)
        self.assertIn("git working tree", res.stderr.lower())


if __name__ == "__main__":
    unittest.main()
