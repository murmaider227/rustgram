//! Shell-host `AppThemesHost` bindings for `LiveSession`.

use std::sync::Arc;

use tg_app_live::session::LiveSession;

/// Wires `live`'s [`ThemesBackend`] into [`crate::themes::AppThemesHost`].
///
/// Called on live session start and after switching to another active account.
pub fn install_live_themes_hooks(ctx: &egui::Context, live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    crate::themes::AppThemesHost::set_backend_for_session(ctx, key, live.themes_backend());
}

/// Ensures [`crate::themes::AppThemesHost`] is bound to `live` (idempotent O(1) check).
pub fn ensure_live_themes_hooks(ctx: &egui::Context, live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    if crate::themes::AppThemesHost::bound_session_key(ctx) != Some(key) {
        install_live_themes_hooks(ctx, live);
    }
}

/// Detaches the live [`ThemesBackend`] from [`crate::themes::AppThemesHost`] (on logout,
/// account switch, or shutdown).
pub fn clear_live_themes_hooks(ctx: &egui::Context) {
    crate::themes::AppThemesHost::clear_backend(ctx);
}

/// Clears any previous live [`ThemesBackend`] and, when `live` is `Some`, installs `live`'s backend.
pub fn sync_live_themes_hooks(ctx: Option<&egui::Context>, live: Option<&LiveSession>) {
    let Some(ctx) = ctx else {
        return;
    };
    clear_live_themes_hooks(ctx);
    if let Some(session) = live {
        install_live_themes_hooks(ctx, session);
    }
}

pub use crate::window::deep_links::dispatch_themes_deep_link_overlay;
