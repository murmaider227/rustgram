//! Sticker and custom-emoji set preview modal box (`StickerSetBox`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/sticker_set_box.h`
//! and `Telegram/SourceFiles/boxes/sticker_set_box.cpp`.

use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Role,
    Sense, Stroke, StrokeKind, TextEdit, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;

use tg_app_core::session_api::{StickerSetPreview, StickerSetPreviewItem};

/// Width of the sticker set preview modal (`st::boxWideWidth` / ~384px).
const STICKER_SET_BOX_WIDTH: f32 = 384.0;
/// Number of grid columns for standard sticker packs (`kStickersPerRow = 5` in `sticker_set_box.cpp`).
const STICKERS_PER_ROW: usize = 5;
/// Number of grid columns for custom-emoji packs (`kEmojiPerRow = 7` in `sticker_set_box.cpp`).
const EMOJI_PER_ROW: usize = 7;
/// Maximum character length for a pack title (`kMaxSetNameLength = 50` in `sticker_set_box.cpp:327`).
pub const MAX_STICKER_SET_TITLE_LEN: usize = 50;

/// Owner pack management actions emitted by [`StickerSetBoxState`] (`sticker_set_box.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StickerSetOwnerAction {
    /// Owner renamed the pack (`stickers.renameStickerSet`).
    RenamePack {
        /// Numeric sticker set ID.
        set_id: u64,
        /// Pack short name slug.
        short_name: String,
        /// Validated new pack title.
        new_title: String,
    },
    /// Owner moved a sticker entry to `new_position` (`stickers.changeStickerPosition`).
    ReorderSticker {
        /// Numeric sticker set ID.
        set_id: u64,
        /// Pack short name slug.
        short_name: String,
        /// Moved sticker document ID.
        document_id: u64,
        /// Zero-based destination index inside the pack.
        new_position: usize,
    },
    /// Owner removed a sticker entry from the pack (`stickers.removeStickerFromSet`).
    RemoveSticker {
        /// Numeric sticker set ID.
        set_id: u64,
        /// Pack short name slug.
        short_name: String,
        /// Removed sticker document ID.
        document_id: u64,
    },
    /// Owner confirmed permanently deleting the entire pack (`stickers.deleteStickerSet`).
    DeletePack {
        /// Numeric sticker set ID.
        set_id: u64,
        /// Pack short name slug.
        short_name: String,
    },
}

/// Interactive state for the sticker / custom-emoji pack preview modal (`StickerSetBox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerSetBoxState {
    /// Numeric sticker set ID from Telegram (`StickerSet.id`).
    pub set_id: u64,
    /// Pack short name slug (`t.me/addstickers/<short_name>`).
    pub short_name: String,
    /// Human-readable pack title.
    pub title: String,
    /// Total sticker or custom-emoji count declared by the set.
    pub count: i32,
    /// `true` when this is a custom-emoji pack (`addemoji`), `false` for stickers (`addstickers`).
    pub is_emoji: bool,
    /// `true` when the pack is currently installed by the user.
    pub installed: bool,
    /// `true` when the pack is currently archived.
    pub archived: bool,
    /// `true` when the pack is an official Telegram pack.
    pub official: bool,
    /// `true` when the current account created/owns this pack (`_amSetCreator` in `sticker_set_box.cpp`).
    pub is_owner: bool,
    /// `true` while the owner is actively reordering or removing sticker entries.
    pub reorder_mode: bool,
    /// `true` while the owner inline rename editor is expanded.
    pub rename_open: bool,
    /// Draft title text inside the owner rename input.
    pub rename_draft: String,
    /// Validation error shown under the owner rename input.
    pub rename_error: Option<String>,
    /// `true` while awaiting explicit owner confirmation before deleting the whole pack.
    pub pending_delete_pack: bool,
    /// Preview items in the pack grid.
    pub items: Vec<StickerSetPreviewItem>,
    /// Currently selected grid index for detail inspection.
    pub selected_index: Option<usize>,
    /// Queued owner management actions drained by [`crate::boxes::BoxHost`].
    pub pending_owner_actions: Vec<StickerSetOwnerAction>,
}

