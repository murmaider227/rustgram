//! Profile QR Card modal box using the `qrcode` crate.
//!
//! Port origin: `Telegram/SourceFiles/ui/boxes/peer_qr_box.h`,
//! `Telegram/SourceFiles/ui/boxes/peer_qr_box.cpp`,
//! `Telegram/SourceFiles/ui/effects/peer_qr_colors_panel.cpp`,
//! and `Telegram/SourceFiles/info/profile/info_profile_qr_link.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use qrcode::{Color as QrColor, EcLevel, QrCode};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;

use crate::info::backend::{InfoBackend, InfoPeerKind, PeerProfile};
use tg_app_core::core_types::dto::contacts_extras::{
    ExportedContactTokenInfo, ExportedNoPaidMessagesTokenInfo,
};
use tg_app_core::stand_in::types::PeerId;

/// Error correction level matching tdesktop's `Qr::Redundancy::Quartile`.
const PEER_QR_EC_LEVEL: EcLevel = EcLevel::Q;

/// Visual color theme swatch for the Profile QR card (`PeerQrColorsPanel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerQrSwatch {
    /// Short display name for accessibility & tooltips.
    pub name: &'static str,
    /// Card gradient top RGB.
    pub bg_top: [u8; 3],
    /// Card gradient bottom RGB.
    pub bg_bottom: [u8; 3],
    /// QR dark module RGB tint.
    pub qr_fg: [u8; 3],
    /// Card accent / badge RGB.
    pub accent: [u8; 3],
}

/// Built-in color swatches matching tdesktop's QR card presets.
pub const PEER_QR_SWATCHES: [PeerQrSwatch; 6] = [
    PeerQrSwatch {
        name: "Ocean",
        bg_top: [0x2A, 0x8F, 0xE5],
        bg_bottom: [0x1C, 0x5D, 0xB8],
        qr_fg: [0x1B, 0x67, 0xB8],
        accent: [0x40, 0xA7, 0xE3],
    },
    PeerQrSwatch {
        name: "Emerald",
        bg_top: [0x2E, 0xB8, 0x72],
        bg_bottom: [0x1A, 0x7A, 0x52],
        qr_fg: [0x1C, 0x7C, 0x4E],
        accent: [0x38, 0xC1, 0x72],
    },
    PeerQrSwatch {
        name: "Sunset",
        bg_top: [0xF0, 0x73, 0x4B],
        bg_bottom: [0xC4, 0x3D, 0x5E],
        qr_fg: [0xBD, 0x3D, 0x4D],
        accent: [0xF2, 0x7A, 0x54],
    },
    PeerQrSwatch {
        name: "Violet",
        bg_top: [0x82, 0x5B, 0xE6],
        bg_bottom: [0x51, 0x36, 0xA8],
        qr_fg: [0x5B, 0x3C, 0xB0],
        accent: [0x8E, 0x6B, 0xF0],
    },
    PeerQrSwatch {
        name: "Amber",
        bg_top: [0xE8, 0xA2, 0x2A],
        bg_bottom: [0xBD, 0x63, 0x1D],
        qr_fg: [0xA8, 0x5B, 0x14],
        accent: [0xF0, 0xAC, 0x35],
    },
    PeerQrSwatch {
        name: "Midnight",
        bg_top: [0x26, 0x34, 0x48],
        bg_bottom: [0x14, 0x1D, 0x2B],
        qr_fg: [0x1E, 0x2B, 0x3D],
        accent: [0x52, 0x88, 0xC1],
    },
];

/// Encoded boolean QR module matrix generated via `qrcode::QrCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerQrMatrix {
    /// Width and height of the QR symbol in modules.
    pub size: usize,
    /// Row-major module occupancy (`true` = dark module).
    pub modules: Vec<bool>,
}

impl PeerQrMatrix {
    /// Encodes `url` with Quartile error correction.
    #[must_use]
    pub fn encode(url: &str) -> Option<Self> {
        if url.is_empty() {
            return None;
        }
        let code = QrCode::with_error_correction_level(url.as_bytes(), PEER_QR_EC_LEVEL).ok()?;
        let size = code.width();
        let mut modules = Vec::with_capacity(size * size);
        for row in 0..size {
            for col in 0..size {
                modules.push(code[(col, row)] == QrColor::Dark);
            }
        }
        Some(Self { size, modules })
    }

