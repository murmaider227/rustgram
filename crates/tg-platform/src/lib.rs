//! `tg-platform`: Windows platform integration for Rustgram (with non-Windows no-op fallbacks).
//!
//! Ports the Windows platform layer from Telegram Desktop (`Telegram/SourceFiles/platform/win/`,
//! `Telegram/lib_base/base/platform/win/`, and `Telegram/lib_spellcheck/spellcheck/platform/win/`):
//! - [`app_id`]: AppUserModelID (`Rustgram`) and Start-menu shortcut validation/creation
//! - [`notifications`]: WinRT toast notifications, COM/WinRT activation callbacks, per-chat
//!   clearing, Focus Assist & Quiet Hours / `SHQueryUserNotificationState` checks
//! - [`tray`]: System tray icon with context menu & unread counter overlay, `ITaskbarList3`
//!   taskbar overlay badge, `.ico` writer, and `FlashWindowEx`
//! - [`single_instance`]: Named-mutex + named-pipe single-instance lock & activation IPC
//! - [`registration`]: `tg://` / `tonsite://` URL scheme registration in `HKCU`, autostart
//!   shortcut (`-autostart`), and Explorer "Send to" shortcut
//! - [`system_info`]: Idle time, dark mode, accent color, Windows version & device model for
//!   MTProto `initConnection`, per-monitor DPI, battery/power saving, Explorer/URL/mail helpers
//! - [`spellcheck`]: Native Windows spellchecker via `ISpellChecker` / `ISpellCheckerFactory`
//! - [`smtc`]: Windows `SystemMediaTransportControls` (SMTC) WinRT audio controls & metadata
//! - [`ocr`]: Native Windows text recognition via `Windows.Media.Ocr`
//! - [`screenshot_protection`]: Multi-reason `SetWindowDisplayAffinity` screenshot protection
//! - [`taskbar_buttons`]: `ITaskbarList3` media thumbnail toolbar buttons (previous/play-pause/next)
//! - [`wallet_protection`]: Windows DPAPI key protection for the built-in TON wallet vault
//! - [`webauthn`]: Windows `webauthn.dll` FIDO2 / Passkeys and cross-device caBLE / hybrid QR
//! - [`windows_hello`]: Windows Hello (`UserConsentVerifier`) biometric / PIN unlock
//! - [`cable_scanner`]: Windows caBLE BLE proximity scanner & FIDO2/Google EID advertisement parser
//! - [`cable_tunnel`]: caBLEv2 WebSocket tunnel client, Noise `KNpsk0`/`NKpsk0` handshake, and CTAP2 framing
//! - [`widget_sandbox`]: Sandboxed widget helper Win32 AppContainer / Job-Object launcher & framed IPC
//! - [`autostart`]: Windows StartupTask (WinRT) & `HKCU\...\Run` autostart registration

pub mod app_id;
pub mod autostart;
pub mod cable_scanner;
pub mod cable_tunnel;
pub mod error;
pub mod frameless;
pub mod geolocation;
pub mod notifications;
pub mod ocr;
pub mod registration;
pub mod screenshot_protection;
pub mod single_instance;
pub mod smtc;
pub mod spellcheck;
pub mod system_info;
pub mod taskbar_buttons;
pub mod translate_win;
pub mod tray;
pub mod wallet_protection;
pub mod webauthn;
pub mod widget_sandbox;
pub mod windows_hello;

pub use error::{PlatformError, Result};
