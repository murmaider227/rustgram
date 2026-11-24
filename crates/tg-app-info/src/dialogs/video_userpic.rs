//! Animated video userpic player and circular mask renderer for the dialogs list.
//!
//! Port origin:
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_video_userpic.h`
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_video_userpic.cpp`

use egui::{Rect, Ui};

use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::video_userpic::*;

/// Photo rectangle of a dialog row painted at `row_rect` (`st.padding` + `st.photo_size` of the
/// row style, as in `layout_dialog_row`).
#[must_use]
pub fn dialog_row_photo_rect(
    row_rect: Rect,
    item: &crate::dialogs::backend::DialogItem,
    narrow: bool,
    show_folder_tags: bool,
) -> Rect {
    let style = crate::dialogs::layout::row_style_for(item, show_folder_tags, narrow);
    Rect::from_min_size(
        row_rect.min + egui::vec2(style.padding.left as f32, style.padding.top as f32),
        egui::vec2(style.photo_size as f32, style.photo_size as f32),
    )
}

/// Updates `peer_id`'s entry from the backend (`InnerWidget::validateVideoUserpic`) and paints
/// the current video frame over the static userpic already drawn in `photo_rect`.
///
/// Returns `true` when a video frame was painted.
pub fn paint_row_video_userpic(
    ui: &Ui,
    cache: &mut DialogVideoUserpicCache,
    backend: &dyn crate::dialogs::backend::DialogsBackend,
    peer_id: PeerId,
    photo_rect: Rect,
) -> bool {
    let Some(photo_id) = backend.dialog_video_userpic(peer_id) else {
        cache.remove(peer_id);
        return false;
    };
    let entry = cache.entry_mut(peer_id);
    entry.sync_photo_id(Some(photo_id));
    if entry.state() == VideoUserpicState::Loading {
        match backend.video_userpic_frames(peer_id, photo_id) {
            VideoUserpicFetch::Pending => {}
            VideoUserpicFetch::Ready(frames) => entry.set_decoded_frames(frames),
            VideoUserpicFetch::Failed => entry.mark_bad(),
        }
    }
    let (now_ms, paused) = ui
        .ctx()
        .input(|i| ((i.time * 1000.0).max(0.0) as u64, !i.focused));
    paint_dialog_userpic(
        ui,
        ui.painter(),
        photo_rect,
        Some(entry),
        now_ms,
        paused,
        |_, _| {},
    )
}
