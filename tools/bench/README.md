# Rustgram vs Telegram Desktop Benchmark Scripts

Reproducible Windows benchmark harness comparing official Telegram Desktop against Rustgram.
No machine-specific or local paths are hardcoded in these scripts; supply all executable,
working-directory, and output paths via CLI flags or environment variables.

## Scripts

- `build_dist.py` — Generates an ephemeral Ed25519 keypair in memory (never printed or committed)
  and runs `cargo run -p tg-xtask -- dist` to build release binaries and stage the portable
  `Rustgram/` distribution folder with FFmpeg DLLs.
- `win_metrics.py` — Shared Win32 measurement library (`GetSystemTimes`, `GetProcessTimes`,
  `GetProcessMemoryInfo`, `EnumWindows`, `PrintWindow`, `BitBlt`, `SendInput`).
- `bench_part1.py` — Part 1 (no account): binary/folder size, cold start (first window visible
  and 500 ms stable login screen via `PrintWindow`), and idle memory & CPU at 30 s and 5 min.
- `bench_part2.py` — Part 2 (test account): `probe` (layout discovery), `cold-start` (start to
  chat list loaded), `interactive` (open chat latency, 10 s wheel scroll CPU & frame pacing,
  memory after opening chats), and `delivery` (A → B message delivery latency).
- `report.py` — Merges Part 1 and Part 2 JSON outputs into `results.json` and `BENCHMARKS.md`,
  and runs `verify_report_privacy.py` on every file it writes (deleting unsafe outputs on failure).
- `workdir_safety.py` — Shared `ScratchSession` lifecycle guard: refuses scratch/diag directories
  inside any git working tree, terminates full process trees via Win32 Job Objects + descendant
  snapshots, retries `rmtree` and exits non-zero naming any leftover directory.
- `ab_chat_ops.py` — Seeds and cleans up A↔B scroll-benchmark messages. Reads `BENCH_TARGET_LINK`
  from `--target-link-file` / `BENCH_TARGET_LINK_FILE` or env (never passed on CLI or client argv),
  locates the target chat by `--target-title` and verifies the open header, and verifies the
  `DeleteMessagesBox` confirmation dialog before confirming deletion.
- `bench_startup_fair.py` — Startup and open-chat comparison that measures both clients the same
  way (test account): start → first window visible, start → chat list drawn and stable, open a
  chat → history stable. See "Interactive desktop" below.

- `frame_bench.py` — Frame-time benchmark (smoothness). Runs scripted scenarios (scroll a 600-chat
  list, scroll a 2500-message history, open 10 chats, open and scroll the emoji/sticker/GIF panels)
  5 times each plus one stack-sampled run, and reports p50/p95/p99/max CPU frame time, frames over
  16.7 ms and 33 ms, and the functions behind the slow frames. See "Frame-time benchmark" below.

## Frame-time benchmark

The app side is env-gated and off by default (`crates/tg-app/src/startup/frame_trace`):

- `RUSTGRAM_FRAME_TRACE=1` writes one line per frame (UI time and eframe's whole-frame CPU time,
  vsync wait excluded) to `RUSTGRAM_FRAME_TRACE_OUT`; `=sample` also samples the UI thread's stack
  every millisecond inside script sections (Windows x64).
- `RUSTGRAM_FRAME_SCRIPT=<file>` feeds wheel, pointer and key events straight into egui, so runs
  repeat exactly and never move the real cursor. Commands are listed in `frame_trace/script.rs`.
- `RUSTGRAM_BENCH_DATA=1` (demo only) adds 600 chats and a 2500-message chat ("Bench History")
  with photos, replies and reactions to the offline demo.

Build with line tables so the samples resolve to functions, then run:

```
CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release -p tg-app --bin rustgram
python -I frame_bench.py run --exe <target>/release/rustgram.exe --scratch-dir <dir outside git> \
    --out-dir <dir> --ffmpeg-bin <ffmpeg bin dir>
# test servers, scratch copy of a test-account workdir (deleted after each run):
python -I frame_bench.py run ... --live-workdir <test workdir> --api-creds-doc <tdesktop>/docs/api_credentials.md \
    --layout frame_bench_layout_testdc.json --startup-wait-ms 12000
python -I frame_bench.py analyze --out-dir <dir>     # re-analyze saved traces
python -I frame_bench.py probe ...                   # screenshots to recalibrate click positions
```