impl StickerSetBoxState {
    /// Builds a [`StickerSetBoxState`] from a resolved [`StickerSetPreview`].
    #[must_use]
    pub fn from_preview(preview: StickerSetPreview) -> Self {
        let title = preview.title;
        Self {
            set_id: preview.set_id,
            short_name: preview.short_name,
            rename_draft: title.clone(),
            title,
            count: preview.count,
            is_emoji: preview.is_emoji,
            installed: preview.installed,
            archived: preview.archived,
            official: preview.official,
            is_owner: false,
            reorder_mode: false,
            rename_open: false,
            rename_error: None,
            pending_delete_pack: false,
            items: preview.items,
            selected_index: None,
            pending_owner_actions: Vec::new(),
        }
    }

    /// Marks whether this pack is owned by the current user (`_amSetCreator`).
    #[must_use]
    pub fn with_owner(mut self, is_owner: bool) -> Self {
        self.is_owner = is_owner;
        if !is_owner {
            self.reorder_mode = false;
            self.rename_open = false;
            self.pending_delete_pack = false;
        }
        self
    }

    /// Refreshes modal metadata and items from an authoritative server [`StickerSetPreview`]
    /// while preserving owner edit state (`is_owner`, `reorder_mode`).
    pub fn apply_preview(&mut self, preview: StickerSetPreview) {
        self.set_id = preview.set_id;
        self.short_name = preview.short_name;
        self.title = preview.title;
        if !self.rename_open {
            self.rename_draft = self.title.clone();
        }
        self.count = preview.count;
        self.is_emoji = preview.is_emoji;
        self.installed = preview.installed;
        self.archived = preview.archived;
        self.official = preview.official;
        self.items = preview.items;
        self.selected_index = self.selected_index.and_then(|idx| {
            if self.items.is_empty() {
                None
            } else {
                Some(idx.min(self.items.len() - 1))
            }
        });
    }

    /// Drains any pending [`StickerSetOwnerAction`] emitted during [`Self::show_body`].
    pub fn take_owner_actions(&mut self) -> Vec<StickerSetOwnerAction> {
        std::mem::take(&mut self.pending_owner_actions)
    }

    /// Enters or exits owner reorder mode (`Inner::setReorderState`).
    pub fn set_reorder_mode(&mut self, enabled: bool) {
        if self.is_owner {
            self.reorder_mode = enabled;
            if enabled {
                self.rename_open = false;
                self.pending_delete_pack = false;
            }
        } else {
            self.reorder_mode = false;
        }
    }

    /// Moves the sticker at `idx` by `delta` (`-1` left, `+1` right) and queues a reorder action.
    pub fn move_sticker(&mut self, idx: usize, delta: isize) -> bool {
        if !self.is_owner || idx >= self.items.len() {
            return false;
        }
        let target = idx as isize + delta;
        if target < 0 || (target as usize) >= self.items.len() {
            return false;
        }
        let new_idx = target as usize;
        let doc_id = self.items[idx].document_id;
        self.items.swap(idx, new_idx);
        self.selected_index = Some(new_idx);
        self.pending_owner_actions
            .push(StickerSetOwnerAction::ReorderSticker {
                set_id: self.set_id,
                short_name: self.short_name.clone(),
                document_id: doc_id,
                new_position: new_idx,
            });
        true
    }

    /// Removes the sticker at `idx` from the owned pack and queues a remove action.
    pub fn remove_sticker_at(&mut self, idx: usize) -> bool {
        if !self.is_owner || idx >= self.items.len() {
            return false;
        }
        let removed = self.items.remove(idx);
        self.count = self.items.len() as i32;
        self.selected_index = if self.items.is_empty() {
            None
        } else {
            Some(idx.min(self.items.len() - 1))
        };
        self.pending_owner_actions
            .push(StickerSetOwnerAction::RemoveSticker {
                set_id: self.set_id,
                short_name: self.short_name.clone(),
                document_id: removed.document_id,
            });
        true
    }

