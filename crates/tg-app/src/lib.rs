//! `tg-app`: Desktop application shell and navigation architecture for Rustgram.
//!
//! Port origin: `Telegram/SourceFiles/window/`, `mainwindow.cpp`, `mainwidget.cpp`,
//! `intro/`, `dialogs/`, `history/`, `info/`, and `settings/`.

pub mod app;
pub use tg_app_core::clock;
pub use tg_app_core::core_types;
pub use tg_app_history::bots;
pub use tg_app_history::business;
pub use tg_app_history::history;
pub use tg_app_info::boxes;
pub use tg_app_info::delegate_marketplace_via_registry;
pub use tg_app_info::dialogs;
pub use tg_app_info::info;
pub use tg_app_media::calls;
pub use tg_app_settings::chat_helpers;
pub use tg_app_settings::export;
pub use tg_app_settings::intro;
pub mod live;
pub mod live_host;
pub use tg_app_core::session_api;
pub use tg_app_info::payments;
pub use tg_app_media::media;
pub use tg_app_media::notifications;
pub use tg_app_settings::settings;
pub mod stand_in;
pub mod startup;
pub use tg_app_info::statistics;
pub use tg_app_media::stories;
pub use tg_app_media::tray;
pub use tg_app_media::webview;
pub use tg_app_settings::themes;
pub use tg_app_settings::wallet;
pub use tg_app_window::window;

pub use app::{AppConfig, TgforkApp, run_app};
