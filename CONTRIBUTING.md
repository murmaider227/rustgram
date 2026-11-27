# Contributing to Rustgram

Thanks for your interest in Rustgram. This guide explains how the project works and what a good
contribution looks like.

## The most useful help: bug reports

Rustgram is in testing, and a precise bug report helps more than anything else.

- **Bugs** go to [issues](https://github.com/murmaider227/rustgram/issues/new/choose). Search open
  and closed issues first; if yours exists, add your version and steps there instead of opening a
  second one. One problem per issue, with the exact version and the shortest steps that show it.
- **Your coding agent can write the report.** [docs/REPORTING_BUGS.md](docs/REPORTING_BUGS.md)
  shows how to point Claude Code, Codex or a similar agent at the
  [triage playbook](docs/TRIAGE_PLAYBOOK.md). The agent reads only logs and crash reports, removes
  private details, and posts nothing until you approve the exact text.
- **Ideas and feature requests** go to
  [Discussions > Ideas](https://github.com/murmaider227/rustgram/discussions/categories/ideas);
  questions go to [Q&A](https://github.com/murmaider227/rustgram/discussions/categories/q-a).
- **Security problems** go to a private report, never a public issue (see [SECURITY.md](SECURITY.md)).

When we need more details we add the `needs more info` label. If there is no reply within 7 days the
issue closes; reply any time with the details and we reopen it.

## Pull requests

This public repository is an export of the private repository where development happens (each
change passes a local merge gate there, and live checks against Telegram's test servers use test
accounts that exist only there). Because of that, **pull requests are not merged here.** They are still welcome, and this is what happens to
one:

1. A maintainer reads it and checks the change against the bug and against Telegram Desktop.
2. If we take it, we re-apply the change in the private repository, adjusted if needed, with you as
   co-author: the commit carries a `Co-authored-by:` line with your GitHub noreply address, and so
   does the commit that brings it into this repository, so GitHub credits you.
3. We close the PR with the `re-applied` label and a note on where the change landed. It ships in
   the next release.

If we do not take it, we say why in the PR before closing it.

Small, focused fixes for a bug have the best chance. A good PR:

- fixes one problem, ideally one that has an issue; new features start as a discussion in Ideas;
- follows Telegram Desktop's behaviour (see "How the code is written" below);
- fills in the [pull request template](.github/pull_request_template.md): **Problem**, **Change**,
  **Verification** (what you ran and saw; before/after screenshots for UI changes) and **AI use**
  (the model and harness, if an AI agent wrote or changed any of it). AI-assisted PRs are fine; we
  only ask you to say so and to have checked the result yourself;
- contains nothing private: no API credentials, phone numbers, session files or chat screenshots.

## How the code is written

Rustgram is a port of [Telegram Desktop](https://github.com/telegramdesktop/tdesktop). For every
feature there is a matching place in Telegram Desktop's `Telegram/SourceFiles` tree.

- **Port behaviour, write idiomatic Rust.** Read the C++ first and keep its wire formats, request
  flags, paging, caching rules and edge cases. Do not copy Qt patterns: use plain Rust types,
  `tokio` and `egui`.
- **Cite the source.** Start each file with a short module doc line naming what it ports, for
  example `//! Port of mtproto/session_private.cpp.`, and cite the C++ function when you fix a
  behaviour difference.
- **Respect the layers.** The `tg-app-*` crates form a strict dependency graph (see
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)). A lower crate never depends on a higher one; use a
  trait in `tg-app-core` instead.

## Quality bar

Every pull request must pass, for each crate it touches:

```powershell
cargo fmt --check -p <crate>
cargo clippy -p <crate> --all-targets -- -D warnings
cargo test -p <crate>
```

- No `unwrap()` / `expect()` on data that comes from the network or disk.
- Add tests. Protocol code gets known-answer tests and, where it talks to the server, a test against
  the scripted fake MTProto server used in `crates/tg-session/tests/*_conformance.rs`.
- UI changes come with before/after screenshots at 1440×900 and 390×844 in the pull request.

## Testing against Telegram

- Unit and conformance tests never touch the network.
- Live checks run only against Telegram's **test servers** (`-testmode`), with test accounts.
- Never commit API credentials, phone numbers, session files (`tdata`) or anything else personal.

## Licensing

By contributing you agree that your work is released under the project's license,
GPL-3.0-or-later with the OpenSSL exception (see [LICENSE](LICENSE) and [LEGAL](LEGAL)). Do not add
assets you cannot release under a GPL-compatible license, and do not add Telegram logos or other
trademarked artwork.
