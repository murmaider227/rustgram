# Contributing to Rustgram

Thanks for your interest in Rustgram. This guide explains how the project works and what a good
contribution looks like.

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
