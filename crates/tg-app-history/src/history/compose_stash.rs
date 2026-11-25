//! Layer-233 compose stash (`Data::ComposeStash`, `HistoryView::Controls::StashManager`,
//! and `HistoryView::Controls::StashHintManager`).
//!
//! Lets users stash an in-progress draft (text + entities, reply/quote, forward draft, or
//! prepared files) via `Ctrl+S` or the floating corner stash button, then swap it back in,
//! send it directly, send silently, schedule it, or discard it from the stash context menu.
//!
//! Port origin: `Telegram/SourceFiles/data/data_compose_stash.h`,
//! `Telegram/SourceFiles/history/view/controls/history_view_compose_stash.{h,cpp}`, and
//! `Telegram/SourceFiles/history/view/controls/history_view_compose_stash_hint.{h,cpp}`.

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};

use crate::history::rich_compose::RichThreadDraft;
use crate::history::theme::HistoryColors;

pub use tg_app_core::core_types::dto::compose_stash::*;

/// Moves the prepared files and caption out of an [`crate::history::compose_boxes::AttachFilesBoxState`]
/// (`SendFilesBox::stash` + `StashManager::takeFromBox`) into a [`ComposeStash`].
///
/// Mirrors tdesktop's limits (`STASH_FILES_LIMIT` = 100 files,
/// `STASH_FILES_MEMORY_LIMIT` = 256 MiB) and clears spoiler flags as well as the box's
/// candidate/prepared lists when stashing succeeds.
pub fn take_stash_from_box(
    box_state: &mut crate::history::compose_boxes::AttachFilesBoxState,
) -> Result<ComposeStash, String> {
    if box_state.prepared.files.is_empty() {
        return Err("No files to stash.".to_owned());
    }
    let total_bytes: u64 = box_state
        .prepared
        .files
        .iter()
        .fold(0u64, |acc, f| acc.saturating_add(f.size.max(0) as u64));
    if box_state.prepared.files.len() > STASH_FILES_LIMIT || total_bytes > STASH_FILES_MEMORY_LIMIT
    {
        return Err("Too many files to stash (limit: 100 files / 256 MB).".to_owned());
    }

    // Match `SendFilesBox::stash()` (`send_files_box.cpp:2972-2992`): reset spoiler flag and
    // move files + caption out of the box.
    box_state.spoiler = false;
    let caption = std::mem::take(&mut box_state.caption);
    let prepared_files = std::mem::take(&mut box_state.prepared.files);
    box_state.prepared.files_to_process.clear();
    box_state.candidates.clear();
    box_state.edited_jpegs.clear();
    box_state.edited_videos.clear();
    box_state.last_errors.clear();

    let files: Vec<StashedFile> = prepared_files
        .into_iter()
        .map(|file| {
            let name = file
                .path
                .rsplit(['/', '\\'])
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(&file.path)
                .to_owned();
            StashedFile {
                name,
                path: Some(file.path),
                size_bytes: file.size.max(0) as u64,
                mime_type: file.mime,
            }
        })
        .collect();

    let trimmed_caption = caption.trim();
    let draft = if trimmed_caption.is_empty() {
        RichThreadDraft::default()
    } else {
        RichThreadDraft {
            text: crate::history::text_bridge::parse_compose_markdown(trimmed_caption),
            ..RichThreadDraft::default()
        }
    };

    Ok(ComposeStash {
        draft,
        reply_author: None,
        reply_preview: None,
        forward: None,
        files,
    })
}

/// Actions emitted by the corner stash button and its right-click menu (`FillStashMenu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashMenuAction {
    /// Primary click or `Ctrl+S`: swap current composer content with stashed content.
    Exchange,
    /// Context menu: send the stashed draft immediately.
    Send,
    /// Context menu: send the stashed draft silently.
    SendSilent,
    /// Context menu: open the schedule picker for the stashed draft.
    Schedule,
    /// Context menu: delete/discard the stashed draft.
    Delete,
}

