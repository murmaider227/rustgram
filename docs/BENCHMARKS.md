# Benchmarks: Rustgram and Telegram Desktop

Measured on 2026-11-26 on one Windows PC, with the scripts in `tools/bench`. Every number below
comes from running the programs; nothing is estimated. Telegram Desktop's no-account numbers
were measured on 2026-11-25 with the same version and scripts (see "Results").

## Versions and machine

- **Telegram Desktop 7.4.6** (official portable build, Qt 6, Direct3D 11 renderer).
- **Rustgram 0.1.0-alpha.2**, portable release build (egui/eframe 0.37, OpenGL renderer),
  started with `--live -testmode`.
- **Machine:** AMD Ryzen 7 9700X (8 cores, 16 threads), 31.1 GiB RAM, Windows 11 Home 26H2,
  build 26300.9974.

Both clients used Telegram's **test servers** and the same throwaway test account. Its chat list
holds two chats: Saved Messages, with no messages, and a bot chat with a single service message.
Rustgram also shows a "Security Check" suggestion above the list; Telegram Desktop 7.4.6 does
not show it for this account.

## Results

Medians of 5 runs; raw runs are listed further down.

| Metric | Telegram Desktop 7.4.6 | Rustgram 0.1.0-alpha.2 | |
|---|---:|---:|---|
| Start → first window visible (test account) | 771 ms | **368 ms** | 2.1× faster |
| Start → chat list drawn and stable (test account) | 1568 ms | **847 ms** | 1.85× faster |
| Open a chat → history stable | 217 ms | **38 ms** | 5.7× faster |
| Private bytes after start (test account) | 311.3 MiB | **117.7 MiB** | 2.6× less |
| Working set after start (test account) | 285.8 MiB | **140.5 MiB** | 2.0× less |
| Start → first window visible (no account) | 1232 ms¹ | **272 ms** | 4.5× faster |
| Idle private bytes at 30 s (no account) | 288.1 MiB¹ | **92.8 MiB** | 3.1× less |
| Idle private bytes at 5 min (no account) | 287.4 MiB¹ | **91.9 MiB** | 3.1× less |
| Main executable size | 246.5 MiB | **148.1 MiB** | 1.7× smaller |
| Install folder size | **264.7 MiB** | 300.2 MiB | 1.13× larger |

¹ Measured on 2026-11-25 with the same Telegram Desktop build and scripts. Without an account,
Telegram Desktop connects to Telegram's production servers, and this project's test rules allow
only the test servers, so this session did not repeat those runs.

The test-account rows (the first five) come from one session in which the two clients took
turns, run by run. Rustgram's no-account rows come from a second session the same day with the
same scripts. Rustgram's install folder holds its executable, the shared FFmpeg libraries, the
updater and the widget helper; Telegram Desktop's holds its executable, its widget helper and
the Direct3D shader compiler.

### What the first window shows

Both clients show their main window first and fill in the chat list when the server answers.
Rustgram copies Telegram Desktop's order here: the window appears with the chat list's
"Loading..." text and "Select a chat to start messaging" on the right, and no chat is opened.
Telegram Desktop needed about 800 ms from its first window to the loaded list in these runs,
Rustgram about 480 ms. Most of the time before the list appears is the connection itself:
both clients create and bind a new temporary key at every start before their first request.
In Rustgram's own traces of these servers on 2026-11-25, about 650 ms passed between sending its
first request and receiving the answer.

### Rustgram builds

| Start → first window / chat list (test account) | Median | Raw runs (ms) |
|---|---:|---|
| Earlier build (window shown only after the chat list loaded) | 1110 ms / 1110 ms | 1409.9, 1007.8, 1110.1, 1002.5, 1166.5 |
| Build of 2026-11-25 | 560 ms / 849 ms | window 829.2, 560.0, 562.1, 547.3, 556.5; list 1060.7, 734.5, 954.0, 849.0, 730.1 |
| 0.1.0-alpha.2 (this measurement) | 368 ms / 847 ms | see below |

The build of 2026-11-25 differs from the earlier one in five ways:

- It shows the window before the network answers, as Telegram Desktop does.
- It opens no chat at start.
- It no longer waits on the session store while the connection is being set up.
- It sends the startup requests (updates state, chat list, pinned chats) in one round trip and
  takes the account's own user from local storage instead of asking the server.
- It requests an opened chat's bot info and pinned messages at the same time.

