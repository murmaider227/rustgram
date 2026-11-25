//! Chat-list preview line: 16 px media thumbnails, `IconEmoji` type icons and the blue
//! `dialogsTextFgService` media / service text.
//!
//! Port origin: `MessageView::prepare` / `MessageView::paint`
//! (`Telegram/SourceFiles/dialogs/ui/dialogs_message_view.cpp`), the `toPreview` methods and
//! `WithCaptionNotificationText` in `Telegram/SourceFiles/data/data_media_types.cpp`, and the
//! `dialogsMini*` values in `Telegram/SourceFiles/dialogs/dialogs.style`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, ColorImage, CornerRadius, FontId, Id, Painter, Pos2, Rect, TextureHandle, Vec2,
};
use tg_history::elide_text_right;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::structs::IconEmoji;
use tg_ui::tg_style::{Scale, st};

use crate::dialogs::backend::{DialogItem, DialogMediaPreview, DialogMediaPreviewKind};
use crate::dialogs::layout::{DialogColors, measure_text_i32};

/// Decoded thumbnails kept per egui context; older entries are dropped past this count.
const MINI_PREVIEW_CACHE_LIMIT: usize = 512;

/// Leading `IconEmoji` drawn before the media type when the row has no mini image
/// (`typeIcon` in `MediaFile::toPreview`, `MediaContact` / `MediaLocation` / `MediaPoll` /
/// `MediaTodoList` / `MediaInvoice` / `MediaStory::toPreview`).
#[must_use]
pub fn preview_type_icon(kind: DialogMediaPreviewKind) -> Option<&'static IconEmoji> {
    use DialogMediaPreviewKind as K;
    match kind {
        K::File => Some(&st::dialogs_mini_file_icon),
        K::Audio => Some(&st::dialogs_mini_audio_icon),
        K::Location => Some(&st::dialogs_mini_location_icon),
        K::Contact => Some(&st::dialogs_mini_contact_icon),
        K::Poll => Some(&st::dialogs_mini_poll_icon),
        K::TodoList => Some(&st::dialogs_mini_todo_list_icon),
        K::Invoice => Some(&st::dialogs_mini_invoice_icon),
        K::Story => Some(&st::dialogs_mini_story_icon),
        _ => None,
    }
}

/// Whether the 16 px thumbnail gets tdesktop's centered play glyph (`PutPlayIcon` for video
/// files and video messages).
#[must_use]
pub const fn preview_has_play_icon(kind: DialogMediaPreviewKind) -> bool {
    matches!(
        kind,
        DialogMediaPreviewKind::Video | DialogMediaPreviewKind::RoundVideo
    )
}

/// Drops the emoji marker of `notificationText()` ("📊 ", "☑️ ") when the chat list draws the
/// poll / checklist `IconEmoji` instead.
#[must_use]
pub fn strip_notification_marker(kind: DialogMediaPreviewKind, text: &str) -> &str {
    let markers: &[&str] = match kind {
        DialogMediaPreviewKind::Poll => &["\u{1F4CA} "],
        DialogMediaPreviewKind::TodoList => &["\u{2611}\u{FE0F} ", "\u{2611} ", "\u{2705} "],
        _ => &[],
    };
    markers
        .iter()
        .find_map(|m| text.strip_prefix(m))
        .unwrap_or(text)
}

/// Byte length of the leading part of `text` painted in `dialogsTextFgService`
/// (`Ui::Text::Colorized` in `WithCaptionNotificationText`): everything for service messages
/// and caption-less media, the `"{media},"` part before a caption when no mini image is shown,
/// and nothing for plain text.
#[must_use]
pub fn colorized_prefix_len(
    preview: Option<&DialogMediaPreview>,
    has_images: bool,
    text: &str,
) -> usize {
    let Some(preview) = preview else {
        return 0;
    };
    if preview.use_service_color {
        return text.len();
    }
    let typed_with_caption = matches!(
        preview.kind,
        DialogMediaPreviewKind::File
            | DialogMediaPreviewKind::Audio
            | DialogMediaPreviewKind::Voice
            | DialogMediaPreviewKind::Video
            | DialogMediaPreviewKind::Gif
            | DialogMediaPreviewKind::RoundVideo
            | DialogMediaPreviewKind::Location
            | DialogMediaPreviewKind::Invoice
    );
    if !preview.has_caption || has_images || !typed_with_caption {
        return 0;
    }
    text.find(", ").map_or(0, |at| at + 1)
}

#[derive(Clone, Default)]
struct MiniPreviewCache(HashMap<u64, Option<(TextureHandle, Rect)>>);

/// Decodes (once per context) a stripped thumbnail into a texture plus the UV rect of its
/// centered square (`PreparePreviewImage` crops to a square).
fn mini_preview_texture(ctx: &egui::Context, bytes: &[u8]) -> Option<(egui::TextureId, Rect)> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let key = hasher.finish();
    let cache_id = Id::unique("tg_dialogs_mini_preview_cache");
    if let Some(hit) = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<MiniPreviewCache>(cache_id)
            .0
            .get(&key)
            .cloned()
    }) {
        return hit.map(|(tex, uv)| (tex.id(), uv));
    }
    let decoded = tg_media::image::inline::from_inline_bytes(bytes)
        .ok()
        .filter(|img| img.width() > 0 && img.height() > 0)
        .map(|img| {
            let (w, h) = img.dimensions();
            let (wf, hf) = (w as f32, h as f32);
            let side = wf.min(hf);
            let uv = Rect::from_min_size(
                Pos2::new((wf - side) * 0.5 / wf, (hf - side) * 0.5 / hf),
                Vec2::new(side / wf, side / hf),
            );
            let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
            let tex = ctx.load_texture(
                format!("tg_dialogs_mini_preview_{key}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            (tex, uv)
        });
    ctx.data_mut(|d| {
        let cache = &mut d.get_temp_mut_or_default::<MiniPreviewCache>(cache_id).0;
        if cache.len() >= MINI_PREVIEW_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, decoded.clone());
    });
    decoded.map(|(tex, uv)| (tex.id(), uv))
}

