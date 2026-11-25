# Benchmarks: Rustgram and Telegram Desktop

Measured on 2026-11-25 on one Windows PC, with the scripts in `tools/bench`. Every number below
comes from running both programs; nothing is estimated.

## Versions and machine

- **Telegram Desktop 7.4.6** (official portable build, Qt 6, Direct3D 11 renderer).
- **Rustgram 0.1.0**, release build of 2026-11-25 (egui/eframe 0.37, OpenGL renderer), started
  with `--live -testmode`.
- **Machine:** AMD Ryzen 7 9700X (8 cores, 16 threads), 31.1 GiB RAM, Windows 11 Home 26H2,
  build 26300.9974.

Both clients used Telegram's **test servers** and the same throwaway test account. Its chat list
holds one chat: a bot chat with a single service message.

## Results

Medians of 5 runs; raw runs are listed further down.

| Metric | Telegram Desktop 7.4.6 | Rustgram | |
|---|---:|---:|---|
| Start → first window visible (test account) | 729 ms | **560 ms** | 1.30× faster |
| Start → chat list drawn and stable (test account) | 1484 ms | **849 ms** | 1.75× faster |
| Open a chat → history stable | 217 ms | **34 ms** | 6.3× faster |
| Private bytes after start (test account) | 311.7 MiB | **133.3 MiB** | 2.3× less |
| Working set after start (test account) | 297.4 MiB | **147.8 MiB** | 2.0× less |
| Start → first window visible (no account) | 1232 ms | **475 ms** | 2.6× faster |
| Idle private bytes at 30 s (no account) | 288.1 MiB | **110.9 MiB** | 2.6× less |
| Idle private bytes at 5 min (no account) | 287.4 MiB | **109.7 MiB** | 2.6× less |
| Main executable size | 246.5 MiB | **130.4 MiB** | 1.9× smaller |

The test-account rows (the first five) come from one session in which the two clients took
turns, run by run. The no-account rows come from an earlier session the same day with the same
scripts. That session measured Telegram Desktop first and Rustgram later, after its start-up
work ("Rustgram builds" below).

### What the first window shows

Both clients show their main window first and fill in the chat list when the server answers.
Rustgram copies Telegram Desktop's order here: the window appears with the chat list's
"Loading..." text and "Select a chat to start messaging" on the right, and no chat is opened.
Telegram Desktop needed about 750 ms from its first window to the loaded list in these runs,
Rustgram about 290 ms. Most of the time before the list appears is the connection itself:
both clients create and bind a new temporary key at every start before their first request.
In Rustgram's own traces of these servers, about 650 ms passed between sending its first
request and receiving the answer.

### Rustgram builds

| Start → first window / chat list (test account) | Median | Raw runs (ms) |
|---|---:|---|
| Earlier build (window shown only after the chat list loaded) | 1110 ms / 1110 ms | 1409.9, 1007.8, 1110.1, 1002.5, 1166.5 |
| Current build | 560 ms / 849 ms | see below |

The current build differs from the earlier one in five ways:

- It shows the window before the network answers, as Telegram Desktop does.
- It opens no chat at start.
- It no longer waits on the session store while the connection is being set up.
- It sends the startup requests (updates state, chat list, pinned chats) in one round trip and
  takes the account's own user from local storage instead of asking the server.
- It requests an opened chat's bot info and pinned messages at the same time.

## Raw runs

### Test account (this session)

| Metric | Telegram Desktop (raw) | Rustgram (raw) |
|---|---|---|
| First window visible (ms) | 1002.7, 723.1, 729.1, 727.5, 736.4 | 829.2, 560.0, 562.1, 547.3, 556.5 |
| Chat list drawn and stable (ms) | 1945.3, 1512.8, 1484.1, 1448.4, 1482.5 | 1060.7, 734.5, 954.0, 849.0, 730.1 |
| Open chat → history stable (ms) | 224.5, 237.4, 210.9, 217.0, 216.5 | 33.8, 33.9, 34.7, 34.3, 35.2 |
| Open chat → first change (ms) | 34.9, 35.7, 36.6, 36.5, 36.4 | 0.0, 0.0, 0.0, 0.0, 0.0 (see notes) |
| Private bytes after start (MiB) | 312.3, 312.1, 311.4, 311.3, 311.7 | 132.6, 132.7, 133.8, 134.1, 133.3 |
| Working set after start (MiB) | 297.7, 297.4, 296.9, 297.8, 297.2 | 147.6, 147.1, 149.0, 148.7, 147.8 |