    /// Returns `true` if `(row, col)` is a dark QR module.
    #[must_use]
    pub fn is_dark(&self, row: usize, col: usize) -> bool {
        if row >= self.size || col >= self.size {
            return false;
        }
        self.modules[row * self.size + col]
    }
}

/// Active state of the paid-message fee-waiver QR code (`FreeMode` in `peer_qr_box.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeMessagesCodeMode {
    /// Normal profile link (paid-message fee applies).
    #[default]
    Fee,
    /// Temporary fee-waiver QR link with an expiry countdown (`temporary = true`).
    Temporary,
    /// Permanent fee-waiver QR link with no time limit (`temporary = false`).
    Permanent,
}

/// Interactive actions emitted from [`PeerQrBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerQrBodyAction {
    /// Copy the peer link to clipboard / notify user.
    CopyLink(String),
    /// Open Share box prefilled with the peer link.
    ShareLink(String),
    /// Open the Shareable Theme Backdrop picker (`info_profile_share_theme.cpp`) for this peer.
    OpenShareTheme {
        /// Target peer identifier.
        peer_id: PeerId,
        /// Peer display title for the shareable theme card preview.
        peer_title: String,
    },
    /// Export the rendered QR card as PNG bytes `(filename, png_bytes)`.
    SavePng {
        /// Suggested filename (e.g. `"mira_vance_qr.png"`).
        filename: String,
        /// Encoded PNG image bytes.
        bytes: Vec<u8>,
    },
    /// Refresh or export a temporary contact QR token (`contacts.exportContactToken`).
    RefreshContactToken,
    /// Export or reset a paid-message fee-waiver QR token (`contacts.exportNoPaidMessagesToken`,
    /// `info_profile_free_codes.cpp:37-57`).
    ExportNoPaidMessagesToken {
        /// `true` to revoke the previous fee-waiver token and mint a replacement.
        revoke: bool,
        /// `true` for a temporary expiring code, `false` for a permanent code.
        temporary: bool,
    },
    /// Restore the plain profile QR link so the paid-message fee applies again
    /// (`tr::lng_no_paid_token_menu_restore`).
    RestoreMessageFee,
}

/// Stateful Profile QR Card modal box (`Ui::PeerQrBox`).
#[derive(Debug, Clone)]
pub struct PeerQrBoxState {
    /// Target peer identifier.
    pub peer_id: PeerId,
    /// Display title of the peer.
    pub peer_title: String,
    /// Display handle (`"@username"`, `"Invite Link"`, or temporary token countdown).
    pub display_handle: String,
    /// Encoded QR target URL (`https://t.me/...`, `https://t.me/contact/...`, or `tg://user?id=...`).
    pub link: String,
    /// Original non-waiver link preserved so "Restore message fee" returns to it cleanly.
    pub plain_link: String,
    /// Original non-waiver handle label preserved alongside [`Self::plain_link`].
    pub plain_display_handle: String,
    /// Avatar color index for fallback initials circle.
    pub avatar_color_index: u8,
    /// Selected swatch index inside [`PEER_QR_SWATCHES`].
    pub swatch_index: usize,
    /// Dark card interior toggle (light card by default, dark mode flips QR box fill).
    pub dark_card: bool,
    /// Cached encoded QR matrix for [`Self::link`].
    pub matrix: Option<PeerQrMatrix>,
    /// Optional status feedback shown inside the box (e.g. "Link copied").
    pub status_message: Option<String>,
    /// `true` when this QR code was generated from `contacts.exportContactToken`.
    pub is_contact_token: bool,
    /// Unix timestamp when the temporary contact QR token expires (`info_profile_qr_link.cpp`).
    pub token_expires_unix: Option<i32>,
    /// `true` when the peer supports exporting a temporary contact token (users without username).
    pub can_export_contact_token: bool,
    /// `true` when the peer supports exporting a paid-message fee-waiver QR code
    /// (`contacts.exportNoPaidMessagesToken`).
    pub can_export_free_message_token: bool,
    /// Active fee-waiver QR code mode (`Fee`, `Temporary`, or `Permanent`).
    pub free_code_mode: FreeMessagesCodeMode,
    /// Optional Unix expiration timestamp of the active temporary fee-waiver token.
    pub free_token_expires_unix: Option<i32>,
}

