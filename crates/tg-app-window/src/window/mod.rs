//! Window shell subsystem porting `Telegram/SourceFiles/window/`, `mainwindow.cpp`, and `mainwidget.cpp`.

pub mod adaptive;
pub mod chat_preview;
pub mod connecting_widget;
pub mod controller;
pub mod deep_links;
pub mod dialogs_snapshot;
pub mod interface_scale;
pub mod layer_stack;
pub mod lock_widgets;
pub mod main_menu;
pub mod main_widget;
pub mod media_fullscreen;
pub use tg_app_core::core_types::platform_call;
pub mod platform_shell;
#[cfg(test)]
mod screenshots;
pub mod section;
pub mod security_policy;
pub mod session_controller;
pub mod shortcuts_wiring;
#[cfg(test)]
mod tests_w8_screens;