/// Cut-then-paste detector that teaches the user about `Ctrl+S` compose stashing
/// (`HistoryView::Controls::StashHintManager`).
#[derive(Debug, Clone, Default)]
pub struct StashHintManager {
    memory: Option<CutMemory>,
    peak_length: usize,
    peak_hash: u64,
    shown_count: u32,
}

#[derive(Debug, Clone, Copy)]
struct CutMemory {
    hash: u64,
    length: usize,
    at_ms: u64,
}

impl StashHintManager {
    /// Creates a new hint manager with `shown_count = 0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the hint may still be displayed (`shown_count < STASH_HINT_MAX_SHOWS`).
    #[must_use]
    pub const fn should_show_hint(&self) -> bool {
        self.shown_count < STASH_HINT_MAX_SHOWS
    }

    /// Marks the stash feature as discovered/used, permanently suppressing future hints.
    pub fn mark_used(&mut self) {
        self.shown_count = STASH_HINT_MAX_SHOWS;
        self.memory = None;
    }

    /// Tracks a compose field text change (`StashHintManager::trackChange`).
    ///
    /// Returns `Some(hint_message)` when the user cleared >= [`STASH_HINT_MIN_LENGTH`] characters
    /// and then pasted the exact same text back within [`STASH_HINT_MEMORY_TTL_MS`].
    pub fn track_change(
        &mut self,
        text: &str,
        user_driven: bool,
        can_use: bool,
        now_ms: u64,
    ) -> Option<String> {
        if let Some(mem) = self.memory
            && now_ms.saturating_sub(mem.at_ms) > STASH_HINT_MEMORY_TTL_MS
        {
            self.memory = None;
        }
        let len = text.chars().count();
        if !user_driven {
            if len == 0 {
                self.peak_length = 0;
            } else {
                self.peak_length = len;
                self.peak_hash = fnv1a_64(text.as_bytes());
            }
            return None;
        }
        if len > 0 {
            let h = fnv1a_64(text.as_bytes());
            if len > self.peak_length {
                self.peak_length = len;
                self.peak_hash = h;
            }
            if let Some(mem) = self.memory
                && len == mem.length
                && h == mem.hash
                && can_use
            {
                self.memory = None;
                if self.should_show_hint() {
                    self.shown_count += 1;
                    return Some(tg_lang::tr::lng_stash_hint("Ctrl+S"));
                }
            }
            return None;
        }
        if self.peak_length >= STASH_HINT_MIN_LENGTH {
            self.memory = Some(CutMemory {
                hash: self.peak_hash,
                length: self.peak_length,
                at_ms: now_ms,
            });
        }
        self.peak_length = 0;
        None
    }
}

/// Paints the floating circular compose-stash corner button (`CornerButtonType::Stash`)
/// and its right-click popup menu (`Send`, `Send Silently`, `Schedule`, `Delete`).
///
/// Positioned at the bottom-right of `list_rect`, stacked above the scroll-to-bottom button
/// when `has_scroll_down_btn` is `true`.
pub fn paint_stash_corner_button(
    ui: &mut Ui,
    list_rect: Rect,
    has_scroll_down_btn: bool,
    stash: &ComposeStash,
    menu_open: &mut bool,
    colors: &HistoryColors,
) -> Option<StashMenuAction> {
    let bottom_offset = if has_scroll_down_btn { 82.0 } else { 32.0 };
    paint_stash_corner_button_at_offset(ui, list_rect, bottom_offset, stash, menu_open, colors)
}