Compared with the build of 2026-11-25, 0.1.0-alpha.2 shows its window about 200 ms sooner, with
and without an account (192 and 203 ms), while the time to a loaded chat list is unchanged (847 ms against
849 ms). Its private bytes after start fell from 133.3 to 117.7 MiB with the test account and
from 110.9 to 92.8 MiB without an account. We have not traced which changes caused these
differences. Its executable grew from 130.4 to 148.1 MiB; between the two builds the code base
gained about 32,000 lines of Rust outside the tests for new features, while embedded assets grew
by less than 0.4 MiB.

## Smoothness (Rustgram only)

These numbers come from Rustgram's own frame trace. Telegram Desktop has no equivalent trace, so
there is no comparison with it here.

`tools/bench/frame_bench.py` runs Rustgram's offline demo (`--demo`, no network) with 600 extra
chats and a 2500-message chat, in a 1200×800 window, and feeds scripted wheel, pointer and key
input straight into the interface, so every run repeats exactly. Each scenario ran 5 times with
each build:

- **chatlist:** scroll the 600-chat list down and up with the wheel.
- **history:** scroll the 2500-message history (photos, replies, reactions).
- **openchats:** open chats one after another, 10 times.
- **panels:** open the emoji, sticker and GIF panels and scroll each.

"CPU frame time" is eframe's whole-frame CPU time (input, layout, painting, GPU submit), without
the wait for vsync. The numbers are pooled over the 5 runs. "Earlier build" is a build from the
early morning of 2026-11-26, before the latest smoothness work; "0.1.0-alpha.2" is the release
build measured above. The two builds ran one after the other on the idle PC; no compiler ran
during them, and every run's trace was written by a single process.

| Scenario | Frames (earlier / alpha.2) | p50 ms | p95 ms | Max ms | Frames over 16.7 ms |
|---|---:|---:|---:|---:|---:|
| chatlist | 23997 / 24040 | 1.97 → **0.77** | 2.62 → **1.18** | 20.1 → **8.0** | 1 → **0** |
| history | 21482 / 21635 | 2.02 → **0.83** | 2.85 → **1.30** | 114.7 → **8.3** | 5 → **0** |
| openchats | 18157 / 18226 | 1.88 → **0.66** | 2.45 → **1.04** | 87.3 → **11.6** | 20 → **0** |
| panels | 22391 / 22672 | 2.20 → **0.89** | 3.27 → **2.01** | 171.9 → **11.0** | 28 → **0** |

A frame over 16.7 ms misses a 60 Hz refresh. With 0.1.0-alpha.2 no frame in any of these runs
took longer than 11.6 ms. The p95 of single runs stayed within 1.16–1.20 ms (chatlist),
1.27–1.32 ms (history), 1.01–1.04 ms (openchats) and 1.94–2.15 ms (panels).

These are demo-mode numbers on local data. Scrolling on the test servers was not measured (see
"Not measured").

## Raw runs

### Test account (this session)

| Metric | Telegram Desktop (raw) | Rustgram (raw) |
|---|---|---|
| First window visible (ms) | 770.6, 776.7, 765.4, 773.8, 768.9 | 361.9, 358.5, 375.7, 367.7, 379.7 |
| Chat list drawn and stable (ms) | 1472.2, 1630.5, 1525.5, 1568.0, 1581.7 | 819.6, 847.2, 848.0, 760.2, 874.1 |
| Open chat → history stable (ms) | 208.8, 217.2, 236.9, 224.0, 210.7 | 36.1, 38.3, 38.6, 47.7, 36.7 |
| Open chat → first change (ms) | 40.4, 45.5, 38.8, 38.8, 42.4 | 0.0, 0.0, 0.0, 0.0, 0.0 (see notes) |
| Private bytes after start (MiB) | 310.9, 311.5, 310.9, 311.3, 312.3 | 116.8, 119.7, 118.5, 117.7, 117.7 |
| Working set after start (MiB) | 285.1, 285.8, 285.9, 285.8, 285.9 | 136.6, 140.6, 140.7, 140.5, 140.5 |

Telegram Desktop's captures held exactly two distinct pictures in every run: the window with the
loading state, then the loaded list. Rustgram's held the same two pictures in three runs; in
runs 2 and 3 the window changed once more, about 37 ms after the list appeared, and the ready
time counts that last change.

### No account