    /// Validates and commits `rename_draft`, queuing [`StickerSetOwnerAction::RenamePack`].
    pub fn submit_rename(&mut self) -> Result<(), String> {
        if !self.is_owner {
            return Err("Only pack owners can rename a sticker set.".to_owned());
        }
        let trimmed = self.rename_draft.trim();
        if trimmed.is_empty() {
            let msg = "Enter a pack name before saving.".to_owned();
            self.rename_error = Some(msg.clone());
            return Err(msg);
        }
        if trimmed.chars().count() > MAX_STICKER_SET_TITLE_LEN {
            let msg = format!("Pack name must be at most {MAX_STICKER_SET_TITLE_LEN} characters.");
            self.rename_error = Some(msg.clone());
            return Err(msg);
        }
        self.title = trimmed.to_owned();
        self.rename_draft = self.title.clone();
        self.rename_open = false;
        self.rename_error = None;
        self.pending_owner_actions
            .push(StickerSetOwnerAction::RenamePack {
                set_id: self.set_id,
                short_name: self.short_name.clone(),
                new_title: self.title.clone(),
            });
        Ok(())
    }

    /// Returns the shareable `https://t.me/...` link for this pack.
    #[must_use]
    pub fn share_url(&self) -> String {
        let prefix = if self.is_emoji {
            "addemoji"
        } else {
            "addstickers"
        };
        format!("https://t.me/{prefix}/{}", self.short_name)
    }

    /// Returns `true` when the primary action should be Remove rather than Install.
    #[must_use]
    pub const fn is_removable(&self) -> bool {
        self.installed && !self.archived
    }

    /// Effective item count displayed on the modal title and primary button.
    #[must_use]
    pub fn display_count(&self) -> usize {
        if self.count > 0 {
            self.count as usize
        } else {
            self.items.len()
        }
    }

