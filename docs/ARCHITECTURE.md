# Rustgram architecture

Rustgram is a Rust port of [Telegram Desktop](https://github.com/telegramdesktop/tdesktop)
(C++/Qt, API layer 233). Behaviour, wire formats and algorithms follow the original; the code is
idiomatic Rust (no Qt, no reactive-library clone).

Target platform: Windows 10/11 x64 (`x86_64-pc-windows-msvc`). All operating-system code lives in
`tg-platform`, so other systems can be added later.

## Runtime stack

| Concern | Choice |
|---|---|
| Async runtime | `tokio` (multi-thread) for network, disk and timers |
| GUI | `egui` / `eframe` with custom painting |
| Images | `image` (JPEG, PNG, WebP, GIF), `resvg` for SVG |
| Crypto | RustCrypto (`aes`, `sha1`, `sha2`, `pbkdf2`, `md-5`) and `num-bigint` |
| Audio / video | FFmpeg through a small C shim, `cpal` for output, Ogg Opus |
| Logging | `tracing` with a file logger in `tg-core` |
| Local data | Telegram Desktop–compatible `tdata` in `tg-storage`, `rusqlite` for caches |
| Windows APIs | `windows` crate, WebView2 for Mini Apps and Instant View |

## Crate map

| Crate | Owns | Telegram Desktop source |
|---|---|---|
| `tg-tl-gen`, `tg-tl` | `.tl` schema parser and generated types/functions for layer 233 | `lib_tl`, `codegen/scheme` |
| `tg-crypto` | AES-IGE/CTR, SHA, PBKDF2, RSA, DH checks, MTProto 2.0 KDF, SRP | `mtproto/`, `base/openssl_help` |
| `tg-transport` | Abridged / intermediate / padded / full transports, obfuscation, MTProxy, SOCKS5/HTTP | `mtproto/connection_*` |
| `tg-mtproto` | Auth keys, PFS, encrypted sessions, acks, resends, salts, DC options and migration | `mtproto/` |
| `tg-data` | Domain model: peers, messages, histories, dialogs, folders, update state | `data/` |
| `tg-api` | API layer over MTProto: login, dialogs, history, sending, files, updates and gap recovery | `api/`, `apiwrap` |
| `tg-session` | Session runtime: the update pump, one update-state tracker, accounts | `main/` |
| `tg-storage` | `tdata` read/write, encrypted local files, media cache | `storage/` |
| `tg-core` | Launcher, command line, working directory, logging, global settings | `core/` |
| `tg-lang`, `tg-text` | Strings, plurals, language packs; text entities and markdown | `lang/`, `ui/text/` |
| `tg-style`, `tg-theme`, `tg-ui` | Ported style system, palettes and themes, widget toolkit | `lib_ui`, `window/themes/` |
| `tg-media`, `tg-video`, `tg-audio`, `tg-lottie` | Media decoding, streaming, playback, animated stickers | `media/`, `lib_lottie` |
| `tg-calls`, `tg-voip` | Call signaling and the media engine | `calls/`, `tgcalls` |
| `tg-iv`, `tg-webview` | Instant View and the Mini App host | `iv/`, `lib_webview` |
| `tg-bots`, `tg-payments`, `tg-passport`, `tg-wallet`, `tg-stories`, `tg-stats`, `tg-export`, `tg-editor` | Feature libraries | matching folders |
| `tg-platform`, `tg-updater`, `tg-xtask` | OS integration, self-update, release tooling | `platform/`, `_other/updater` |
| `tg-app-*`, `tg-app` | The application, split into eight crates (below) | `window/`, `history/`, `boxes/`, `settings/`, … |

## Application crates

The interface is split into eight crates that form a strict dependency graph:

| Layer | Crate | Contents |
|---|---|---|
| 0 | `tg-app-core` | Shared types and the backend traits every screen talks to |
| 1 | `tg-app-media` | Media viewer, player, stories, calls UI, Mini Apps, notifications, tray |
| 1 | `tg-app-settings` | Settings, themes, login screens, data export, wallet UI |
| 1 | `tg-app-info` | Boxes, profiles, chat list, payments and statistics UI |
| 1 | `tg-app-live` | The live session backend that implements the traits over `tg-api` |
| 2 | `tg-app-history` | Chat view, message bubbles, composer, bot keyboards, Business |
| 3 | `tg-app-window` | Main window, layout, navigation, deep links |
| 4 | `tg-app` | The `rustgram` binary, startup and wiring |

A crate depends only on lower layers; something a lower crate needs from a higher one goes behind a
trait in `tg-app-core`.

```
tg-tl-gen -> (build dependency of) tg-tl
tg-crypto, tg-tl -> tg-transport -> tg-mtproto -> tg-api -> tg-session -> tg-app-live
tg-tl, tg-text -> tg-data -> tg-api
tg-crypto -> tg-storage -> tg-core -> tg-app-core
tg-theme, tg-style -> tg-ui -> tg-app-core
tg-app-core -> tg-app-media, tg-app-settings, tg-app-info, tg-app-live
tg-app-core, tg-app-media, tg-app-info -> tg-app-history
tg-app-core, tg-app-media, tg-app-settings, tg-app-info, tg-app-history -> tg-app-window
tg-app-window, tg-app-live -> tg-app
```

## Testing

- Unit tests in every crate; known-answer tests for protocol and crypto code.
- Conformance suites (`crates/*/tests/*_conformance.rs`) run full MTProto 2.0 sessions against a
  scripted in-process server and check every request byte for byte against what Telegram Desktop
  sends.
- UI tests with `egui_kittest`, including golden screenshots of the main window.
- Live smoke tests against Telegram's test servers (`crates/tg-session/examples/test_dc_smoke.rs`),
  which refuse to connect anywhere except the test data centers.
