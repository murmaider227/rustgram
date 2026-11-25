//! Sub-boxes for WP-69 peer management parity:
//! - Chat History Visibility for New Members (`edit_peer_history_visibility_box.cpp`,
//!   `channels.togglePreHistoryHidden`)
//! - Group Stickers & Custom Emoji Pack selector (`edit_peer_info_box.cpp:900-926`,
//!   `channels.setStickers`, `channels.setEmojiStickers`)

use egui::{FontId, Ui};
use tg_app_core::info::backend::{ChannelStickerSetRef, InfoBackend};
use tg_app_core::stand_in::types::PeerId;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckboxStyle, Colors, FlatLabelStyle, InputFieldStyle, RadioStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Radio;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

/// Radio choice matching tdesktop's `HistoryVisibility` (`edit_peer_history_visibility_box.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryVisibility {
    /// New members can read messages sent before joining (`pre_history_hidden = false`).
    Visible,
    /// New members cannot read earlier messages (`pre_history_hidden = true`).
    Hidden,
}

impl HistoryVisibility {
    /// Converts from `pre_history_hidden` boolean flag.
    #[must_use]
    pub const fn from_hidden(hidden: bool) -> Self {
        if hidden { Self::Hidden } else { Self::Visible }
    }

    /// Returns `true` when pre-history is hidden.
    #[must_use]
    pub const fn is_hidden(self) -> bool {
        matches!(self, Self::Hidden)
    }
}

/// Modal box editing "Chat history for new members" (`EditPeerHistoryVisibilityBox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditHistoryVisibilityBoxState {
    /// Target supergroup/group peer ID.
    pub peer_id: PeerId,
    /// `true` for legacy basic groups (displays the 100-message legacy caption).
    pub is_legacy: bool,
    /// Currently selected radio value.
    pub visibility: HistoryVisibility,
    /// Inline RPC error, if submission failed.
    pub error: Option<String>,
}

impl EditHistoryVisibilityBoxState {
    /// Creates a new history visibility box initialized from `pre_history_hidden`.
    #[must_use]
    pub const fn new(peer_id: PeerId, is_legacy: bool, pre_history_hidden: bool) -> Self {
        Self {
            peer_id,
            is_legacy,
            visibility: HistoryVisibility::from_hidden(pre_history_hidden),
            error: None,
        }
    }

    /// Persists the chosen visibility via `channels.togglePreHistoryHidden`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        match backend.toggle_pre_history_hidden(self.peer_id, self.visibility.is_hidden()) {
            Ok(()) => {
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_manage_history_visibility_title())
            .width(st::box_width.0 as f32 + 32.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
    }

    /// Renders the Visible / Hidden radio options and tdesktop explanatory labels.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let cb_st = CheckboxStyle::box_checkbox(colors);
        let rad_st = RadioStyle::default_radio(colors);

        let visible_lbl = tr::lng_manage_history_visibility_shown();
        Radio::new(
            &mut self.visibility,
            HistoryVisibility::Visible,
            &visible_lbl,
            cb_st,
            rad_st,
        )
        .show(ui);
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            FlatLabel::plain(
                &tr::lng_manage_history_visibility_shown_about(),
                FlatLabelStyle::sub_text(colors),
            )
            .id_salt("hist_vis_shown_about")
            .show(ui);
        });

        ui.add_space(12.0);
        let hidden_lbl = tr::lng_manage_history_visibility_hidden();
        Radio::new(
            &mut self.visibility,
            HistoryVisibility::Hidden,
            &hidden_lbl,
            cb_st,
            rad_st,
        )
        .show(ui);
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            let hidden_about = if self.is_legacy {
                tr::lng_manage_history_visibility_hidden_legacy()
            } else {
                tr::lng_manage_history_visibility_hidden_about()
            };
            FlatLabel::plain(&hidden_about, FlatLabelStyle::sub_text(colors))
                .id_salt("hist_vis_hidden_about")
                .show(ui);
        });

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

/// Modal box assigning or clearing the group's official Sticker Pack (`channels.setStickers`)
/// and Custom Emoji Pack (`channels.setEmojiStickers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditGroupStickersBoxState {
    /// Target supergroup peer ID.
    pub peer_id: PeerId,
    /// Draft short name for the group sticker set (empty clears it).
    pub sticker_short_name: String,
    /// Draft short name for the group custom-emoji set (empty clears it).
    pub emoji_short_name: String,
    /// Initial sticker set short name (for dirty tracking).
    initial_sticker_short_name: String,
    /// Initial emoji set short name (for dirty tracking).
    initial_emoji_short_name: String,
    /// Inline RPC error, if submission failed.
    pub error: Option<String>,
}