| Metric | Telegram Desktop (raw, 2026-11-25) | Rustgram (raw, this session) |
|---|---|---|
| First window visible (ms) | 1251.1, 1227.0, 1213.9, 1232.1, 1259.0 | 308.8, 270.6, 300.9, 271.7, 264.9 |
| Idle private bytes at 30 s (MiB) | 287.4, 289.4, 287.8, 289.1, 288.1 | 93.0, 92.5, 92.8, 91.9, 93.8 |
| Idle working set at 30 s (MiB) | 284.5, 284.8, 284.5, 284.0, 283.9 | 102.2, 102.1, 102.0, 101.8, 102.1 |
| Idle private bytes at 5 min (MiB) | 286.6, 288.7, 287.0, 288.3, 287.4 | 91.6, 92.5, 91.4, 91.9, 92.5 |
| Idle working set at 5 min (MiB) | 284.2, 284.6, 284.2, 283.7, 283.6 | 102.2, 102.1, 101.9, 101.8, 102.0 |

## Method

- **Idle PC.** Before each session the scripts check that total CPU stays below 15 % for 30 s
  and that every disk is at least 80 % idle. On 2026-11-25, Windows servicing work on the system
  disk slowed both clients' start by several seconds while CPU stayed low; runs taken then were
  discarded, and the disk check was added.
- **Fresh state.** Each run starts the client on a fresh copy of its data folder; the copy is
  deleted afterwards. The client is closed by its process id. Telegram Desktop's copies came from
  a snapshot of the test account's data in which two media-cache index files were missing (another
  running copy of the client held them locked), so its media cache started empty. The account's
  chats use letter avatars, and every run showed the same two pictures, so nothing in the
  measured window waited on that cache.
- **Interactive desktop.** Window contents are captured with `PrintWindow`. Telegram Desktop's
  Direct3D 11 window is captured as a blank rectangle unless it runs on the desktop that
  receives user input, so all test-account runs ran on the logged-on user's desktop.
  `tools/bench/README.md` explains how to start the script there from another session (a Task
  Scheduler task with "run only when the user is logged on").
- **First window visible:** time from process start until a visible top-level window of at least
  300×200 px exists.
- **Chat list drawn and stable:** the window is captured continuously and every capture that
  differs from the previous one (more than 0.1 % of pixels changed) is kept, until the window
  has not changed for 3 s. The ready time is the first capture from which every later capture
  matches the final picture. A loading state that stays still for a while is therefore not
  counted as ready, for either client.
- **Open a chat:** both windows are moved to the same 1200×800 size and position. The mouse
  pointer is placed on the bot chat's row with a posted mouse message and the screen is left to
  settle. A click is then posted to the window (the real mouse cursor is not moved). "First
  change" is the first capture that differs from the one before the click. "History stable" is
  the last change before the window stays unchanged for 500 ms.
- **Memory:** `PrivateUsage` and `WorkingSetSize` of the process tree once the chat list has been
  stable for 3 s (test account), or 30 s and 5 min after start (no account). Each client ran as
  a single process.
- **Sizes:** file sizes of the main executable and of every file in the install folder.

## Notes and limits

- The chat that was opened has one message, which both clients already know from the chat list.
  Rustgram paints the history from it at once; its history request returns later without
  changing the picture. Telegram Desktop paints the history after its own request returns.
- Rustgram's "first change" after the click is 0 ms because `PrintWindow` returned only after
  Rustgram had handled the posted click and repainted. It is shown for completeness. "History
  stable" is the comparable number.
- In Rustgram's traces of 2026-11-25, its interface thread still waited after the click for two
  requests in turn (marking the chat read, then loading history), about 30 ms each on these
  servers. The screen does not change during that time.
- At the start of the test-account runs each client used its own window size: 1456×909 for
  Telegram Desktop (restored from its settings), 1024×768 for Rustgram. The memory after start
  was taken at those sizes.
- Every run started from the same saved data, so neither client had the opened chat's history
  from an earlier run.

## Not measured

- Scrolling speed and frame pacing in a long chat on the test servers, for either client. The
  test account has no long chat, and the test-server rules for this project allow messages only
  between two test accounts and require deleting them afterwards. Rustgram's frame times in its
  offline demo are under "Smoothness"; Telegram Desktop has no equivalent trace.
- Memory after opening many chats, for the same reason (the account has two chats).
- Message delivery between two Telegram Desktop instances.
- Login-screen readiness of Telegram Desktop without an account (the no-account session of
  2026-11-25 ran on a desktop where its window could not be captured).
- Telegram Desktop without an account on 2026-11-26 (see "Results").