/// Colors of the preview line for the row state (`dialogsTextFg*` / `dialogsTextFgService*`).
#[derive(Debug, Clone, Copy)]
pub struct PreviewLineColors {
    /// Plain preview text (`dialogsTextFg`, `Over`, `Active`).
    pub text: Color32,
    /// Sender, media type and service text (`dialogsTextFgService`, `Over`, `Active`).
    pub service: Color32,
}

/// Paints the latest-message line `[Sender:] [mini image | type icon] text` inside `r`
/// (`MessageView::paint`). `painter` should already clip to the row's text column.
pub fn paint_preview_line(
    painter: &Painter,
    r: Rect,
    item: &DialogItem,
    font: &FontId,
    fg: PreviewLineColors,
    colors: &DialogColors<'_>,
) {
    let preview = item.media_preview.as_ref();
    let text = preview.map_or(item.last_message.as_str(), |p| {
        strip_notification_marker(p.kind, &item.last_message)
    });
    let thumb = preview
        .and_then(|p| p.stripped_thumb.as_deref())
        .and_then(|bytes| mini_preview_texture(painter.ctx(), bytes));
    let has_images = thumb.is_some();
    let line_h = painter
        .layout_no_wrap("Ag".to_owned(), font.clone(), fg.text)
        .size()
        .y;
    let line_top = r.center().y - line_h * 0.5;
    let mut x = r.left();

    // `_senderCache`: "Name:" in the service color; a trailing space only before plain text.
    if let Some(sender) = item.last_sender.as_ref().filter(|_| !text.is_empty()) {
        let wrapped = tg_lang::tr::lng_dialogs_text_from_wrapped(sender);
        let sender_text = if has_images {
            wrapped
        } else {
            format!("{wrapped} ")
        };
        let galley = painter.layout_no_wrap(sender_text, font.clone(), fg.service);
        let w = galley.size().x;
        painter.galley(Pos2::new(x, line_top), galley, fg.service);
        x += w;
        if has_images {
            x += (st::dialogs_mini_preview_skip.0 + st::dialogs_mini_preview_right.0) as f32;
        }
    }

    let colored = colorized_prefix_len(preview, has_images, text);
    if let (Some((tex, uv)), Some(p)) = (thumb, preview) {
        let size = st::dialogs_mini_preview.0 as f32;
        let step = size + st::dialogs_mini_preview_skip.0 as f32;
        if r.right() - x >= step {
            let mini = Rect::from_min_size(
                Pos2::new(x, line_top + st::dialogs_mini_preview_top.0 as f32),
                Vec2::splat(size),
            );
            let radius = if p.kind == DialogMediaPreviewKind::RoundVideo {
                size * 0.5
            } else {
                st::dialogs_mini_preview_radius.0 as f32
            };
            painter.add(
                egui::epaint::RectShape::filled(
                    mini,
                    CornerRadius::same(radius.round() as u8),
                    Color32::WHITE,
                )
                .with_texture(tex, uv),
            );
            if preview_has_play_icon(p.kind) {
                paint_style_icon(painter, mini, st::dialogs_mini_play, colors, None);
            }
            x += step;
        }
        x += st::dialogs_mini_preview_right.0 as f32;
    } else if let Some(icon) = preview.and_then(|p| preview_type_icon(p.kind)) {
        let size = icon
            .icon
            .scaled_size_at(Scale::DEFAULT)
            .map_or(Vec2::splat(18.0), |s| {
                Vec2::new(s.width as f32, s.height as f32)
            });
        let pad = icon.padding;
        let advance = pad.left.0 as f32 + size.x + pad.right.0 as f32;
        if r.right() - x > advance {
            let icon_rect = Rect::from_min_size(
                Pos2::new(x + pad.left.0 as f32, line_top + pad.top.0 as f32),
                size,
            );
            let tint = if colored > 0 { fg.service } else { fg.text };
            paint_style_icon(painter, icon_rect, icon.icon, colors, Some(tint));
            x += advance;
        }
    }

    let avail = (r.right() - x).max(0.0) as i32;
    let body = elide_text_right(text, avail, |s| measure_text_i32(painter, s, font.clone()));
    let split = if colored >= text.len() {
        body.len()
    } else {
        colored.min(body.len())
    };
    let split = if body.is_char_boundary(split) {
        split
    } else {
        body.len()
    };
    let mut job = LayoutJob::default();
    let (head, tail) = body.split_at(split);
    if !head.is_empty() {
        job.append(head, 0.0, TextFormat::simple(font.clone(), fg.service));
    }
    if !tail.is_empty() {
        job.append(tail, 0.0, TextFormat::simple(font.clone(), fg.text));
    }
    if job.sections.is_empty() {
        return;
    }
    let galley = painter.layout_job(job);
    painter.galley(Pos2::new(x, line_top), galley, fg.text);
}