    /// Primary install/remove button label.
    #[must_use]
    pub fn primary_action_label(&self) -> String {
        let n = self.display_count();
        if self.is_removable() {
            if self.is_emoji {
                "Remove Emoji".to_owned()
            } else {
                "Remove Stickers".to_owned()
            }
        } else if self.is_emoji {
            if n > 0 {
                format!("Add {n} Emoji")
            } else {
                "Add Emoji".to_owned()
            }
        } else if n > 0 {
            format!("Add {n} Stickers")
        } else {
            "Add Stickers".to_owned()
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor with Install/Remove, Copy Link, Done (when
    /// reordering), and Close buttons (`StickerSetBox::updateButtons`).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let n = self.display_count();
        let unit = if self.is_emoji {
            "emoji"
        } else if n == 1 {
            "sticker"
        } else {
            "stickers"
        };
        let mut modal = BoxModal::new(id, &self.title)
            .additional_title(format!("{n} {unit}"))
            .width(STICKER_SET_BOX_WIDTH);
        modal.max_content_height = 460.0;

        if self.is_owner && self.reorder_mode {
            return modal.add_button(BoxButton::default_btn("done_reorder", tr::lng_box_done()));
        }

        modal = modal.add_button(BoxButton::default_btn("close", tr::lng_close()));

        if !self.short_name.is_empty() {
            modal = modal.with_left_button(BoxButton::default_btn("copy_link", "Copy Link"));
        }

        let primary_label = self.primary_action_label();
        if self.is_removable() {
            modal = modal.add_button(BoxButton::attention("remove_set", primary_label));
        } else {
            modal = modal.add_button(BoxButton::default_btn("install_set", primary_label));
        }

        modal
    }

    /// Renders pack metadata, owner management controls (strictly when `self.is_owner`),
    /// the interactive sticker/emoji preview grid, and selection footer.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_fg = colors.color("windowActiveTextFg");
        let attention_fg = colors.color("attentionButtonFg");
        let tile_bg = colors.color("windowBgOver");
        let divider = colors.color("shadowFg");

        // Compact metadata header strip.
        ui.horizontal_wrapped(|ui| {
            let kind_label = if self.is_emoji {
                "Custom Emoji Pack"
            } else {
                "Sticker Pack"
            };
            ui.label(
                egui::RichText::new(kind_label)
                    .font(font_id_from_style(st::semibold_font))
                    .color(bold_fg),
            );
            if !self.short_name.is_empty() {
                ui.label(
                    egui::RichText::new(format!("· @{}", self.short_name))
                        .font(font_id_from_style(st::normal_font))
                        .color(sub_fg),
                );
            }
            if self.is_owner {
                ui.label(
                    egui::RichText::new("· Owner")
                        .font(font_id_from_style(st::semibold_font))
                        .color(active_fg),
                );
            }
            if self.official {
                ui.label(
                    egui::RichText::new("· Official")
                        .font(font_id_from_style(st::normal_font))
                        .color(active_fg),
                );
            }
            if self.is_removable() {
                ui.label(
                    egui::RichText::new("· Installed")
                        .font(font_id_from_style(st::normal_font))
                        .color(active_fg),
                );
            } else if self.archived {
                ui.label(
                    egui::RichText::new("· Archived")
                        .font(font_id_from_style(st::normal_font))
                        .color(sub_fg),
                );
            }
        });

        // Owner management toolbar (`fillSetCreatorMenu` / `fillSetCreatorFooter` in `sticker_set_box.cpp`).
        if self.is_owner {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                let edit_lbl = tr::lng_stickers_context_edit_name();
                if owner_chip_button(ui, &edit_lbl, self.rename_open, false, colors) {
                    self.rename_open = !self.rename_open;
                    if self.rename_open {
                        self.rename_draft = self.title.clone();
                        self.rename_error = None;
                        self.pending_delete_pack = false;
                        self.reorder_mode = false;
                    }
                }
                let reorder_lbl = if self.reorder_mode {
                    tr::lng_box_done()
                } else {
                    tr::lng_stickers_context_reorder()
                };
                if owner_chip_button(ui, &reorder_lbl, self.reorder_mode, false, colors) {
                    self.set_reorder_mode(!self.reorder_mode);
                }
                if owner_chip_button(ui, "Delete Pack", self.pending_delete_pack, true, colors) {
                    self.pending_delete_pack = !self.pending_delete_pack;
                    if self.pending_delete_pack {
                        self.rename_open = false;
                        self.reorder_mode = false;
                    }
                }
            });

            if self.rename_open {
                ui.add_space(6.0);
                Frame::NONE
                    .fill(tile_bg)
                    .stroke(Stroke::new(1.0, divider))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 4.0).max(180.0));
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(tr::lng_stickers_context_edit_name())
                                    .font(font_id_from_style(st::semibold_font))
                                    .color(bold_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{}/{MAX_STICKER_SET_TITLE_LEN}",
                                            self.rename_draft.chars().count()
                                        ))
                                        .font(FontId::proportional(11.0))
                                        .color(sub_fg),
                                    );
                                },
                            );
                        });
                        ui.add_space(4.0);
                        ui.add(
                            TextEdit::singleline(&mut self.rename_draft)
                                .desired_width(ui.available_width())
                                .hint_text("Pack name"),
                        );
                        if self.rename_draft.chars().count() > MAX_STICKER_SET_TITLE_LEN {
                            self.rename_draft = self
                                .rename_draft
                                .chars()
                                .take(MAX_STICKER_SET_TITLE_LEN)
                                .collect();
                        }
                        if let Some(ref err) = self.rename_error {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .font(FontId::proportional(11.5))
                                    .color(colors.color("boxTextFgError")),
                            );
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if owner_chip_button(ui, "Save Name", true, false, colors) {
                                let _ = self.submit_rename();
                            }
                            if owner_chip_button(ui, &tr::lng_cancel(), false, false, colors) {
                                self.rename_open = false;
                                self.rename_error = None;
                            }
                        });
                    });
            }

            if self.pending_delete_pack {
                ui.add_space(6.0);
                Frame::NONE
                    .fill(tile_bg)
                    .stroke(Stroke::new(1.0, attention_fg))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 4.0).max(180.0));
                        ui.label(
                            egui::RichText::new(tr::lng_stickers_delete_pack_sure())
                                .font(font_id_from_style(st::normal_font))
                                .color(bold_fg),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if owner_chip_button(ui, "Confirm Delete Pack", true, true, colors) {
                                self.pending_delete_pack = false;
                                self.pending_owner_actions.push(
                                    StickerSetOwnerAction::DeletePack {
                                        set_id: self.set_id,
                                        short_name: self.short_name.clone(),
                                    },
                                );
                            }
                            if owner_chip_button(ui, &tr::lng_cancel(), false, false, colors) {
                                self.pending_delete_pack = false;
                            }
                        });
                    });
            }
        }

        ui.add_space(10.0);

        if self.items.is_empty() {
            let avail_w = ui.available_width().max(200.0);
            let (empty_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 88.0), Sense::hover());
            ui.painter()
                .rect_filled(empty_rect, CornerRadius::same(8), tile_bg);
            ui.painter().text(
                empty_rect.center(),
                Align2::CENTER_CENTER,
                if self.is_emoji {
                    "No custom emoji previews available in this pack."
                } else {
                    "No sticker previews available in this pack."
                },
                font_id_from_style(st::normal_font),
                sub_fg,
            );
            return;
        }

        // Responsive preview grid (`StickerSetBox::Inner`).
        let cols = if self.is_emoji {
            EMOJI_PER_ROW
        } else {
            STICKERS_PER_ROW
        };
        let gap = 6.0;
        let avail_w = ui.available_width().max(220.0);
        let cell_side = ((avail_w - gap * (cols.saturating_sub(1) as f32)) / (cols as f32))
            .clamp(if self.is_emoji { 36.0 } else { 52.0 }, 72.0);

        let total = self.items.len();
        let rows = total.div_ceil(cols);
        let mut clicked_index: Option<usize> = None;
        let mut move_request: Option<(usize, isize)> = None;
        let mut remove_request: Option<usize> = None;

        for row in 0..rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for col in 0..cols {
                    let idx = row * cols + col;
                    if idx >= total {
                        break;
                    }
                    let item = &self.items[idx];
                    let is_selected = self.selected_index == Some(idx);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::splat(cell_side), Sense::click());
                    let tip = format_item_tooltip(item, idx, self.is_emoji);
                    resp.widget_info(|| {
                        WidgetInfo::selected(Role::Button, true, is_selected, &tip)
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    let fill = if is_selected || resp.hovered() {
                        tile_bg
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
                    let stroke = if is_selected || (self.is_owner && self.reorder_mode) {
                        Stroke::new(
                            if is_selected { 1.5 } else { 1.0 },
                            if is_selected { active_fg } else { divider },
                        )
                    } else {
                        Stroke::new(0.75, divider)
                    };
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(8),
                        stroke,
                        StrokeKind::Inside,
                    );

                    if let Some((w, h, ref rgba)) = item.thumb_rgba
                        && w > 0
                        && h > 0
                        && rgba.len() == (w as usize) * (h as usize) * 4
                    {
                        let tex = get_or_upload_preview_texture(
                            ui.ctx(),
                            self.set_id,
                            item.document_id,
                            w,
                            h,
                            rgba,
                        );
                        let img_rect = rect.shrink(4.0);
                        ui.painter().image(
                            tex.id(),
                            img_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        let emblem_rect = if self.is_owner && self.reorder_mode {
                            Rect::from_min_max(
                                Pos2::new(rect.left() + 4.0, rect.top() + 12.0),
                                Pos2::new(rect.right() - 4.0, rect.bottom() - 2.0),
                            )
                        } else {
                            rect
                        };
                        paint_fallback_sticker_emblem(
                            ui.painter(),
                            emblem_rect,
                            idx,
                            self.is_emoji,
                            item.is_premium,
                        );
                    }

                    // Compact format / Premium indicator badge in bottom-right corner.
                    let badge = if item.is_premium {
                        Some("PRO")
                    } else if item.is_video {
                        Some("VID")
                    } else if item.is_animated {
                        Some("TGS")
                    } else {
                        None
                    };
                    if let Some(tag) = badge {
                        let pill_w = 26.0;
                        let pill = Rect::from_min_size(
                            Pos2::new(rect.right() - pill_w - 2.0, rect.bottom() - 14.0),
                            Vec2::new(pill_w, 12.0),
                        );
                        let badge_bg = if item.is_premium {
                            Color32::from_rgb(0x88, 0x62, 0xF8)
                        } else {
                            Color32::from_rgba_unmultiplied(0x18, 0x20, 0x28, 196)
                        };
                        ui.painter()
                            .rect_filled(pill, CornerRadius::same(4), badge_bg);
                        ui.painter().text(
                            pill.center(),
                            Align2::CENTER_CENTER,
                            tag,
                            FontId::new(8.0, family_semibold()),
                            Color32::WHITE,
                        );
                    }

                    // Owner reorder / remove controls overlay when `reorder_mode` is active.
                    let mut overlay_clicked = false;
                    if self.is_owner && self.reorder_mode {
                        let rm_rect = Rect::from_min_size(
                            Pos2::new(rect.right() - 18.0, rect.top() + 2.0),
                            Vec2::splat(16.0),
                        );
                        let rm_resp = ui.interact(
                            rm_rect,
                            ui.make_persistent_id(("sticker_rm", self.set_id, item.document_id)),
                            Sense::click(),
                        );
                        rm_resp.widget_info(|| {
                            WidgetInfo::labeled(
                                Role::Button,
                                true,
                                format!("Remove sticker #{}", idx + 1),
                            )
                        });
                        if rm_resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        ui.painter()
                            .circle_filled(rm_rect.center(), 8.0, attention_fg);
                        ui.painter().text(
                            rm_rect.center(),
                            Align2::CENTER_CENTER,
                            "×",
                            FontId::new(11.0, family_semibold()),
                            Color32::WHITE,
                        );
                        if rm_resp.clicked() {
                            remove_request = Some(idx);
                            overlay_clicked = true;
                        }

                        if idx > 0 {
                            let l_rect = Rect::from_min_size(
                                Pos2::new(rect.left() + 2.0, rect.top() + 2.0),
                                Vec2::splat(16.0),
                            );
                            let l_resp = ui.interact(
                                l_rect,
                                ui.make_persistent_id((
                                    "sticker_left",
                                    self.set_id,
                                    item.document_id,
                                )),
                                Sense::click(),
                            );
                            l_resp.widget_info(|| {
                                WidgetInfo::labeled(
                                    Role::Button,
                                    true,
                                    format!("Move sticker #{} left", idx + 1),
                                )
                            });
                            if l_resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            ui.painter().circle_filled(l_rect.center(), 8.0, active_fg);
                            ui.painter().text(
                                l_rect.center(),
                                Align2::CENTER_CENTER,
                                "‹",
                                FontId::new(11.0, family_semibold()),
                                Color32::WHITE,
                            );
                            if l_resp.clicked() {
                                move_request = Some((idx, -1));
                                overlay_clicked = true;
                            }
                        }

                        if idx + 1 < total {
                            let r_rect = Rect::from_min_size(
                                Pos2::new(rect.left() + 20.0, rect.top() + 2.0),
                                Vec2::splat(16.0),
                            );
                            let r_resp = ui.interact(
                                r_rect,
                                ui.make_persistent_id((
                                    "sticker_right",
                                    self.set_id,
                                    item.document_id,
                                )),
                                Sense::click(),
                            );
                            r_resp.widget_info(|| {
                                WidgetInfo::labeled(
                                    Role::Button,
                                    true,
                                    format!("Move sticker #{} right", idx + 1),
                                )
                            });
                            if r_resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            ui.painter().circle_filled(r_rect.center(), 8.0, active_fg);
                            ui.painter().text(
                                r_rect.center(),
                                Align2::CENTER_CENTER,
                                "›",
                                FontId::new(11.0, family_semibold()),
                                Color32::WHITE,
                            );
                            if r_resp.clicked() {
                                move_request = Some((idx, 1));
                                overlay_clicked = true;
                            }
                        }
                    }

                    if resp.clicked() && !overlay_clicked {
                        clicked_index = Some(idx);
                    }
                    let _ = resp.on_hover_text(tip);
                }
            });
            ui.add_space(gap);
        }

        if let Some(idx) = clicked_index {
            self.selected_index = Some(idx);
        }
        if let Some((idx, delta)) = move_request {
            self.move_sticker(idx, delta);
        }
        if let Some(idx) = remove_request {
            self.remove_sticker_at(idx);
        }

        if let Some(idx) = self.selected_index
            && let Some(item) = self.items.get(idx).cloned()
        {
            ui.add_space(4.0);
            let summary = format_item_tooltip(&item, idx, self.is_emoji);
            ui.label(
                egui::RichText::new(summary)
                    .font(font_id_from_style(st::normal_font))
                    .color(sub_fg),
            );
            if self.is_owner {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                    if idx > 0 && owner_chip_button(ui, "Move Left", false, false, colors) {
                        self.move_sticker(idx, -1);
                    }
                    if idx + 1 < self.items.len()
                        && owner_chip_button(ui, "Move Right", false, false, colors)
                    {
                        self.move_sticker(idx, 1);
                    }
                    if owner_chip_button(ui, "Remove Sticker", false, true, colors) {
                        self.remove_sticker_at(idx);
                    }
                });
            }
        }
    }
}

