//! The media viewer covers the whole monitor of the window (`OverlayWidget` is a fullscreen
//! top-level window in tdesktop). Rustgram draws the viewer inside the main viewport, so while it
//! is open the viewport goes fullscreen on its current monitor and returns to its previous state
//! when the viewer closes. Screenshot protection keeps working because the viewer stays in the
//! protected main window.
//!
//! A maximized window leaves the maximized state first: winit keeps the client area of a
//! maximized window without decorations at the monitor's work area (`WM_NCCALCSIZE`), so a
//! fullscreen window that is still maximized left the taskbar strip unpainted and the taskbar on
//! top, and after the viewer closed the window kept the monitor-sized rect (TGB-2970). The window
//! is maximized again when the viewer closes.

use egui::ViewportCommand;

/// The window state to return to when the viewer closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Restore {
    fullscreen: bool,
    maximized: bool,
}

/// Tracks whether the media viewer switched the main viewport to fullscreen.
#[derive(Debug, Default)]
pub struct MediaViewerFullscreen {
    /// `Some(..)` while the viewer is open.
    restore: Option<Restore>,
}

impl MediaViewerFullscreen {
    /// Call once per frame after the viewer was shown; `viewer_open` is its current state.
    pub fn sync(&mut self, ctx: &egui::Context, viewer_open: bool) {
        match (viewer_open, self.restore) {
            (true, None) => {
                let (fullscreen, maximized) = ctx.input(|i| {
                    let v = i.viewport();
                    (v.fullscreen.unwrap_or(false), v.maximized.unwrap_or(false))
                });
                let restore = Restore {
                    fullscreen,
                    maximized: maximized && !fullscreen,
                };
                if !restore.fullscreen {
                    if restore.maximized {
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                }
                self.restore = Some(restore);
            }
            (false, Some(restore)) => {
                if !restore.fullscreen {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    if restore.maximized {
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                    }
                }
                self.restore = None;
            }
            _ => {}
        }
    }

    /// Whether the viewer currently holds the viewport in fullscreen.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(
            self.restore,
            Some(Restore {
                fullscreen: false,
                ..
            })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        fullscreen: bool,
        maximized: bool,
        mut f: impl FnMut(&egui::Context),
    ) -> Vec<ViewportCommand> {
        let mut input = egui::RawInput::default();
        if let Some(info) = input.viewports.get_mut(&input.viewport_id) {
            info.fullscreen = Some(fullscreen);
            info.maximized = Some(maximized);
        }
        let mut output = ctx.run_ui(input, |ui| f(ui.ctx()));
        output.textures_delta.clear();
        output
            .viewport_output
            .remove(&egui::ViewportId::ROOT)
            .map(|v| v.commands)
            .unwrap_or_default()
            .into_iter()
            // egui's own first-frame commands (`SetTheme`) are not the viewer's.
            .filter(|c| {
                matches!(
                    c,
                    ViewportCommand::Fullscreen(_) | ViewportCommand::Maximized(_)
                )
            })
            .collect()
    }

    #[test]
    fn opening_the_viewer_goes_fullscreen_and_closing_restores_the_window() {
        let ctx = egui::Context::default();
        let mut fs = MediaViewerFullscreen::default();
        let cmds = frame(&ctx, false, false, |c| fs.sync(c, true));
        assert_eq!(cmds, vec![ViewportCommand::Fullscreen(true)]);
        assert!(fs.is_active());
        let cmds = frame(&ctx, true, false, |c| fs.sync(c, true));
        assert!(cmds.is_empty(), "no repeated commands while open");
        let cmds = frame(&ctx, true, false, |c| fs.sync(c, false));
        assert_eq!(cmds, vec![ViewportCommand::Fullscreen(false)]);
        assert!(!fs.is_active());
    }

    /// TGB-2970: a maximized window that went fullscreen kept its client area at the work area,
    /// so the taskbar stayed visible under the viewer. It leaves the maximized state first and
    /// is maximized again after the viewer closes.
    #[test]
    fn a_maximized_window_leaves_maximized_before_fullscreen_and_returns_to_it() {
        let ctx = egui::Context::default();
        let mut fs = MediaViewerFullscreen::default();
        let cmds = frame(&ctx, false, true, |c| fs.sync(c, true));
        assert_eq!(
            cmds,
            vec![
                ViewportCommand::Maximized(false),
                ViewportCommand::Fullscreen(true)
            ]
        );
        let cmds = frame(&ctx, true, false, |c| fs.sync(c, true));
        assert!(cmds.is_empty());
        let cmds = frame(&ctx, true, false, |c| fs.sync(c, false));
        assert_eq!(
            cmds,
            vec![
                ViewportCommand::Fullscreen(false),
                ViewportCommand::Maximized(true)
            ]
        );
    }

    #[test]
    fn a_window_that_was_already_fullscreen_stays_fullscreen() {
        let ctx = egui::Context::default();
        let mut fs = MediaViewerFullscreen::default();
        let cmds = frame(&ctx, true, true, |c| fs.sync(c, true));
        assert!(cmds.is_empty());
        let cmds = frame(&ctx, true, true, |c| fs.sync(c, false));
        assert!(cmds.is_empty());
    }
}