/// Paints the floating circular compose-stash corner button at an explicit `bottom_offset`
/// (used when unread `@`/heart/poll jump FABs are also stacked in the bottom-right corner).
pub fn paint_stash_corner_button_at_offset(
    ui: &mut Ui,
    list_rect: Rect,
    bottom_offset: f32,
    stash: &ComposeStash,
    menu_open: &mut bool,
    colors: &HistoryColors,
) -> Option<StashMenuAction> {
    let center = Pos2::new(list_rect.right() - 32.0, list_rect.bottom() - bottom_offset);
    let btn_rect = Rect::from_center_size(center, Vec2::splat(40.0));
    let resp = ui.interact(
        btn_rect,
        Id::unique("tg_hist_compose_stash_fab"),
        Sense::click(),
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Restore stashed draft"));

    let bg = if resp.hovered() || *menu_open {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        colors.bar_bg_over
    } else {
        colors.bar_bg
    };

    // Subtle elevation shadow + circular badge
    ui.painter().circle_filled(
        center + Vec2::new(0.0, 2.0),
        20.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 32),
    );
    ui.painter().circle_filled(center, 20.0, bg);
    ui.painter()
        .circle_stroke(center, 20.0, Stroke::new(1.0, colors.divider));

    // Stash drawer/attachment icon
    let stash_icon = if stash.has_files() {
        tg_ui::tg_style::st::history_attach.icon
    } else {
        tg_ui::tg_style::st::menu_icon_archive
    };
    tg_app_core::ui_icons::paint_style_tinted(
        ui.painter(),
        Rect::from_center_size(center, Vec2::splat(18.0)),
        stash_icon,
        colors.accent,
    );

    let mut action = None;
    let tooltip = format!(
        "Stashed: {} (Click or Ctrl+S to swap, Right-click for actions)",
        stash.preview_label()
    );
    if resp.secondary_clicked() {
        *menu_open = !*menu_open;
    } else if resp.on_hover_text(tooltip).clicked() {
        *menu_open = false;
        action = Some(StashMenuAction::Exchange);
    }

    if *menu_open {
        let menu_w = 156.0;
        let row_h = 28.0;
        let items = [
            ("Send Message", StashMenuAction::Send, false),
            ("Send Silently", StashMenuAction::SendSilent, false),
            ("Schedule Message", StashMenuAction::Schedule, false),
            ("Delete Stash", StashMenuAction::Delete, true),
        ];
        let menu_h = items.len() as f32 * row_h + 8.0;
        let menu_rect = Rect::from_min_size(
            Pos2::new(
                (btn_rect.left() - menu_w - 6.0).max(list_rect.left() + 8.0),
                (btn_rect.bottom() - menu_h).max(list_rect.top() + 8.0),
            ),
            Vec2::new(menu_w, menu_h),
        );
        ui.painter().rect_filled(
            menu_rect.translate(Vec2::new(0.0, 2.0)),
            CornerRadius::same(8),
            Color32::from_rgba_unmultiplied(0, 0, 0, 44),
        );
        ui.painter()
            .rect_filled(menu_rect, CornerRadius::same(8), colors.bar_bg);
        ui.painter().rect_stroke(
            menu_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, colors.divider),
            egui::StrokeKind::Inside,
        );

        for (idx, (label, act, is_danger)) in items.iter().enumerate() {
            let item_rect = Rect::from_min_size(
                Pos2::new(
                    menu_rect.left() + 4.0,
                    menu_rect.top() + 4.0 + idx as f32 * row_h,
                ),
                Vec2::new(menu_w - 8.0, row_h),
            );
            let item_resp = ui.interact(
                item_rect,
                Id::unique(("tg_hist_stash_menu_item", idx)),
                Sense::click(),
            );
            item_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, *label));
            if item_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(item_rect, CornerRadius::same(5), colors.bar_bg_over);
            }
            let fg = if *is_danger {
                Color32::from_rgb(0xE0, 0x48, 0x48)
            } else {
                colors.text_fg
            };
            ui.painter().text(
                Pos2::new(item_rect.left() + 10.0, item_rect.center().y),
                Align2::LEFT_CENTER,
                *label,
                FontId::proportional(12.5),
                fg,
            );
            if item_resp.clicked() {
                *menu_open = false;
                action = Some(*act);
            }
        }
    }

    action
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
