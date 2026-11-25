//! Attached Sticker / Mask / Custom-Emoji Sets modal box (`messages.getAttachedStickers`).
//!
//! Port origin: `Telegram/SourceFiles/api/api_attached_stickers.cpp`
//! and `Telegram/SourceFiles/boxes/stickers_box.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_app_core::chat_helpers::backend::{
    AttachedStickerSetSummary, AttachedStickersMedia, ChatHelpersBackend,
};
use tg_app_core::session_api::StickerSetPreview;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;

use crate::boxes::sticker_set::StickerSetBoxState;

/// Width of the Attached Stickers modal matching `st::boxWideWidth`.
const ATTACHED_STICKERS_BOX_WIDTH: f32 = 384.0;

/// Outcome of resolving `messages.getAttachedStickers` per `api_attached_stickers.cpp:38-65`.
#[derive(Debug, Clone)]
pub enum AttachedStickersOutcome {
    /// Zero sticker/mask sets were attached to the media (`tr::lng_stickers_not_found()`).
    NotFound,
    /// Exactly one sticker set was attached: open `StickerSetBox` directly.
    SingleSet(StickerSetBoxState),
    /// Multiple sticker sets were attached: show the chooser modal listing every set.
    MultipleSets(AttachedStickersBoxState),
}

/// Classifies already-fetched attached sticker sets into tdesktop's exact branching
/// (`0 -> NotFound`, `1 -> SingleSet`, `>1 -> MultipleSets`).
pub fn classify_attached_stickers(
    sets: Vec<AttachedStickerSetSummary>,
    media_label: impl Into<String>,
    resolve_preview: impl FnOnce(&AttachedStickerSetSummary) -> Option<StickerSetPreview>,
) -> AttachedStickersOutcome {
    if sets.is_empty() {
        return AttachedStickersOutcome::NotFound;
    }
    if sets.len() == 1 {
        let only = &sets[0];
        let preview = resolve_preview(only).unwrap_or_else(|| preview_from_attached_summary(only));
        return AttachedStickersOutcome::SingleSet(StickerSetBoxState::from_preview(preview));
    }
    AttachedStickersOutcome::MultipleSets(AttachedStickersBoxState::new(media_label, sets))
}

/// Fetches attached sticker sets for `media` via [`ChatHelpersBackend::get_attached_stickers`]
/// and mirrors tdesktop's exact branching (`0 -> NotFound`, `1 -> SingleSet`, `>1 -> MultipleSets`).
pub fn resolve_attached_stickers_box(
    media: &AttachedStickersMedia,
    media_label: impl Into<String>,
    chat_helpers: &dyn ChatHelpersBackend,
    resolve_preview: impl FnOnce(&AttachedStickerSetSummary) -> Option<StickerSetPreview>,
) -> Result<AttachedStickersOutcome, String> {
    let sets = chat_helpers.get_attached_stickers(media)?;
    Ok(classify_attached_stickers(
        sets,
        media_label,
        resolve_preview,
    ))
}

/// Builds a fallback [`StickerSetPreview`] from an [`AttachedStickerSetSummary`] when full
/// pack items are not yet fetched.
#[must_use]
pub fn preview_from_attached_summary(summary: &AttachedStickerSetSummary) -> StickerSetPreview {
    let items = summary
        .cover_doc_ids
        .iter()
        .map(|doc_id| tg_app_core::session_api::StickerSetPreviewItem {
            document_id: doc_id.0,
            alt_emoji: if summary.is_masks {
                "🎭".to_owned()
            } else if summary.is_emoji {
                "✨".to_owned()
            } else {
                "🎨".to_owned()
            },
            is_animated: false,
            is_video: false,
            is_premium: false,
            thumb_rgba: None,
        })
        .collect();
    StickerSetPreview {
        set_id: summary.set_id.0,
        access_hash: summary.access_hash as i64,
        title: summary.title.clone(),
        short_name: summary.short_name.clone(),
        count: summary.count,
        is_emoji: summary.is_emoji,
        installed: false,
        archived: false,
        official: false,
        items,
    }
}

/// Interactive action emitted by [`AttachedStickersBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachedStickersAction {
    /// User clicked a sticker/mask/emoji pack row to inspect & install it.
    OpenSet {
        /// Numeric sticker set ID.
        set_id: u64,
        /// Short name slug.
        short_name: String,
        /// Pack title.
        title: String,
        /// `true` when this is a custom-emoji pack.
        is_emoji: bool,
    },
}

/// Modal state listing all sticker/mask/emoji sets attached to a photo or document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedStickersBoxState {
    /// Context label describing the inspected media (e.g. `"Photo"`, `"Profile Photo"`).
    pub media_label: String,
    /// Attached sets returned by `messages.getAttachedStickers`.
    pub sets: Vec<AttachedStickerSetSummary>,
    /// Last error, if opening a set failed.
    pub error: Option<String>,
}