impl EditGroupStickersBoxState {
    /// Creates a new group stickers / custom-emoji pack editor initialized from cached refs.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        current_stickers: Option<&ChannelStickerSetRef>,
        current_emoji: Option<&ChannelStickerSetRef>,
    ) -> Self {
        let sticker_short_name = sticker_ref_to_input(current_stickers);
        let emoji_short_name = sticker_ref_to_input(current_emoji);
        Self {
            peer_id,
            initial_sticker_short_name: sticker_short_name.clone(),
            initial_emoji_short_name: emoji_short_name.clone(),
            sticker_short_name,
            emoji_short_name,
            error: None,
        }
    }

    /// Saves updated sticker and/or emoji sets to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let next_sticker = normalize_pack_slug(&self.sticker_short_name);
        let next_emoji = normalize_pack_slug(&self.emoji_short_name);

        let sticker_changed = next_sticker != normalize_pack_slug(&self.initial_sticker_short_name);
        let emoji_changed = next_emoji != normalize_pack_slug(&self.initial_emoji_short_name);

        if sticker_changed || !emoji_changed {
            let set_ref = slug_to_sticker_ref(&next_sticker);
            if let Err(err) = backend.set_channel_sticker_set(self.peer_id, set_ref) {
                self.error = Some(err.clone());
                return Err(err);
            }
            self.initial_sticker_short_name = next_sticker.clone();
            self.sticker_short_name = next_sticker;
        }
        if emoji_changed {
            let emoji_ref = slug_to_sticker_ref(&next_emoji);
            if let Err(err) = backend.set_channel_emoji_sticker_set(self.peer_id, emoji_ref) {
                self.error = Some(err.clone());
                return Err(err);
            }
            self.initial_emoji_short_name = next_emoji.clone();
            self.emoji_short_name = next_emoji;
        }
        self.error = None;
        Ok(())
    }

    /// Clears both the group sticker pack and custom-emoji pack (`ChannelStickerSetRef::Empty`).
    pub fn clear_all(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        if let Err(err) = backend.set_channel_sticker_set(self.peer_id, ChannelStickerSetRef::Empty)
        {
            self.error = Some(err.clone());
            return Err(err);
        }
        if let Err(err) =
            backend.set_channel_emoji_sticker_set(self.peer_id, ChannelStickerSetRef::Empty)
        {
            self.error = Some(err.clone());
            return Err(err);
        }
        self.sticker_short_name.clear();
        self.emoji_short_name.clear();
        self.initial_sticker_short_name.clear();
        self.initial_emoji_short_name.clear();
        self.error = None;
        Ok(())
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, tr::lng_group_stickers())
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()));
        if !self.sticker_short_name.trim().is_empty() || !self.emoji_short_name.trim().is_empty() {
            modal = modal.with_left_button(BoxButton::attention("clear_stickers", "Remove Packs"));
        }
        modal
    }

    /// Renders the group sticker set & group emoji set short-name inputs.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let input_st = InputFieldStyle::default_input(colors);

        FlatLabel::plain(
            &tr::lng_stickers_group_set(),
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("group_stickers_hdr")
        .show(ui);
        ui.add_space(4.0);
        InputField::single_line(
            "group_sticker_set_slug",
            &mut self.sticker_short_name,
            "Sticker pack short name (e.g. rust_core_stickers)",
            input_st,
        )
        .show(ui);
        ui.add_space(4.0);
        FlatLabel::plain(
            &tr::lng_group_stickers_description(),
            FlatLabelStyle::sub_text(colors),
        )
        .id_salt("group_stickers_desc")
        .show(ui);

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(8.0);

        FlatLabel::plain(
            "Group Custom Emoji Pack",
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("group_emoji_hdr")
        .show(ui);
        ui.add_space(4.0);
        InputField::single_line(
            "group_emoji_set_slug",
            &mut self.emoji_short_name,
            "Custom emoji pack short name (optional)",
            input_st,
        )
        .show(ui);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Leave a field blank to clear that pack for this group.")
                .font(FontId::new(11.5, family_regular()))
                .color(colors.color("windowSubTextFg")),
        );

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

/// Formats a [`ChannelStickerSetRef`] into a concise badge string for `EditPeerInfoBox`.
#[must_use]
pub fn format_sticker_set_badge(set: Option<&ChannelStickerSetRef>) -> String {
    match set {
        Some(ChannelStickerSetRef::ShortName(s)) if !s.trim().is_empty() => format!("@{s}"),
        Some(ChannelStickerSetRef::Id { id, .. }) => format!("#{id}"),
        _ => tr::lng_group_stickers_add(),
    }
}

fn sticker_ref_to_input(set: Option<&ChannelStickerSetRef>) -> String {
    match set {
        Some(ChannelStickerSetRef::ShortName(s)) => s.clone(),
        _ => String::new(),
    }
}

fn normalize_pack_slug(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("https://t.me/addstickers/")
        .or_else(|| trimmed.strip_prefix("https://t.me/addemoji/"))
        .or_else(|| trimmed.strip_prefix("t.me/addstickers/"))
        .or_else(|| trimmed.strip_prefix("t.me/addemoji/"))
        .unwrap_or(trimmed);
    stripped.trim_start_matches('@').trim().to_owned()
}

fn slug_to_sticker_ref(slug: &str) -> ChannelStickerSetRef {
    if slug.is_empty() {
        ChannelStickerSetRef::Empty
    } else {
        ChannelStickerSetRef::ShortName(slug.to_owned())
    }
}