Click positions (`LAYOUT` in the script, overridable with `--layout <json>`) are logical points in a
1200x800 window. Symbolization needs a dbghelp.dll that reads rust-lld PDBs; the script picks the
Visual Studio or Windows Kits copy (the System32 one loads no symbols), or `FRAME_BENCH_DBGHELP`.
Each run records system CPU load and running `rustc` processes, since other builds skew the numbers.

## Interactive desktop (needed for Telegram Desktop captures)

`PrintWindow` returns a white rectangle for Telegram Desktop's Direct3D 11 window unless the
client runs on the desktop that receives user input (`WinSta0\Default` in the logged-on user's
console session). `bench_startup_fair.py` checks this first and exits with code 3 elsewhere.

- A shell that already runs on that desktop (the process's thread desktop equals
  `OpenInputDesktop`) can start the script directly.
- From a shell on another desktop or session, start it through a Task Scheduler task with the
  "run only when the user is logged on" option, which runs in the user's interactive session:
  `schtasks /Create /TN <name> /TR "<python> -I <script> <args>" /SC ONCE /ST 23:59 /IT /F`,
  then `schtasks /Run /TN <name>`, wait for its output file, and `schtasks /Delete /TN <name> /F`.

The clients' windows appear on the user's screen during the runs. Clicks are posted to the
window (`PostMessageW`), so the user's mouse is not moved.

`chat_list_ready_ms` keeps every capture that differs from the previous one, records until the
window has not changed for `--settle-s` seconds, and reports the first capture from which the
window matches its settled state. A loading state that stays still for a while is therefore not
counted as ready. Run it only on an idle PC: the script checks CPU (< 15 %) and the idle time of
every disk (`--disk-idle-min`, default 80 %), because disk-bound background work such as Windows
servicing slows the start of both clients by seconds.

## Requirements

- Windows 10/11 (x64)
- Python 3.11+ with `numpy` and `Pillow` (`python -I` compatible)
- For Rustgram live mode (`--rustgram-live` / Part 2): `RUSTGRAM_API_ID` and `RUSTGRAM_API_HASH`
  in the environment (or `--api-creds-doc` pointing to tdesktop's `docs/api_credentials.md`).

## Memory diagnosis (where private bytes go)

- `mem_walk.py` — read-only VMMap-style walk of a running process (`VirtualQueryEx`, TEB stacks, PEB heap
  list, `QueryWorkingSetEx`). Splits committed memory into thread stacks, NT heaps, writable image pages,
  mapped views and other private `VirtualAlloc` regions, with the resident part of each.
  `python -I mem_walk.py --pid <pid> --out-json walk.json`. Heaps that are not in the PEB list (segment
  heaps created by system or driver DLLs) are counted under "other VirtualAlloc".
- `memdiag/` — a small standalone `cdylib` (own `[workspace]`, build it with
  `cargo build --release --manifest-path tools/bench/memdiag/Cargo.toml`). Loaded into a measurement build, it
  hooks `NtAllocateVirtualMemory(/Ex)`, `NtFreeVirtualMemory` and the single-argument `D3DKMT*` syscalls in
  `win32u.dll`, records each call with its call stack, and measures the private-bytes delta of every D3DKMT
  call (GPU drivers commit memory from the kernel side, which no user-mode allocation hook sees). It costs
  about 100 MiB of commit for its own tables; `mem_attribution.py` subtracts them.
  To load it, a throwaway build calls `LoadLibraryW(<dll>)` and the exported `memdiag_start()` as the first
  statement of `main`, with `RG_MEMDIAG_OUT=<folder>` in the environment. Creating `<folder>/va.trigger`
  makes it write `events.bin`, `stacks.tsv`, `frames.tsv`, `heaps.txt` (a `HeapWalk` summary) and `va.done`.
  Build the app with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and keep the `.pdb` next to the exe so
  frames resolve to `file:line`. Never ship a build that loads it.
- `mem_attribution.py` — joins `walk.json` (taken just before the trigger) with the `memdiag` dump:
  `python -I mem_attribution.py --dir <folder>` prints MiB per memory type, owner module and phase.
- Heap allocation sites: use `dhat` as the global allocator in the same throwaway build and sort its
  `dhat-heap.json` by bytes live at the end.