impl AttachedStickersBoxState {
    /// Creates an attached stickers list modal for `sets`.
    #[must_use]
    pub fn new(media_label: impl Into<String>, sets: Vec<AttachedStickerSetSummary>) -> Self {
        Self {
            media_label: media_label.into(),
            sets,
            error: None,
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, tr::lng_stickers_attached_sets())
            .additional_title(self.sets.len().to_string())
            .width(ATTACHED_STICKERS_BOX_WIDTH)
            .add_button(BoxButton::default_btn("close", tr::lng_close()));
        modal.max_content_height = 420.0;
        modal
    }

    /// Renders the list of attached sticker/mask/emoji sets.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
    ) -> Option<AttachedStickersAction> {
        let mut action = None;
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_fg = colors.color("windowActiveTextFg");
        let over_bg = colors.color("windowBgOver");
        let divider = colors.color("shadowFg");

        if !self.media_label.is_empty() {
            ui.label(
                egui::RichText::new(format!("Sticker sets attached to {}", self.media_label))
                    .font(FontId::new(12.5, family_regular()))
                    .color(sub_fg),
            );
            ui.add_space(8.0);
        }

        if self.sets.is_empty() {
            let avail_w = ui.available_width().max(200.0);
            let (empty_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 72.0), Sense::hover());
            ui.painter()
                .rect_filled(empty_rect, CornerRadius::same(8), over_bg);
            ui.painter().text(
                empty_rect.center(),
                Align2::CENTER_CENTER,
                tr::lng_stickers_not_found(),
                font_id_from_style(st::normal_font),
                sub_fg,
            );
            return None;
        }

        let width = ui.available_width().max(220.0);
        for (idx, set) in self.sets.iter().enumerate() {
            let row_h = 56.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
            let kind_tag = if set.is_masks {
                "Masks"
            } else if set.is_emoji {
                "Emoji"
            } else {
                "Stickers"
            };
            let acc_label = format!("{} ({}, {} items)", set.title, kind_tag, set.count.max(0));
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &acc_label));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(8), over_bg);
            }

            let av_rect = Rect::from_center_size(
                Pos2::new(rect.left() + 24.0, rect.center().y),
                Vec2::splat(38.0),
            );
            EmptyUserpic::for_color_index((set.set_id.0 % 7) as u8, &set.title, colors)
                .paint_circle(ui.painter(), av_rect);

            let badge_w = 56.0;
            let badge_rect = Rect::from_center_size(
                Pos2::new(rect.right() - badge_w * 0.5 - 8.0, rect.center().y),
                Vec2::new(badge_w, 24.0),
            );
            ui.painter()
                .rect_filled(badge_rect, CornerRadius::same(12), over_bg);
            ui.painter().text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                "View",
                FontId::new(11.5, family_semibold()),
                active_fg,
            );

            let text_x = av_rect.right() + 12.0;
            let text_max_w = (badge_rect.left() - 8.0 - text_x).max(40.0);
            let text_clip = Rect::from_min_max(
                Pos2::new(text_x, rect.top() + 4.0),
                Pos2::new(text_x + text_max_w, rect.bottom() - 4.0),
            );
            let title_font = font_id_from_style(st::semibold_font);
            let sub_font = FontId::new(11.5, family_regular());
            let title_fit = ellipsize_single_line(ui, &set.title, &title_font, text_max_w);
            let subtitle_raw = if set.short_name.is_empty() {
                format!("{} · {} items", kind_tag, set.count.max(0))
            } else {
                format!(
                    "{} · {} items · @{}",
                    kind_tag,
                    set.count.max(0),
                    set.short_name
                )
            };
            let subtitle_fit = ellipsize_single_line(ui, &subtitle_raw, &sub_font, text_max_w);
            let clip = ui.painter().with_clip_rect(text_clip);
            clip.text(
                Pos2::new(text_x, rect.top() + 9.0),
                Align2::LEFT_TOP,
                title_fit,
                title_font,
                bold_fg,
            );
            clip.text(
                Pos2::new(text_x, rect.top() + 29.0),
                Align2::LEFT_TOP,
                subtitle_fit,
                sub_font,
                sub_fg,
            );

            if idx + 1 < self.sets.len() {
                ui.painter().line_segment(
                    [
                        Pos2::new(text_x, rect.bottom()),
                        Pos2::new(rect.right() - 8.0, rect.bottom()),
                    ],
                    Stroke::new(0.5, divider),
                );
            }

            if resp.clicked() {
                action = Some(AttachedStickersAction::OpenSet {
                    set_id: set.set_id.0,
                    short_name: set.short_name.clone(),
                    title: set.title.clone(),
                    is_emoji: set.is_emoji,
                });
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }

        action
    }
}

/// Truncates `text` with a trailing `…` if its single-line layout exceeds `max_width`.
fn ellipsize_single_line(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
    let fits = |candidate: &str| {
        ui.painter()
            .layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
            <= max_width
    };
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo.midpoint(hi + 1);
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push('…');
        if fits(&candidate) {
            lo = mid;
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}

/// Formats a compact badge color for attached sticker sets.
#[allow(dead_code)]
const fn _unused_color_anchor() -> Color32 {
    Color32::TRANSPARENT
}
