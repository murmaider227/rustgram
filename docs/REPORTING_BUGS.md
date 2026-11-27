# Reporting a bug

Rustgram is in testing, so bug reports are the most useful contribution. Issues are for bugs; ideas
and feature requests go to [Discussions > Ideas](https://github.com/murmaider227/rustgram/discussions/categories/ideas),
and security problems go to a [private report](https://github.com/murmaider227/rustgram/security/advisories/new).

## By hand

Open a [bug report](https://github.com/murmaider227/rustgram/issues/new?template=bug_report.yml).
Search existing issues first, give the exact version, and list the shortest steps that show the
problem.

- **Version**: at the bottom of the main menu (☰), or right-click `rustgram.exe` > Properties >
  Details > Product version.
- **Log**: `log.txt` in the working folder, which is `%APPDATA%\Rustgram` for the installer and for a
  portable copy without its own data folder, or the folder of `rustgram.exe` for a portable copy
  with a `tdata` or `RustgramForcePortable` folder next to it. For a more detailed log, start
  Rustgram with `-debug`; it then also writes `DebugLogs\log_HH_MM.txt`.
- **Crash reports**: `tdata\dumps\crash_*.txt` in the same folder.

Paste only the lines around the problem. Never attach anything else from `tdata`: it holds the keys
to your Telegram session. Before you upload a screenshot or log, hide phone numbers, names, chats
and message text: everything on GitHub is public.

## With your coding agent

If you use a coding agent (Claude Code, Codex, Gemini CLI or similar), it can investigate the
problem on your computer and draft the report for you. Start a session and paste:

```text
Rustgram (a Telegram client) misbehaves on this computer. Follow the playbook at
https://raw.githubusercontent.com/murmaider227/rustgram/main/docs/TRIAGE_PLAYBOOK.md
to find out why and draft a bug report. The problem: <describe it>
```

The playbook tells the agent what it may read (logs and crash reports only, never your session
files, chats, contacts or phone number), to remove private details from everything it quotes, and
to show you the complete report and post it only after you say yes. If `gh` is not installed it
gives you a prefilled link to the "Triage report" form instead, and you submit it yourself.
