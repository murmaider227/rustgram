//! The media viewer covers the whole monitor of the window (`OverlayWidget` is a fullscreen
//! top-level window in tdesktop). Rustgram draws the viewer inside the main viewport, so while it
//! is open the viewport goes fullscreen on its current monitor and returns to its previous state
//! when the viewer closes. Screenshot protection keeps working because the viewer stays in the
//! protected main window.

use egui::ViewportCommand;

/// Tracks whether the media viewer switched the main viewport to fullscreen.
#[derive(Debug, Default)]
pub struct MediaViewerFullscreen {
    /// `Some(was_fullscreen)` while the viewer is open.
    restore: Option<bool>,
}

impl MediaViewerFullscreen {
    /// Call once per frame after the viewer was shown; `viewer_open` is its current state.
    pub fn sync(&mut self, ctx: &egui::Context, viewer_open: bool) {
        match (viewer_open, self.restore) {
            (true, None) => {
                let was_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                if !was_fullscreen {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                }
                self.restore = Some(was_fullscreen);
            }
            (false, Some(was_fullscreen)) => {
                if !was_fullscreen {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                }
                self.restore = None;
            }
            _ => {}
        }
    }

    /// Whether the viewer currently holds the viewport in fullscreen.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self.restore, Some(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        fullscreen: bool,
        mut f: impl FnMut(&egui::Context),
    ) -> Vec<ViewportCommand> {
        let mut input = egui::RawInput::default();
        if let Some(info) = input.viewports.get_mut(&input.viewport_id) {
            info.fullscreen = Some(fullscreen);
        }
        let mut output = ctx.run_ui(input, |ui| f(ui.ctx()));
        output.textures_delta.clear();
        output
            .viewport_output
            .remove(&egui::ViewportId::ROOT)
            .map(|v| v.commands)
            .unwrap_or_default()
    }

    #[test]
    fn opening_the_viewer_goes_fullscreen_and_closing_restores_the_window() {
        let ctx = egui::Context::default();
        let mut fs = MediaViewerFullscreen::default();
        let cmds = frame(&ctx, false, |c| fs.sync(c, true));
        assert!(cmds.contains(&ViewportCommand::Fullscreen(true)));
        assert!(fs.is_active());
        let cmds = frame(&ctx, true, |c| fs.sync(c, true));
        assert!(cmds.is_empty(), "no repeated commands while open");
        let cmds = frame(&ctx, true, |c| fs.sync(c, false));
        assert!(cmds.contains(&ViewportCommand::Fullscreen(false)));
        assert!(!fs.is_active());
    }

    #[test]
    fn a_window_that_was_already_fullscreen_stays_fullscreen() {
        let ctx = egui::Context::default();
        let mut fs = MediaViewerFullscreen::default();
        let cmds = frame(&ctx, true, |c| fs.sync(c, true));
        assert!(!cmds.contains(&ViewportCommand::Fullscreen(true)));
        let cmds = frame(&ctx, true, |c| fs.sync(c, false));
        assert!(!cmds.contains(&ViewportCommand::Fullscreen(false)));
    }
}
