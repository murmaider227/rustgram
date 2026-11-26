//! Shell-level live host wiring (`LiveAppHost`, `MultiAccountWorkspaceHost`,
//! `AppExportHost`/`AppThemesHost`/`AppWalletHost` live hooks, and video stream loop).

pub mod export_host;
pub mod host;
pub mod multi_account;
pub mod themes_host;
mod user_errors;
pub use user_errors::{forced_logout_text, payments_link_kind};
pub mod video_stream_loop;
pub mod wallet_host;
