//! Peer Member Rank / Custom Tag Editor (`EditTagControl` & `EditCustomRankBox`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/edit_tag_control.h`
//! - `Telegram/SourceFiles/boxes/peers/edit_tag_control.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_participants_box.cpp` (`SaveMemberRank`, `EditCustomRankBox`)

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_lang::tr;
use tg_text::emoji;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::{EmptyUserpic, peer_name_color};
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{
    InfoBackend, MemberEntry, MemberRole, PeerProfile, is_emoji_fallback_scalar,
};
pub use crate::info::backend::{MAX_PEER_TAG_RANK_LEN, sanitize_peer_tag_rank};
use tg_app_core::stand_in::types::PeerId;

/// Opacity for placeholder message text lines in `EditTagControl::PreviewWidget` (`kTextLinesAlpha`).
const PREVIEW_TEXT_LINES_ALPHA: f32 = 0.12;

/// Fractional widths of the 4 skeleton lines in `TextLinesPart::draw` (`edit_tag_control.cpp:90`).
const PREVIEW_LINE_FRACTIONS: [f32; 4] = [1.0, 0.85, 0.65, 0.4];

/// Role determining badge styling & default fallback label (`HistoryView::BadgeRole`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BadgeRole {
    /// Group creator / owner (`"owner"`).
    Creator,
    /// Group administrator (`"admin"`).
    Admin,
    /// Regular group member (no default badge when rank is empty).
    #[default]
    User,
}

impl BadgeRole {
    /// Derives [`BadgeRole`] from an [`MemberRole`] (`LookupBadgeRole`).
    #[must_use]
    pub const fn from_member_role(role: &MemberRole) -> Self {
        match role {
            MemberRole::Owner { .. } => Self::Creator,
            MemberRole::Admin { .. } => Self::Admin,
            MemberRole::Member | MemberRole::Restricted { .. } => Self::User,
        }
    }

    /// Returns the default badge label shown when the custom rank is empty.
    #[must_use]
    pub const fn default_badge_label(self) -> &'static str {
        match self {
            Self::Creator => "owner",
            Self::Admin => "admin",
            Self::User => "",
        }
    }

    /// Input placeholder matching `edit_tag_control.cpp:369-373`.
    #[must_use]
    pub const fn placeholder(self) -> &'static str {
        match self {
            Self::Creator => "owner",
            Self::Admin => "admin",
            Self::User => "Tag",
        }
    }
}

/// Interactive `EditTagControl` widget (`boxes/peers/edit_tag_control.cpp`):
/// live message-bubble preview with skeleton lines + emoji-filtered 16-char tag input.
#[derive(Debug, Clone)]
pub struct EditTagControl {
    /// Target user's peer ID (for deterministic avatar / name colors).
    pub user_id: PeerId,
    /// Target user's display name.
    pub user_name: String,
    /// Target user's avatar color index (`0..7`).
    pub color_index: u8,
    /// Badge role (`Creator`, `Admin`, or `User`).
    pub role: BadgeRole,
    /// Current editable rank string (emoji stripped, max 16 chars).
    pub rank_input: String,
}

impl EditTagControl {
    /// Creates a new [`EditTagControl`] initialized with `current_rank`.
    #[must_use]
    pub fn new(
        user_id: PeerId,
        user_name: impl Into<String>,
        color_index: u8,
        role: BadgeRole,
        current_rank: &str,
    ) -> Self {
        Self {
            user_id,
            user_name: user_name.into(),
            color_index,
            role,
            rank_input: sanitize_peer_tag_rank(current_rank),
        }
    }

    /// Returns the sanitized current rank string (`EditTagControl::currentRank`).
    #[must_use]
    pub fn current_rank(&self) -> String {
        sanitize_peer_tag_rank(&self.rank_input)
    }

    /// Computes the effective badge text displayed in the live preview bubble.
    #[must_use]
    pub fn effective_badge_text(&self) -> String {
        let trimmed = self.current_rank();
        if trimmed.is_empty() {
            self.role.default_badge_label().to_owned()
        } else {
            trimmed
        }
    }