Each run's captures held exactly two distinct pictures for both clients: the window with the
loading state, then the loaded list.

### No account (earlier session the same day)

| Metric | Telegram Desktop (raw) | Rustgram (raw) |
|---|---|---|
| First window visible (ms) | 1251.1, 1227.0, 1213.9, 1232.1, 1259.0 | 446.0, 462.9, 667.9, 475.3, 510.9 |
| Idle private bytes at 30 s (MiB) | 287.4, 289.4, 287.8, 289.1, 288.1 | 110.5, 110.9, 110.3, 111.1, 111.4 |
| Idle working set at 30 s (MiB) | 284.5, 284.8, 284.5, 284.0, 283.9 | 123.3, 123.1, 123.1, 123.3, 123.1 |
| Idle private bytes at 5 min (MiB) | 286.6, 288.7, 287.0, 288.3, 287.4 | 109.2, 109.8, 108.9, 109.7, 110.0 |
| Idle working set at 5 min (MiB) | 284.2, 284.6, 284.2, 283.7, 283.6 | 123.3, 123.2, 123.1, 123.2, 123.1 |

## Method

- **Idle PC.** Before each session the scripts check that total CPU stays below 15 % for 30 s.
  The test-account session also checks that every disk is at least 80 % idle. During the same
  day, Windows servicing work on the system disk slowed both clients' start by several seconds
  while CPU stayed low, so runs taken then were discarded.
- **Fresh state.** Each run starts the client on a fresh copy of its data folder; the copy is
  deleted afterwards. The client is closed by its process id.
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
  pointer is placed on the first chat row with a posted mouse message and the screen is left to
  settle. A click is then posted to the window (the real mouse cursor is not moved). "First
  change" is the first capture that differs from the one before the click. "History stable" is
  the last change before the window stays unchanged for 500 ms.
- **Memory:** `PrivateUsage` and `WorkingSetSize` of the process tree once the chat list has been
  stable for 3 s (test account), or 30 s and 5 min after start (no account).

## Notes and limits

- The chat that was opened has one message, which both clients already know from the chat list.
  Rustgram paints the history from it at once; its history request returns later without
  changing the picture. Telegram Desktop paints the history after its own request returns.
- Rustgram's "first change" after the click is 0 ms because `PrintWindow` returned only after
  Rustgram had handled the posted click and repainted. It is shown for completeness. "History
  stable" is the comparable number.
- After the click, Rustgram's interface thread still waits for two requests in turn (marking the
  chat read, then loading history), about 30 ms each on these servers. The screen does not
  change during that time.
- At the start of the test-account runs each client used its own window size: 1456×908 for
  Telegram Desktop (restored from its settings), 1024×768 for Rustgram. The memory after start
  was taken at those sizes.
- Every run started from the same saved data, so neither client had the opened chat's history
  from an earlier run.

## Not measured

- Scrolling speed and frame pacing in a long chat. The test account has no long chat, and the
  test-server rules for this project allow messages only between two test accounts and require
  deleting them afterwards.
- Memory after opening many chats, for the same reason (the account has one chat).
- Message delivery between two Telegram Desktop instances.
- Login-screen readiness of Telegram Desktop without an account (the no-account session ran on a
  desktop where its window could not be captured).
- Install folder size of the current Rustgram build. The earlier measurement (Rustgram's
  executable plus shared FFmpeg libraries, 276.6 MiB, against Telegram Desktop's 264.7 MiB)
  still applies to the folder layout.
