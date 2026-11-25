//! tdesktop's poll / checklist `Ui::Text::IconEmoji` in one-line message previews
//! (`MediaPoll::toPreview` uses `dialogsMiniPollIcon`, `MediaTodoList::toPreview`
//! `dialogsMiniTodoListIcon`). Plain-text previews carry the `notificationText()` emoji
//! ("📊 ", "✅ ", "☑️ ") instead; widgets that draw a preview swap that marker for the icon.

use egui::{Color32, Painter, Pos2, Rect, Vec2};
use tg_style::structs::IconEmoji;
use tg_style::{ColorResolver, Scale, st};

use crate::icons::paint_style_icon;

/// Splits a leading poll / checklist marker off `text`: the icon to draw and the text after it.
#[must_use]
pub fn split_media_marker(text: &str) -> Option<(&'static IconEmoji, &str)> {
    const POLL: [&str; 1] = ["\u{1F4CA} "];
    const TODO: [&str; 3] = ["\u{2705} ", "\u{2611}\u{FE0F} ", "\u{2611} "];
    if let Some(rest) = POLL.iter().find_map(|m| text.strip_prefix(m)) {
        return Some((&st::dialogs_mini_poll_icon, rest));
    }
    TODO.iter()
        .find_map(|m| text.strip_prefix(m))
        .map(|rest| (&st::dialogs_mini_todo_list_icon, rest))
}

/// Horizontal space the icon takes in the line (`padding.left + width + padding.right`).
#[must_use]
pub fn media_marker_advance(icon: &IconEmoji) -> f32 {
    icon_size(icon).x + icon.padding.left.0 as f32 + icon.padding.right.0 as f32
}

/// Paints `icon` at `line_left_top` (the top-left of the text line) tinted `tint`; returns the
/// advance for the text that follows.
pub fn paint_media_marker(
    painter: &Painter,
    line_left_top: Pos2,
    icon: &IconEmoji,
    tint: Color32,
    resolver: &dyn ColorResolver,
) -> f32 {
    let size = icon_size(icon);
    let rect = Rect::from_min_size(
        Pos2::new(
            line_left_top.x + icon.padding.left.0 as f32,
            line_left_top.y + icon.padding.top.0 as f32,
        ),
        size,
    );
    paint_style_icon(painter, rect, icon.icon, resolver, Some(tint));
    media_marker_advance(icon)
}

fn icon_size(icon: &IconEmoji) -> Vec2 {
    icon.icon
        .scaled_size_at(Scale::DEFAULT)
        .map_or(Vec2::splat(16.0), |s| {
            Vec2::new(s.width as f32, s.height as f32)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_and_checklist_markers_become_icons() {
        let (icon, rest) = split_media_marker("\u{1F4CA} Which theme?").expect("poll");
        assert!(std::ptr::eq(icon, &st::dialogs_mini_poll_icon));
        assert_eq!(rest, "Which theme?");
        for marker in ["\u{2705} ", "\u{2611}\u{FE0F} "] {
            let text = format!("{marker}Release checklist");
            let (icon, rest) = split_media_marker(&text).expect("todo");
            assert!(std::ptr::eq(icon, &st::dialogs_mini_todo_list_icon));
            assert_eq!(rest, "Release checklist");
        }
        assert!(split_media_marker("Plain text").is_none());
        assert!(split_media_marker("\u{1F4CA}no space").is_none());
    }
}