    /// Renders the live message-bubble preview and tag input field.
    pub fn show(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        self.paint_preview_widget(ui, colors);
        ui.add_space(12.0);

        let before = self.rank_input.clone();
        InputField::single_line(
            "edit_peer_tag_rank_field",
            &mut self.rank_input,
            self.role.placeholder(),
            InputFieldStyle::default_input(colors),
        )
        .max_length(MAX_PEER_TAG_RANK_LEN)
        .show(ui);

        // Enforce live emoji removal while typing (preserving interior space while editing).
        if self.rank_input != before {
            let mut cleaned = String::with_capacity(self.rank_input.len());
            let mut pos = 0usize;
            while pos < self.rank_input.len() {
                if let Some((_, len)) = emoji::find_emoji(&self.rank_input, pos) {
                    pos += len;
                    continue;
                }
                if let Some(ch) = self.rank_input[pos..].chars().next() {
                    if !is_emoji_fallback_scalar(ch) && ch != '\n' && ch != '\r' {
                        cleaned.push(ch);
                    }
                    pos += ch.len_utf8();
                } else {
                    break;
                }
            }
            self.rank_input = cleaned.chars().take(MAX_PEER_TAG_RANK_LEN).collect();
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let count = self.rank_input.chars().count();
            ui.label(
                egui::RichText::new(format!("{count} / {MAX_PEER_TAG_RANK_LEN}"))
                    .font(FontId::new(11.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
            if !self.rank_input.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove Tag").clicked() {
                        self.rank_input.clear();
                    }
                });
            }
        });
    }

    fn paint_preview_widget(&self, ui: &mut Ui, colors: &dyn Colors) {
        let w = ui.available_width().max(260.0);
        let h = 114.0;
        let (outer, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());

        // Chat wallpaper-like tinted background card.
        ui.painter()
            .rect_filled(outer, CornerRadius::same(10), colors.color("windowBgOver"));

        // User avatar on the bottom-left of the preview bubble.
        let av_size = 34.0;
        let av_rect = Rect::from_min_size(
            Pos2::new(outer.left() + 12.0, outer.bottom() - av_size - 12.0),
            Vec2::splat(av_size),
        );
        EmptyUserpic::for_color_index(self.color_index, &self.user_name, colors)
            .paint_circle(ui.painter(), av_rect);

        // Incoming message bubble preview.
        let bubble_left = av_rect.right() + 8.0;
        let bubble_w = (outer.width() - (bubble_left - outer.left()) - 14.0).max(180.0);
        let bubble = Rect::from_min_size(
            Pos2::new(bubble_left, outer.top() + 12.0),
            Vec2::new(bubble_w, h - 24.0),
        );
        ui.painter()
            .rect_filled(bubble, CornerRadius::same(12), colors.color("msgInBg"));
        ui.painter().rect_stroke(
            bubble,
            CornerRadius::same(12),
            Stroke::new(0.8, colors.color("shadowFg")),
            egui::StrokeKind::Inside,
        );

        let pad_x = 12.0;
        let header_y = bubble.top() + 9.0;

        // Author display name in deterministic peer color.
        let name_fg = peer_name_color(self.color_index, colors);
        ui.painter().text(
            Pos2::new(bubble.left() + pad_x, header_y),
            Align2::LEFT_TOP,
            &self.user_name,
            FontId::new(13.0, family_semibold()),
            name_fg,
        );

        // Live right-side badge pill (`applyBadge` in `edit_tag_control.cpp`).
        let badge = self.effective_badge_text();
        if !badge.is_empty() {
            let (badge_bg, badge_fg) = match self.role {
                BadgeRole::Creator => (
                    Color32::from_rgba_unmultiplied(149, 92, 219, 44),
                    Color32::from_rgb(149, 92, 219),
                ),
                BadgeRole::Admin => (
                    Color32::from_rgba_unmultiplied(54, 138, 209, 44),
                    colors.color("windowActiveTextFg"),
                ),
                BadgeRole::User => (
                    colors.color("lightButtonBgOver"),
                    colors.color("windowSubTextFg"),
                ),
            };
            let badge_font = FontId::new(11.0, family_semibold());
            let galley = ui
                .painter()
                .layout_no_wrap(badge.clone(), badge_font.clone(), badge_fg);
            let pill_w = galley.size().x + 12.0;
            let pill = Rect::from_min_size(
                Pos2::new(bubble.right() - pad_x - pill_w, header_y - 1.0),
                Vec2::new(pill_w, 18.0),
            );
            ui.painter()
                .rect_filled(pill, CornerRadius::same(5), badge_bg);
            ui.painter().text(
                pill.center(),
                Align2::CENTER_CENTER,
                badge,
                badge_font,
                badge_fg,
            );
        }

        // 4 skeleton placeholder text lines (`TextLinesPart::draw` in `edit_tag_control.cpp`).
        let base_text_fg = colors.color("historyTextInFg");
        let line_color = Color32::from_rgba_unmultiplied(
            base_text_fg.r(),
            base_text_fg.g(),
            base_text_fg.b(),
            (f32::from(base_text_fg.a()) * PREVIEW_TEXT_LINES_ALPHA).round() as u8,
        );
        let line_avail = (bubble.width() - pad_x * 2.0).max(40.0);
        let line_h = 8.0;
        let line_spacing = 6.0;
        let mut y = bubble.top() + 32.0;
        for fraction in PREVIEW_LINE_FRACTIONS {
            let lw = line_avail * fraction;
            let lr =
                Rect::from_min_size(Pos2::new(bubble.left() + pad_x, y), Vec2::new(lw, line_h));
            ui.painter()
                .rect_filled(lr, CornerRadius::same(4), line_color);
            y += line_h + line_spacing;
        }
    }
}

