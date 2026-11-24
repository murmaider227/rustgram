//! `tg-app-window`: Layer-3 main window, navigation controller, and `MockSession` crate.
//!
//! Strictly decoupled from `tg-app-live` via [`tg_app_core::session_api::AppSession`].

pub mod mock_session;
pub mod window;