fn owner_chip_button(
    ui: &mut Ui,
    label: &str,
    active: bool,
    attention: bool,
    colors: &dyn Colors,
) -> bool {
    let fg = if active {
        Color32::WHITE
    } else if attention {
        colors.color("attentionButtonFg")
    } else {
        colors.color("windowFg")
    };
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::new(11.5, family_semibold()), fg);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 18.0, 26.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, active, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if active {
        if attention {
            colors.color("attentionButtonFg")
        } else {
            colors.color("windowBgActive")
        }
    } else if resp.hovered() {
        colors.color("windowBgOver")
    } else {
        Color32::from_rgba_unmultiplied(128, 128, 128, 28)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}

fn paint_fallback_sticker_emblem(
    painter: &egui::Painter,
    rect: Rect,
    idx: usize,
    is_emoji: bool,
    is_premium: bool,
) {
    const PALETTE: [Color32; 6] = [
        Color32::from_rgb(0x3B, 0x82, 0xF6),
        Color32::from_rgb(0x8B, 0x5C, 0xF6),
        Color32::from_rgb(0x22, 0xA0, 0x6B),
        Color32::from_rgb(0xE5, 0xA0, 0x0D),
        Color32::from_rgb(0xEC, 0x48, 0x99),
        Color32::from_rgb(0x06, 0xB6, 0xD4),
    ];
    let fill = if is_premium {
        Color32::from_rgb(0x88, 0x62, 0xF8)
    } else {
        PALETTE[idx % PALETTE.len()]
    };
    let radius = (rect.width().min(rect.height()) * 0.30).max(10.0);
    let center = Pos2::new(rect.center().x, rect.center().y - 2.0);
    painter.circle_filled(center, radius, fill);
    let label = if is_emoji {
        format!("E{}", idx + 1)
    } else {
        format!("#{}", idx + 1)
    };
    painter.text(
        center,
        Align2::CENTER_CENTER,
        label,
        FontId::new(11.5, family_semibold()),
        Color32::WHITE,
    );
}

fn format_item_tooltip(item: &StickerSetPreviewItem, idx: usize, is_emoji: bool) -> String {
    let kind = if is_emoji {
        "Custom emoji"
    } else if item.is_video {
        "Video sticker"
    } else if item.is_animated {
        "Animated sticker"
    } else {
        "Sticker"
    };
    let emoji = if item.alt_emoji.trim().is_empty() {
        format!("#{}", idx + 1)
    } else {
        item.alt_emoji.clone()
    };
    if item.is_premium {
        format!("{emoji} · {kind} (Premium)")
    } else {
        format!("{emoji} · {kind}")
    }
}

fn get_or_upload_preview_texture(
    ctx: &egui::Context,
    set_id: u64,
    doc_id: u64,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> TextureHandle {
    let key = egui::Id::unique(("tg_sticker_set_box_thumb", set_id, doc_id));
    if let Some(existing) = ctx.data(|d| d.get_temp::<TextureHandle>(key)) {
        return existing;
    }
    let image = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba);
    let handle = ctx.load_texture(
        format!("sticker_set_preview_{set_id}_{doc_id}"),
        image,
        TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(key, handle.clone()));
    handle
}