impl PeerQrBoxState {
    /// Constructs a QR card for `peer`, resolving the best shareable link.
    #[must_use]
    pub fn from_profile(peer: &PeerProfile) -> Self {
        let can_export_contact_token =
            matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::SavedMessages);
        let (link, display_handle) = if let Some(u) = peer.primary_username() {
            (format!("https://t.me/{u}"), format!("@{u}"))
        } else if let Some(ref inv) = peer.invite_link {
            (inv.clone(), "Invite Link".to_owned())
        } else {
            (
                format!("tg://user?id={}", peer.id.bare()),
                peer.title.clone(),
            )
        };
        let swatch_index = usize::from(peer.color_index) % PEER_QR_SWATCHES.len();
        let matrix = PeerQrMatrix::encode(&link);
        Self {
            peer_id: peer.id,
            peer_title: peer.title.clone(),
            plain_display_handle: display_handle.clone(),
            display_handle,
            plain_link: link.clone(),
            link,
            avatar_color_index: peer.color_index,
            swatch_index,
            dark_card: false,
            matrix,
            status_message: None,
            is_contact_token: false,
            token_expires_unix: None,
            can_export_contact_token,
            can_export_free_message_token: can_export_contact_token,
            free_code_mode: FreeMessagesCodeMode::Fee,
            free_token_expires_unix: None,
        }
    }

    /// Constructs a QR card for `peer`, automatically calling `contacts.exportContactToken`
    /// when a user has no public username or invite link (`info_profile_qr_link.cpp:139-232`).
    #[must_use]
    pub fn from_profile_with_backend(peer: &PeerProfile, backend: &dyn InfoBackend) -> Self {
        let mut state = Self::from_profile(peer);
        if state.can_export_contact_token
            && peer.primary_username().is_none()
            && peer.invite_link.is_none()
        {
            match backend.export_contact_token() {
                Ok(info) => state.apply_contact_token(&info),
                Err(err) => {
                    state.status_message = tg_app_core::user_error::share_theme_or_qr(
                        "contacts.exportContactToken",
                        &err,
                    )
                    .into_text();
                }
            }
        }
        state
    }

    /// Applies a temporary contact QR token returned by `contacts.exportContactToken`.
    pub fn apply_contact_token(&mut self, info: &ExportedContactTokenInfo) {
        self.link = info.url.clone();
        self.plain_link = info.url.clone();
        self.matrix = PeerQrMatrix::encode(&self.link);
        self.is_contact_token = true;
        self.token_expires_unix = Some(info.expires);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(info.expires - 900, |d| d.as_secs() as i32);
        let remaining = (info.expires - now).max(0);
        self.display_handle = format!("Contact QR · {}:{:02}", remaining / 60, remaining % 60);
        self.plain_display_handle = self.display_handle.clone();
        self.free_code_mode = FreeMessagesCodeMode::Fee;
        self.free_token_expires_unix = None;
        self.status_message = Some("Temporary contact QR link exported.".to_owned());
    }

    /// Applies a paid-message fee-waiver token returned by `contacts.exportNoPaidMessagesToken`
    /// (`SetupFreeCodes::applyCode` in `peer_qr_box.cpp:720-762`) and returns the toast message.
    pub fn apply_no_paid_messages_token(
        &mut self,
        info: &ExportedNoPaidMessagesTokenInfo,
        revoke: bool,
        temporary: bool,
    ) -> String {
        if info.url.is_empty() {
            if revoke {
                self.restore_plain_link();
            }
            let unavailable = tr::lng_peer_qr_link_unavailable();
            self.status_message = Some(unavailable.clone());
            return unavailable;
        }
        self.link = info.url.clone();
        self.matrix = PeerQrMatrix::encode(&self.link);
        if temporary {
            self.free_code_mode = FreeMessagesCodeMode::Temporary;
            self.free_token_expires_unix = info.expires;
            let countdown = format_free_code_countdown(info.expires);
            self.display_handle = tr::lng_no_paid_token_pill_temporary(&countdown);
            let toast = if revoke {
                tr::lng_no_paid_token_toast_reset()
            } else {
                tr::lng_no_paid_token_toast_expires(&countdown)
            };
            self.status_message = Some(toast.clone());
            toast
        } else {
            self.free_code_mode = FreeMessagesCodeMode::Permanent;
            self.free_token_expires_unix = None;
            self.display_handle = tr::lng_no_paid_token_pill_permanent();
            let toast = if revoke {
                tr::lng_no_paid_token_toast_reset()
            } else {
                tr::lng_no_paid_token_toast_permanent()
            };
            self.status_message = Some(toast.clone());
            toast
        }
    }

    /// Restores the plain profile QR link so the paid-message fee applies again
    /// (`SetupFreeCodes::returnToFee` in `peer_qr_box.cpp:716-719`).
    pub fn restore_plain_link(&mut self) {
        self.link = self.plain_link.clone();
        self.display_handle = self.plain_display_handle.clone();
        self.matrix = PeerQrMatrix::encode(&self.link);
        self.free_code_mode = FreeMessagesCodeMode::Fee;
        self.free_token_expires_unix = None;
        self.status_message = Some(tr::lng_no_paid_token_toast_restored());
    }

    /// Constructs a QR card for an explicit link and title (useful for tests & invite links).
    #[must_use]
    pub fn for_link(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        display_handle: impl Into<String>,
        link: impl Into<String>,
        avatar_color_index: u8,
    ) -> Self {
        let link = link.into();
        let display_handle = display_handle.into();
        let matrix = PeerQrMatrix::encode(&link);
        Self {
            peer_id,
            peer_title: peer_title.into(),
            plain_display_handle: display_handle.clone(),
            display_handle,
            plain_link: link.clone(),
            link,
            avatar_color_index,
            swatch_index: usize::from(avatar_color_index) % PEER_QR_SWATCHES.len(),
            dark_card: false,
            matrix,
            status_message: None,
            is_contact_token: false,
            token_expires_unix: None,
            can_export_contact_token: false,
            can_export_free_message_token: false,
            free_code_mode: FreeMessagesCodeMode::Fee,
            free_token_expires_unix: None,
        }
    }

    /// Returns the currently selected [`PeerQrSwatch`].
    #[must_use]
    pub fn active_swatch(&self) -> PeerQrSwatch {
        PEER_QR_SWATCHES[self.swatch_index % PEER_QR_SWATCHES.len()]
    }

    /// Selects a theme swatch by index.
    pub fn set_swatch(&mut self, idx: usize) {
        self.swatch_index = idx % PEER_QR_SWATCHES.len();
    }

    /// Encodes the current QR card into a crisp PNG image (`scale_px` pixels per module).
    pub fn encode_qr_png(&self, scale_px: u32) -> Result<Vec<u8>, String> {
        let Some(ref matrix) = self.matrix else {
            return Err("QR matrix is unavailable.".to_owned());
        };
        let scale = scale_px.clamp(2, 24);
        let quiet_zone = 3u32;
        let dim = (matrix.size as u32 + quiet_zone * 2) * scale;
        let swatch = self.active_swatch();
        let (bg, fg) = if self.dark_card {
            ([0x18, 0x20, 0x2B, 0xFF], [0xFF, 0xFF, 0xFF, 0xFF])
        } else {
            (
                [0xFF, 0xFF, 0xFF, 0xFF],
                [swatch.qr_fg[0], swatch.qr_fg[1], swatch.qr_fg[2], 0xFF],
            )
        };

        let mut img = image::RgbaImage::from_pixel(dim, dim, image::Rgba(bg));
        for row in 0..matrix.size {
            for col in 0..matrix.size {
                if !matrix.is_dark(row, col) {
                    continue;
                }
                let x0 = (col as u32 + quiet_zone) * scale;
                let y0 = (row as u32 + quiet_zone) * scale;
                for dy in 0..scale {
                    for dx in 0..scale {
                        img.put_pixel(x0 + dx, y0 + dy, image::Rgba(fg));
                    }
                }
            }
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| format!("PNG encode failed: {e}"))?;
        Ok(out.into_inner())
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, "QR Code")
            .width(st::box_wide_width.0 as f32)
            .with_left_button(BoxButton::default_btn("share_qr", "Share"))
            .add_button(BoxButton::default_btn("copy_qr_link", "Copy Link"))
            .add_button(BoxButton::default_btn("close", tr::lng_close()));
        modal.max_content_height = 510.0;
        modal
    }

    /// Renders the QR card, theme swatch strip, dark/light toggle, and Save PNG button.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<PeerQrBodyAction> {
        let mut action = None;
        let swatch = self.active_swatch();
        let width = (ui.available_width() - 14.0).max(240.0);

        // 1. Styled QR Card surface
        let card_h = 286.0;
        let (card_rect, _) = ui.allocate_exact_size(Vec2::new(width, card_h), Sense::hover());
        let bg_top = Color32::from_rgb(swatch.bg_top[0], swatch.bg_top[1], swatch.bg_top[2]);
        let bg_bot = Color32::from_rgb(
            swatch.bg_bottom[0],
            swatch.bg_bottom[1],
            swatch.bg_bottom[2],
        );
        // Two-band subtle gradient backdrop
        ui.painter()
            .rect_filled(card_rect, CornerRadius::same(14), bg_top);
        let lower_half = Rect::from_min_max(
            Pos2::new(card_rect.left(), card_rect.center().y),
            card_rect.max,
        );
        ui.painter().rect_filled(
            lower_half,
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: 14,
                se: 14,
            },
            bg_bot,
        );

        // Avatar circle overlapping top of inner QR tile
        let qr_tile_size = 186.0;
        let qr_tile_rect = Rect::from_center_size(
            Pos2::new(card_rect.center().x, card_rect.top() + 132.0),
            Vec2::new(qr_tile_size + 24.0, qr_tile_size + 42.0),
        );
        let tile_bg = if self.dark_card {
            Color32::from_rgb(0x18, 0x20, 0x2B)
        } else {
            Color32::WHITE
        };
        let module_fg = if self.dark_card {
            Color32::WHITE
        } else {
            Color32::from_rgb(swatch.qr_fg[0], swatch.qr_fg[1], swatch.qr_fg[2])
        };
        ui.painter()
            .rect_filled(qr_tile_rect, CornerRadius::same(14), tile_bg);

        let av_rect = Rect::from_center_size(
            Pos2::new(card_rect.center().x, qr_tile_rect.top() - 2.0),
            Vec2::splat(40.0),
        );
        ui.painter().circle_filled(av_rect.center(), 22.0, tile_bg);
        EmptyUserpic::for_color_index(self.avatar_color_index, &self.peer_title, colors)
            .paint_circle(ui.painter(), av_rect);

        // Paint QR matrix modules inside `qr_inner_rect`
        let qr_inner = Rect::from_center_size(
            Pos2::new(qr_tile_rect.center().x, qr_tile_rect.top() + 102.0),
            Vec2::splat(154.0),
        );
        if let Some(ref matrix) = self.matrix {
            paint_qr_matrix(ui.painter(), qr_inner, matrix, module_fg, tile_bg);
        } else {
            ui.painter().text(
                qr_inner.center(),
                Align2::CENTER_CENTER,
                "QR unavailable",
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
        }

        // Handle label below QR inside tile
        ui.painter().text(
            Pos2::new(qr_tile_rect.center().x, qr_tile_rect.bottom() - 16.0),
            Align2::CENTER_CENTER,
            &self.display_handle,
            FontId::new(13.0, family_semibold()),
            module_fg,
        );

        // Peer display title & link footer on gradient card
        ui.painter().text(
            Pos2::new(card_rect.center().x, card_rect.bottom() - 20.0),
            Align2::CENTER_CENTER,
            &self.peer_title,
            FontId::new(12.5, family_semibold()),
            Color32::WHITE,
        );

        ui.add_space(12.0);

        // 2. Color Theme Swatch Picker (`PeerQrColorsPanel`)
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Theme")
                    .font(FontId::new(12.5, family_semibold()))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(8.0);
            for (idx, sw) in PEER_QR_SWATCHES.iter().enumerate() {
                let (sw_rect, sw_resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                sw_resp.widget_info(|| {
                    WidgetInfo::selected(
                        Role::Button,
                        true,
                        self.swatch_index == idx,
                        format!("QR Theme {}", sw.name),
                    )
                });
                if sw_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let col = Color32::from_rgb(sw.bg_top[0], sw.bg_top[1], sw.bg_top[2]);
                ui.painter().circle_filled(sw_rect.center(), 10.0, col);
                if self.swatch_index == idx {
                    ui.painter().circle_stroke(
                        sw_rect.center(),
                        12.5,
                        Stroke::new(2.0, colors.color("windowActiveTextFg")),
                    );
                }
                if sw_resp.on_hover_text(sw.name).clicked() {
                    self.swatch_index = idx;
                }
            }
        });

        ui.add_space(8.0);

        // 3. Dark card toggle + Save PNG + Share Theme + Temporary Contact QR export
        ui.horizontal_wrapped(|ui| {
            let mode_lbl = if self.dark_card {
                "Light Card"
            } else {
                "Dark Card"
            };
            if ui.button(mode_lbl).clicked() {
                self.dark_card = !self.dark_card;
            }
            if ui.button("Save QR Image").clicked() {
                match self.encode_qr_png(6) {
                    Ok(bytes) => {
                        let slug: String = self
                            .display_handle
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        let filename = if slug.is_empty() {
                            format!("peer_{}_qr.png", self.peer_id.bare())
                        } else {
                            format!("{slug}_qr.png")
                        };
                        self.status_message = Some(format!(
                            "QR PNG prepared ({filename}, {} bytes)",
                            bytes.len()
                        ));
                        action = Some(PeerQrBodyAction::SavePng { filename, bytes });
                    }
                    Err(err) => {
                        self.status_message = Some(err);
                    }
                }
            }
            if ui.button("Share Theme").clicked() {
                action = Some(PeerQrBodyAction::OpenShareTheme {
                    peer_id: self.peer_id,
                    peer_title: self.peer_title.clone(),
                });
            }
            if self.can_export_contact_token {
                let token_btn = if self.is_contact_token {
                    "Refresh Contact QR"
                } else {
                    "Temporary Contact QR"
                };
                if ui.button(token_btn).clicked() {
                    action = Some(PeerQrBodyAction::RefreshContactToken);
                }
            }
        });

        // 4. Paid-message fee-waiver QR link controls (`contacts.exportNoPaidMessagesToken`,
        //    `SetupFreeCodes` in `peer_qr_box.cpp:672-920`)
        if self.can_export_free_message_token {
            ui.add_space(8.0);
            match self.free_code_mode {
                FreeMessagesCodeMode::Fee => {
                    if qr_pill_button(
                        ui,
                        "Allow Free Messages",
                        colors.color("windowBgOver"),
                        colors.color("windowActiveTextFg"),
                    ) {
                        action = Some(PeerQrBodyAction::ExportNoPaidMessagesToken {
                            revoke: false,
                            temporary: true,
                        });
                    }
                }
                FreeMessagesCodeMode::Temporary | FreeMessagesCodeMode::Permanent => {
                    let is_temp = self.free_code_mode == FreeMessagesCodeMode::Temporary;
                    let badge_text = if is_temp {
                        let countdown = format_free_code_countdown(self.free_token_expires_unix);
                        tr::lng_no_paid_token_pill_temporary(&countdown)
                    } else {
                        tr::lng_no_paid_token_pill_permanent()
                    };
                    ui.horizontal(|ui| {
                        let font = FontId::new(12.0, family_semibold());
                        let galley =
                            ui.painter()
                                .layout_no_wrap(badge_text.clone(), font, Color32::WHITE);
                        let (pill_rect, _) = ui.allocate_exact_size(
                            Vec2::new(galley.size().x + 18.0, 26.0),
                            Sense::hover(),
                        );
                        ui.painter().rect_filled(
                            pill_rect,
                            CornerRadius::same(13),
                            colors.color("activeButtonBg"),
                        );
                        ui.painter().galley(
                            Pos2::new(
                                pill_rect.center().x - galley.size().x * 0.5,
                                pill_rect.center().y - galley.size().y * 0.5,
                            ),
                            galley,
                            Color32::WHITE,
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(tr::lng_no_paid_token_menu_reset()).clicked() {
                            action = Some(PeerQrBodyAction::ExportNoPaidMessagesToken {
                                revoke: true,
                                temporary: is_temp,
                            });
                        }
                        let timer_lbl = if is_temp {
                            tr::lng_no_paid_token_menu_disable_timer()
                        } else {
                            tr::lng_no_paid_token_menu_enable_timer()
                        };
                        if ui.button(timer_lbl).clicked() {
                            action = Some(PeerQrBodyAction::ExportNoPaidMessagesToken {
                                revoke: false,
                                temporary: !is_temp,
                            });
                        }
                        if ui.button(tr::lng_no_paid_token_menu_restore()).clicked() {
                            action = Some(PeerQrBodyAction::RestoreMessageFee);
                        }
                    });
                }
            }
        }

        ui.add_space(6.0);
        if self.is_contact_token {
            ui.label(
                egui::RichText::new("Temporary contact link expires automatically for privacy.")
                    .font(FontId::new(11.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        }
        ui.allocate_ui(Vec2::new(width, 0.0), |ui| {
            ui.label(
                egui::RichText::new(&self.link)
                    .font(FontId::new(11.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        });

        if let Some(ref status) = self.status_message {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(status)
                    .font(FontId::new(11.5, family_semibold()))
                    .color(colors.color("windowActiveTextFg")),
            );
        }

        action
    }
}

/// Paints a [`PeerQrMatrix`] into `rect` with rounded module corners matching tdesktop's QR style.
pub fn paint_qr_matrix(
    painter: &egui::Painter,
    rect: Rect,
    matrix: &PeerQrMatrix,
    fg: Color32,
    bg: Color32,
) {
    if matrix.size == 0 {
        return;
    }
    painter.rect_filled(rect, CornerRadius::same(6), bg);
    let cell = rect.width().min(rect.height()) / matrix.size as f32;
    let origin = Pos2::new(
        rect.center().x - cell * matrix.size as f32 * 0.5,
        rect.center().y - cell * matrix.size as f32 * 0.5,
    );
    let rounding = (cell * 0.25).clamp(1.0, 3.0).round() as u8;
    for row in 0..matrix.size {
        for col in 0..matrix.size {
            if !matrix.is_dark(row, col) {
                continue;
            }
            let r = Rect::from_min_size(
                Pos2::new(origin.x + col as f32 * cell, origin.y + row as f32 * cell),
                Vec2::splat(cell + 0.3),
            );
            painter.rect_filled(r, CornerRadius::same(rounding), fg);
        }
    }
}

fn format_free_code_countdown(expires_unix: Option<i32>) -> String {
    let Some(exp) = expires_unix else {
        return "60:00".to_owned();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(exp.saturating_sub(3_600), |d| d.as_secs() as i32);
    let remaining = (exp - now).max(60);
    if remaining >= 3_600 {
        format!("{}m", (remaining + 59) / 60)
    } else {
        format!("{}:{:02}", remaining / 60, remaining % 60)
    }
}

fn qr_pill_button(ui: &mut Ui, label: &str, bg: Color32, fg: Color32) -> bool {
    let font = FontId::new(12.0, family_semibold());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = Vec2::new(galley.size().x + 20.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
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
