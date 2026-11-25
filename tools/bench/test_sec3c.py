#!/usr/bin/env python3
"""Regression tests for SEC3-C (tools/bench and tools/uiparity security fixes).

Runs completely offline against dummy directories and dummy Python subprocesses
(never launches or touches any Telegram client or real workdir).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

BENCH_DIR = Path(__file__).resolve().parent
UIPARITY_DIR = BENCH_DIR.parent / "uiparity"
REPO_ROOT = BENCH_DIR.parent.parent

for _p in (str(BENCH_DIR), str(UIPARITY_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import ab_chat_ops  # noqa: E402
import bench_part2  # noqa: E402
import workdir_safety as ws  # noqa: E402


def _make_temp_base() -> tuple[Path, bool]:
    """Return an out-of-repo temporary base dir and whether it should be removed on cleanup."""
    env_root = os.environ.get("SEC3C_SCRATCH_ROOT", "").strip()
    if env_root:
        root = Path(env_root)
        root.mkdir(parents=True, exist_ok=True)
        d = Path(tempfile.mkdtemp(prefix="sec3c_case_", dir=root))
        return d, True
    d = Path(tempfile.mkdtemp(prefix="sec3c_case_"))
    return d, True


class TestC1WorkdirLifecycleAndGuards(unittest.TestCase):
    def setUp(self) -> None:
        self.base, _ = _make_temp_base()

    def tearDown(self) -> None:
        ws.remove_workdir_verified(self.base, retries=5, delay_s=0.2)

    def test_c1_refuses_scratch_inside_git_repo(self) -> None:
        in_repo_scratch = REPO_ROOT / "tools" / "bench" / "scratch_test_should_fail"
        with self.assertRaises(ValueError):
            ws.ensure_outside_git_repo(in_repo_scratch, label="--scratch-dir")
        with self.assertRaises(ValueError):
            ws.ScratchSession(in_repo_scratch)

        # Also detects a separate dummy git repo outside this checkout.
        fake_repo = self.base / "fake_git_repo"
        (fake_repo / ".git").mkdir(parents=True)
        nested = fake_repo / "sub" / "scratch"
        with self.assertRaises(ValueError):
            ws.ensure_outside_git_repo(nested, label="--scratch-dir")

        # Out-of-repo scratch dir is allowed.
        ok_scratch = self.base / "private_scratch"
        self.assertEqual(ws.ensure_outside_git_repo(ok_scratch, label="--scratch-dir"), ok_scratch)

    def test_c1_cli_scripts_refuse_in_repo_dirs(self) -> None:
        in_repo = REPO_ROOT / "tools" / "bench" / "_tmp_in_repo_guard"
        dummy = self.base / "dummy"
        dummy.mkdir(parents=True, exist_ok=True)
        dummy_exe = dummy / "noop.exe"
        dummy_exe.write_bytes(b"")
        out_json = self.base / "out.json"

        cases = [
            [
                sys.executable, "-I", str(BENCH_DIR / "bench_part1.py"),
                "--official-exe", str(dummy_exe), "--official-dir", str(dummy),
                "--rustgram-exe", str(dummy_exe), "--rustgram-dir", str(dummy),
                "--scratch-dir", str(in_repo), "--out-json", str(out_json),
            ],
            [
                sys.executable, "-I", str(BENCH_DIR / "bench_part2.py"), "probe",
                "--official-exe", str(dummy_exe), "--official-workdir", str(dummy),
                "--rustgram-exe", str(dummy_exe), "--rustgram-workdir", str(dummy),
                "--scratch-dir", str(in_repo),
            ],
            [
                sys.executable, "-I", str(BENCH_DIR / "bench_startup_fair.py"),
                "--official-exe", str(dummy_exe), "--official-workdir", str(dummy),
                "--rustgram-exe", str(dummy_exe), "--rustgram-workdir", str(dummy),
                "--scratch-dir", str(in_repo), "--out-json", str(out_json),
            ],
            [
                sys.executable, "-I", str(BENCH_DIR / "ab_chat_ops.py"), "inspect-delete-menu",
                "--rustgram-exe", str(dummy_exe), "--workdir-a", str(dummy),
                "--scratch-dir", str(in_repo), "--diag-dir", str(self.base / "diag"),
            ],
        ]
        for cmd in cases:
            res = subprocess.run(cmd, capture_output=True, text=True, check=False)
            self.assertNotEqual(res.returncode, 0, f"Expected non-zero exit for {cmd}")
            self.assertIn("git working tree", res.stderr.lower(), f"Missing git-tree refusal in stderr: {res.stderr}")
            self.assertFalse(in_repo.exists(), "Refused scratch dir must not be created")

    def test_c1_second_copy_failure_cleans_both_copies(self) -> None:
        src_a = self.base / "src_a"
        (src_a / "tdata").mkdir(parents=True)
        (src_a / "tdata" / "key_data").write_bytes(b"synthetic-auth-key-a")
        src_b_missing = self.base / "src_b_does_not_exist"
        scratch = self.base / "scratch"

        with self.assertRaises(FileNotFoundError):
            with ws.ScratchSession(scratch, post_stop_delay_s=0.0, rm_retries=3, rm_delay_s=0.1) as sess:
                wd_a = sess.copy_workdir(src_a, "copy_a")
                self.assertTrue((wd_a / "tdata" / "key_data").is_file())
                sess.copy_workdir(src_b_missing, "copy_b")

        self.assertFalse((scratch / "copy_a").exists(), "First workdir copy must be deleted when second copy fails")
        self.assertFalse((scratch / "copy_b").exists(), "Second workdir copy must not remain on disk")

    def test_c1_second_launch_failure_stops_first_process_and_cleans_both_copies(self) -> None:
        src_a = self.base / "src_a"
        src_b = self.base / "src_b"
        for s in (src_a, src_b):
            (s / "tdata").mkdir(parents=True)
            (s / "tdata" / "key_data").write_bytes(b"synthetic-auth-key")
        scratch = self.base / "scratch"
        layout_path = self.base / "layout.json"
        layout_path.write_text(json.dumps({"delivery_official": {}}), encoding="utf-8")

        args = type("Args", (), {
            "scratch_dir": scratch,
            "diag_dir": None,
            "layout_json": layout_path,
            "out_json": self.base / "deliv.json",
            "client": "official",
            "idle_max_cpu": 99.0,
            "idle_check_s": 0.1,
            "official_exe": Path(sys.executable),
            "rustgram_exe": Path(sys.executable),
            "official_workdir": src_a,
            "rustgram_workdir": src_a,
            "workdir_b": src_b,
            "api_creds_doc": None,
            "runs": 1,
        })()

        spawned_procs: list[subprocess.Popen[bytes]] = []
        call_count = [0]

        def fake_launch(_cl: str, _exe: Path, wd: Path, _doc: Path | None) -> tuple[subprocess.Popen[bytes], float]:
            call_count[0] += 1
            if call_count[0] == 1:
                # Hold a file open inside wd_a so cleanup only succeeds if process A is stopped first.
                code = (
                    "import sys, time; "
                    "f = open(sys.argv[1], 'wb'); f.write(b'lock'); f.flush(); "
                    "time.sleep(60)"
                )
                p = subprocess.Popen([sys.executable, "-I", "-c", code, str(wd / "held.lock")])
                spawned_procs.append(p)
                time.sleep(0.3)
                return p, time.perf_counter()
            raise RuntimeError("Simulated launch B failure")

        with mock.patch.object(bench_part2.wm, "check_system_idle", return_value={"passed": True, "mean_pct": 1.0, "max_pct": 2.0}):
            with mock.patch.object(bench_part2, "launch_client", side_effect=fake_launch):
                with self.assertRaisesRegex(RuntimeError, "Simulated launch B failure"):
                    bench_part2.cmd_delivery(args)

        self.assertEqual(len(spawned_procs), 1)
        self.assertIsNotNone(spawned_procs[0].poll(), "Process A must be terminated when launch B fails")
        self.assertFalse((scratch / "p2_deliv_official_a").exists(), "wd_a must be deleted after launch B failure")
        self.assertFalse((scratch / "p2_deliv_official_b").exists(), "wd_b must be deleted after launch B failure")

    def test_c1_kills_surviving_descendant_holding_file_lock(self) -> None:
        """Even when the root process exits cleanly, surviving children must be killed before rmtree."""
        src = self.base / "src"
        (src / "tdata").mkdir(parents=True)
        (src / "tdata" / "key_data").write_bytes(b"synthetic-key")
        scratch = self.base / "scratch"
        child_pid_file = self.base / "child.pid"

        # Root spawns a detached-like child holding a file inside wd, waits briefly, then exits 0 cleanly.
        parent_code = (
            "import subprocess, sys, time; "
            "child_code = \"import sys, time; f=open(sys.argv[1],'wb'); f.write(b'x'); f.flush(); time.sleep(60)\"; "
            "c = subprocess.Popen([sys.executable, '-I', '-c', child_code, sys.argv[1]]); "
            "open(sys.argv[2], 'w').write(str(c.pid)); "
            "time.sleep(0.4)"
        )
        with ws.ScratchSession(scratch, graceful_timeout_s=1.0, post_stop_delay_s=0.3, rm_retries=6, rm_delay_s=0.2) as sess:
            wd = sess.copy_workdir(src, "tree_wd")
            proc = subprocess.Popen(
                [sys.executable, "-I", "-c", parent_code, str(wd / "child_lock.bin"), str(child_pid_file)]
            )
            sess.track_process(proc)
            # Wait until child has written its PID and opened the lock file, and parent is still or just exiting.
            deadline = time.monotonic() + 5.0
            while time.monotonic() < deadline and not (child_pid_file.exists() and (wd / "child_lock.bin").exists()):
                time.sleep(0.05)
            self.assertTrue(child_pid_file.exists(), "Child PID file should have been created")
            # Let parent exit cleanly (exit code 0) while child stays alive holding child_lock.bin!
            proc.wait(timeout=5.0)
            self.assertEqual(proc.returncode, 0)

        self.assertFalse((scratch / "tree_wd").exists(), "Workdir must be removed after killing surviving child")

    def test_c1_remove_workdir_verified_raises_naming_leftover_path(self) -> None:
        leftover = self.base / "stubborn_wd"
        leftover.mkdir(parents=True)
        (leftover / "key_data").write_bytes(b"secret")

        with mock.patch.object(shutil, "rmtree", side_effect=PermissionError("locked")):
            with self.assertRaises(ws.WorkdirCleanupError) as ctx:
                ws.remove_workdir_verified(leftover, retries=2, delay_s=0.01)

        self.assertIn(str(leftover), str(ctx.exception))
        self.assertEqual(ctx.exception.paths, [leftover])

    def test_c1_copy_workdir_fails_closed_on_error(self) -> None:
        src = self.base / "src_fail"
        src.mkdir(parents=True)
        (src / "key_data").write_bytes(b"secret")
        dst = self.base / "dst_fail"

        real_open = open

        def failing_open(path: object, mode: str = "r", *a: object, **kw: object) -> object:
            if str(path) == str(src / "key_data") and "r" in mode:
                raise PermissionError("simulated read lock")
            return real_open(path, mode, *a, **kw)  # type: ignore[arg-type]

        with mock.patch.object(shutil, "copy2", side_effect=PermissionError("simulated copy2 lock")):
            with mock.patch("builtins.open", side_effect=failing_open):
                with self.assertRaises(PermissionError):
                    ws.copy_workdir_safely(src, dst)


def _sample_part1_json() -> dict[str, object]:
    stat = {"count": 1, "median": 100.0, "min": 100.0, "max": 100.0, "mean": 100.0, "raw": [100.0]}
    summary = {
        "first_visible_ms": stat,
        "login_ready_ms": stat,
        "idle_30s_private_mib": stat,
        "idle_30s_working_set_mib": stat,
        "idle_30s_cpu_pct": stat,
        "idle_5min_private_mib": stat,
        "idle_5min_working_set_mib": stat,
        "idle_5min_cpu_pct": stat,
    }
    sz = {"exe_mib": 10.0, "folder_mib": 20.0, "files": [{"name": "app.exe", "bytes": 10485760}]}
    return {
        "machine": {
            "cpu_model": "Synthetic CPU",
            "logical_processors": 8,
            "ram_gib": 32.0,
            "os": "Windows 11",
            "windows_display_version": "24H2",
            "windows_build": "26100.1",
        },
        "idle_checks": [{"passed": True, "mean_pct": 1.0, "median_pct": 1.0, "max_pct": 2.0, "duration_s": 30.0}],
        "sizes": {"official": sz, "rustgram": sz},
        "summary": {"official": summary, "rustgram": summary},
    }


class TestC3PrivacyReportAndGitignore(unittest.TestCase):
    def setUp(self) -> None:
        self.base, _ = _make_temp_base()

    def tearDown(self) -> None:
        ws.remove_workdir_verified(self.base, retries=5, delay_s=0.2)

    def test_c3_report_runs_privacy_verifier_and_passes_clean_data(self) -> None:
        p1_path = self.base / "part1.json"
        p1_path.write_text(json.dumps(_sample_part1_json()), encoding="utf-8")
        out_json = self.base / "results.json"
        out_md = self.base / "BENCHMARKS.md"

        res = subprocess.run(
            [
                sys.executable, "-B", "-I", str(BENCH_DIR / "report.py"),
                "--part1", str(p1_path),
                "--out-json", str(out_json),
                "--out-md", str(out_md),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(res.returncode, 0, f"report.py failed on clean input: stdout={res.stdout} stderr={res.stderr}")
        self.assertIn("[privacy-ok]", res.stdout)
        self.assertTrue(out_json.is_file())
        self.assertTrue(out_md.is_file())

    def test_c3_report_fails_and_removes_outputs_when_pii_or_path_leaks(self) -> None:
        p1_path = self.base / "part1.json"
        p1_path.write_text(json.dumps(_sample_part1_json()), encoding="utf-8")
        notes_path = self.base / "notes.json"
        leaked_path = "Z" + ":" + "\\" + "private" + "\\" + "secret"
        notes_path.write_text(json.dumps({"caveats": [f"Leaked path: {leaked_path}"]}), encoding="utf-8")
        out_json = self.base / "results_leaked.json"
        out_md = self.base / "BENCHMARKS_leaked.md"

        res = subprocess.run(
            [
                sys.executable, "-B", "-I", str(BENCH_DIR / "report.py"),
                "--part1", str(p1_path),
                "--notes-json", str(notes_path),
                "--out-json", str(out_json),
                "--out-md", str(out_md),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(res.returncode, 0, "report.py must fail when privacy check catches a leaked path")
        self.assertIn("[privacy-fail]", res.stderr)
        self.assertFalse(out_json.exists(), "Unsafe out-json must be removed on privacy failure")
        self.assertFalse(out_md.exists(), "Unsafe out-md must be removed on privacy failure")

    def test_c3_gitignore_ignores_tool_output_folders(self) -> None:
        sample_paths = [
            "tools/bench/out/results.json",
            "tools/uiparity/out/metrics.json",
            "tools/bench/scratch/copy_a/tdata/key_data",
            "tools/uiparity/snaps/13_settings_main_1440x900_official.png",
            "tools/bench/diag/probe.png",
            "tools/uiparity/pairs/01_pair.png",
        ]
        for rel in sample_paths:
            res = subprocess.run(
                ["git", "check-ignore", "-q", rel],
                cwd=str(REPO_ROOT),
                check=False,
            )
            self.assertEqual(res.returncode, 0, f"Expected .gitignore to ignore {rel}")


class TestC4AbChatOpsCleanupSafety(unittest.TestCase):
    def setUp(self) -> None:
        self.base, _ = _make_temp_base()

    def tearDown(self) -> None:
        ws.remove_workdir_verified(self.base, retries=5, delay_s=0.2)

    def test_c4_cleanup_refuses_without_target_title(self) -> None:
        dummy_exe = self.base / "noop.exe"
        dummy_exe.write_bytes(b"")
        dummy_wd = self.base / "wd_a"
        dummy_wd.mkdir(parents=True)
        scratch = self.base / "scratch"
        env = os.environ.copy()
        env.pop("BENCH_TARGET_TITLE", None)

        res = subprocess.run(
            [
                sys.executable, "-B", "-I", str(BENCH_DIR / "ab_chat_ops.py"),
                "cleanup",
                "--rustgram-exe", str(dummy_exe),
                "--workdir-a", str(dummy_wd),
                "--scratch-dir", str(scratch),
                "--confirm-xy", "670", "470",
            ],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        self.assertEqual(res.returncode, 2)
        self.assertIn("--target-title", res.stderr)

    def test_c4_reads_target_link_from_file_or_env_never_cli_or_argv(self) -> None:
        # Raw --target-link CLI flag must not exist.
        with self.assertRaises(SystemExit):
            ab_chat_ops.parse_args([
                "seed",
                "--rustgram-exe", str(self.base / "noop.exe"),
                "--workdir-a", str(self.base / "wd"),
                "--scratch-dir", str(self.base / "scratch"),
                "--target-link", "https://t.me/bench_peer_b",
            ])

        # Reading from file takes precedence over env and strips comments/blanks.
        link_file = self.base / "target_link.txt"
        link_file.write_text("# comment\n\nhttps://t.me/bench_peer_b\n", encoding="utf-8")
        self.assertEqual(ab_chat_ops.load_target_link(link_file), "https://t.me/bench_peer_b")
        self.assertEqual(ab_chat_ops.extract_search_query("https://t.me/bench_peer_b"), "@bench_peer_b")
        self.assertEqual(ab_chat_ops.extract_search_query("tg://resolve?domain=bench_peer_b"), "@bench_peer_b")

        # Reading from BENCH_TARGET_LINK env var works when no file is given.
        with mock.patch.dict(os.environ, {"BENCH_TARGET_LINK": "tg://resolve?domain=peer_env"}, clear=False):
            os.environ.pop("BENCH_TARGET_LINK_FILE", None)
            self.assertEqual(ab_chat_ops.load_target_link(None), "tg://resolve?domain=peer_env")

        # Rustgram launch command must NEVER include the target link on argv.
        cmd = ab_chat_ops.build_rustgram_cmd(self.base / "rustgram.exe", self.base / "wd_copy")
        self.assertFalse(any("t.me" in a or "tg://" in a or "bench_peer" in a for a in cmd))

    def test_c4_cleanup_stops_if_target_chat_title_not_found(self) -> None:
        import numpy as np

        blank = np.full((800, 1200, 3), 240, dtype=np.uint8)

        def fake_ocr(_crop: object) -> list[ab_chat_ops.OcrLine]:
            return [ab_chat_ops.OcrLine("Pinned Personal Chat", (10, 40, 180, 20))]

        with mock.patch.object(ab_chat_ops.wm, "capture_window_printwindow", return_value=blank):
            with mock.patch.object(ab_chat_ops.wm, "post_right_click") as mock_rclick:
                with mock.patch.object(ab_chat_ops.wm, "post_click") as mock_click:
                    rc = ab_chat_ops.run_cleanup_on_window(
                        0x1234,
                        target_title="Bench Account B",
                        target_link="",
                        menu_delete_xy=(280, 300),
                        menu_delete_offset=(60, 180),
                        confirm_xy=(670, 470),
                        revoke_checkbox_xy=None,
                        ocr_reader=fake_ocr,
                        sleep_fn=lambda _s: None,
                    )
                    self.assertEqual(rc, 3)
                    mock_rclick.assert_not_called()
                    mock_click.assert_not_called()

    def test_c4_cleanup_stops_if_confirmation_box_missing(self) -> None:
        import numpy as np

        base_frame = np.full((800, 1200, 3), 240, dtype=np.uint8)
        # Same frame before and after clicking delete -> no DeleteMessagesBox appeared!
        frames = [base_frame, base_frame, base_frame, base_frame]
        idx = [0]

        def next_frame(_hwnd: int) -> np.ndarray:
            f = frames[min(idx[0], len(frames) - 1)]
            idx[0] += 1
            return f

        def fake_ocr(_crop: object) -> list[ab_chat_ops.OcrLine]:
            return [ab_chat_ops.OcrLine("Bench Account B", (20, 140, 140, 20))]

        clicks: list[tuple[int, int]] = []

        def record_click(_hwnd: int, x: int, y: int) -> float:
            clicks.append((x, y))
            return 0.0

        with mock.patch.object(ab_chat_ops.wm, "capture_window_printwindow", side_effect=next_frame):
            with mock.patch.object(ab_chat_ops.wm, "post_click", side_effect=record_click):
                with mock.patch.object(ab_chat_ops.wm, "post_right_click") as mock_rclick:
                    with mock.patch.object(ab_chat_ops.wm, "post_vk") as mock_vk:
                        rc = ab_chat_ops.run_cleanup_on_window(
                            0x1234,
                            target_title="Bench Account B",
                            target_link="",
                            menu_delete_xy=(280, 380),
                            menu_delete_offset=(60, 180),
                            confirm_xy=(670, 470),
                            revoke_checkbox_xy=(520, 410),
                            ocr_reader=fake_ocr,
                            sleep_fn=lambda _s: None,
                        )
                        self.assertEqual(rc, 3)
                        mock_rclick.assert_called_once()
                        self.assertNotIn((670, 470), clicks, "Must NOT click confirm_xy when modal is missing")
                        self.assertNotIn((520, 410), clicks, "Must NOT click revoke checkbox when modal is missing")
                        mock_vk.assert_called_once_with(0x1234, ab_chat_ops.VK_ESCAPE)

    def test_c4_cleanup_succeeds_when_title_header_and_confirmation_box_match(self) -> None:
        import numpy as np

        base_frame = np.full((800, 1200, 3), 240, dtype=np.uint8)
        modal_frame = base_frame.copy()
        # Simulate dimmed background + centered DeleteMessagesBox modal.
        modal_frame[:, :, :] = 120
        modal_frame[220:580, 380:820, :] = 255

        # Capture sequence: 0=list scan, 1=header check, 2=before delete click, 3=after delete click (modal)
        frames = [base_frame, base_frame, base_frame, modal_frame]
        cap_idx = [0]

        def next_frame(_hwnd: int) -> np.ndarray:
            f = frames[min(cap_idx[0], len(frames) - 1)]
            cap_idx[0] += 1
            return f

        ocr_calls = [0]

        def fake_ocr(_crop: object) -> list[ab_chat_ops.OcrLine]:
            ocr_calls[0] += 1
            if ocr_calls[0] == 1:
                # Row 2 (not row 0!) holds the target chat.
                return [
                    ab_chat_ops.OcrLine("Other Chat", (20, 40, 100, 20)),
                    ab_chat_ops.OcrLine("Bench Account B", (20, 200, 140, 20)),
                ]
            if ocr_calls[0] == 2:
                return [ab_chat_ops.OcrLine("Bench Account B", (10, 15, 140, 22))]
            return [
                ab_chat_ops.OcrLine("Are you sure you want to delete this chat?", (30, 50, 320, 24)),
                ab_chat_ops.OcrLine("Cancel  Delete", (200, 300, 180, 24)),
            ]

        clicks: list[tuple[int, int]] = []
        rclicks: list[tuple[int, int]] = []

        with mock.patch.object(ab_chat_ops.wm, "capture_window_printwindow", side_effect=next_frame):
            with mock.patch.object(ab_chat_ops.wm, "post_click", side_effect=lambda _h, x, y: clicks.append((x, y)) or 0.0):
                with mock.patch.object(ab_chat_ops.wm, "post_right_click", side_effect=lambda _h, x, y: rclicks.append((x, y)) or 0.0):
                    rc = ab_chat_ops.run_cleanup_on_window(
                        0x1234,
                        target_title="Bench Account B",
                        target_link="",
                        menu_delete_xy=None,
                        menu_delete_offset=(60, 180),
                        confirm_xy=(670, 470),
                        revoke_checkbox_xy=(520, 410),
                        ocr_reader=fake_ocr,
                        sleep_fn=lambda _s: None,
                    )
        self.assertEqual(rc, 0)
        # Matched row center: list_rect=(72, 70, ...) + bbox=(20, 200, 140, 20) -> (72+20+70, 70+200+10) = (162, 280)
        self.assertEqual(rclicks, [(162, 280)], "Must right-click the matched row, not fixed row 0")
        self.assertEqual(clicks, [(162, 280), (222, 460), (520, 410), (670, 470)])


if __name__ == "__main__":
    unittest.main()