/// Modal dialog state for editing a group member's custom rank/tag
/// (`EditCustomRankBox` in `edit_participants_box.cpp` + `messages.editChatParticipantRank`).
#[derive(Debug, Clone)]
pub struct EditPeerTagBoxState {
    /// Group or supergroup peer ID.
    pub peer_id: PeerId,
    /// Group display title.
    pub peer_title: String,
    /// Whether the user is editing their own tag (`isSelf`).
    pub is_self: bool,
    /// Embedded `EditTagControl` preview + input.
    pub control: EditTagControl,
    /// Inline RPC error message, if saving failed.
    pub error: Option<String>,
}

impl EditPeerTagBoxState {
    /// Constructs an [`EditPeerTagBoxState`] from a group [`PeerProfile`] and [`MemberEntry`].
    #[must_use]
    pub fn from_member(
        peer: &PeerProfile,
        member: &MemberEntry,
        current_rank: Option<&str>,
        is_self: bool,
    ) -> Self {
        let initial = current_rank
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| member.role.custom_title());
        let role = BadgeRole::from_member_role(&member.role);
        Self {
            peer_id: peer.id,
            peer_title: peer.title.clone(),
            is_self,
            control: EditTagControl::new(
                member.peer_id,
                member.name.clone(),
                member.color_index,
                role,
                initial,
            ),
            error: None,
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor for [`crate::boxes::BoxHost`].
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = if self.is_self {
            "Edit My Tag"
        } else {
            "Edit Member Tag"
        };
        BoxModal::new(id, title)
            .width(380.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
    }

    /// Renders the modal body inside `BoxHost`.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        self.control.show(ui, colors);
        ui.add_space(8.0);
        let about = if self.is_self {
            format!(
                "Choose a custom tag displayed next to your name in {}.",
                self.peer_title
            )
        } else {
            format!(
                "Choose a custom tag displayed next to {}'s name in {}.",
                self.control.user_name, self.peer_title
            )
        };
        ui.label(
            egui::RichText::new(about)
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowSubTextFg")),
        );
        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }

    /// Persists the sanitized rank via [`InfoBackend::edit_member_rank`]
    /// (`messages.editChatParticipantRank`).
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<String, String> {
        let rank = self.control.current_rank();
        match backend.edit_member_rank(self.peer_id, self.control.user_id, &rank) {
            Ok(saved) => {
                self.error = None;
                Ok(saved)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }
}
