//! Wallet Card Editor (`wallet.setCardStickers`) & Move-to-Gifts sheet
//! (`payments.depositStarGift` / `payments.displayStarGift`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/wallet/wallet_card_editor.{h,cpp}`
//! - `Telegram/SourceFiles/wallet/wallet_card_stickers.{h,cpp}`
//! - `Telegram/SourceFiles/wallet/wallet_own_card.{h,cpp}`
//! - `Telegram/SourceFiles/wallet/wallet_move_to_gifts.{h,cpp}`
//!
//! Safety: Move-to-Gifts ALWAYS stops at an explicit user confirmation step showing
//! the verified escrow address, attached TON deposit amount, and estimated fee before
//! signing or submitting any transaction.

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, RichText, ScrollArea, Sense,
    Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_wallet::mtproto_api::{CardStickerCoordinates, CardStickerInput, WALLET_CARD_MAX_STICKERS};
use tg_wallet::toncenter::{NftItem, NftKind};
use tg_wallet::{NANO_PER_TON, format_ton};

use tg_app_core::core_types::misc::nft_display_title;
use tg_app_core::stand_in::theme::Palette;

use super::backend::{
    MoveToGiftsQuote, MoveToGiftsReceipt, WalletBackend, WalletCardState, WalletCardStickerEntry,
    WalletOverview,
};

/// Snap rotation angle in degrees matching `kSnapAngle = 45.` (`wallet_card_editor.cpp:32`).
pub const CARD_STICKER_SNAP_ANGLE: f64 = 45.0;

/// Selection highlight easing duration in seconds (`review-animations`: sub-300ms UI feedback).
const SELECTION_ANIM_SECS: f32 = 0.16;

// ============================================================================
// 1. Wallet Card Editor (`wallet_card_editor.cpp`, `wallet_own_card.cpp`)
// ============================================================================

/// Transient state for the Wallet Card Editor screen.
#[derive(Debug, Clone, Default)]
pub struct CardEditorState {
    /// Draft card state being edited before clicking Save.
    pub draft: WalletCardState,
    /// Whether `draft` has been initialized from the backend.
    pub initialized: bool,
    /// Currently selected sticker NFT address on the card canvas.
    pub selected_sticker: Option<String>,
    /// Whether `draft` has unsaved modifications.
    pub dirty: bool,
    /// Inline validation or backend error message.
    pub error: Option<String>,
}

impl CardEditorState {
    /// Loads the current card state from `backend` into the editable draft.
    pub fn load_from_backend(&mut self, backend: &dyn WalletBackend) {
        self.draft = backend.wallet_card_state();
        self.initialized = true;
        self.dirty = false;
        self.error = None;
        if let Some(ref sel) = self.selected_sticker
            && !self
                .draft
                .stickers
                .iter()
                .any(|s| s.nft_address.eq_ignore_ascii_case(sel))
        {
            self.selected_sticker = None;
        }
    }

    /// Resets editor state when leaving the screen.
    pub fn reset(&mut self) {
        self.initialized = false;
        self.dirty = false;
        self.error = None;
    }

    /// Adds a collectible gift sticker onto the wallet card at a clean non-overlapping slot.
    pub fn add_sticker(&mut self, item: &NftItem) -> Result<(), String> {
        if self
            .draft
            .stickers
            .iter()
            .any(|s| s.nft_address.eq_ignore_ascii_case(&item.address))
        {
            self.selected_sticker = Some(item.address.clone());
            return Ok(());
        }
        if self.draft.stickers.len() >= WALLET_CARD_MAX_STICKERS {
            let msg = tr::lng_wallet_card_limit(WALLET_CARD_MAX_STICKERS as i64);
            self.error = Some(msg.clone());
            return Err(msg);
        }
        let slot = self.draft.stickers.len();
        let col = (slot % 4) as f64;
        let row = (slot / 4) as f64;
        let coords = CardStickerCoordinates {
            x: (18.0 + col * 21.0).clamp(10.0, 90.0),
            y: (26.0 + row * 28.0).clamp(12.0, 88.0),
            w: 18.0,
            h: 28.0,
            rotation: 0.0,
            radius: Some(4.0),
        };
        self.draft.stickers.push(WalletCardStickerEntry {
            nft_address: item.address.clone(),
            title: nft_display_title(item),
            collection_name: item.collection_name.clone(),
            coordinates: coords,
        });
        self.selected_sticker = Some(item.address.clone());
        self.dirty = true;
        self.error = None;
        Ok(())
    }

    /// Removes a sticker from the card, clearing background if it was the background gift
    /// (`RemoveOwnCardSticker` in `wallet_own_card.cpp:256-266`).
    pub fn remove_sticker(&mut self, nft_address: &str) -> bool {
        let before = self.draft.stickers.len();
        self.draft
            .stickers
            .retain(|s| !s.nft_address.eq_ignore_ascii_case(nft_address));
        let removed = self.draft.stickers.len() < before;
        if removed {
            if self
                .draft
                .background_address
                .as_deref()
                .is_some_and(|bg| bg.eq_ignore_ascii_case(nft_address))
            {
                self.draft.background_address = None;
            }
            if self
                .selected_sticker
                .as_deref()
                .is_some_and(|sel| sel.eq_ignore_ascii_case(nft_address))
            {
                self.selected_sticker = self.draft.stickers.first().map(|s| s.nft_address.clone());
            }
            self.dirty = true;
            self.error = None;
        }
        removed
    }

    /// Applies or removes a placed sticker as the card background color.
    pub fn toggle_background(&mut self, nft_address: &str) {
        let already_bg = self
            .draft
            .background_address
            .as_deref()
            .is_some_and(|bg| bg.eq_ignore_ascii_case(nft_address));
        if already_bg {
            self.draft.background_address = None;
        } else if self
            .draft
            .stickers
            .iter()
            .any(|s| s.nft_address.eq_ignore_ascii_case(nft_address))
        {
            self.draft.background_address = Some(nft_address.to_owned());
        }
        self.dirty = true;
        self.error = None;
    }

    /// Converts the current draft stickers into [`CardStickerInput`] wire entries.
    #[must_use]
    pub fn to_wire_inputs(&self) -> Vec<CardStickerInput> {
        self.draft
            .stickers
            .iter()
            .map(|s| CardStickerInput {
                coordinates: s.coordinates,
                nft_address: s.nft_address.clone(),
            })
            .collect()
    }
}

/// Outcome returned by [`render_card_editor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardEditorOutcome {
    /// Stay on the Card Editor screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Card stickers saved via `wallet.setCardStickers`; emit toast.
    Saved(String),
}

/// Derives a deterministic, high-contrast card backdrop tint from an optional background gift address.
#[must_use]
pub fn card_backdrop_color(bg_nft_address: Option<&str>, palette: &Palette) -> Color32 {
    let Some(addr) = bg_nft_address.filter(|s| !s.trim().is_empty()) else {
        return palette.window_bg_active;
    };
    let hash = addr.bytes().fold(0u32, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    // Curated deep-saturated tdesktop-harmonious card backdrop colors (all >= 4.5:1 with white text).
    const BACKDROPS: [Color32; 5] = [
        Color32::from_rgb(0x28, 0x69, 0xB8),
        Color32::from_rgb(0x5E, 0x3D, 0x9E),
        Color32::from_rgb(0x1E, 0x7A, 0x63),
        Color32::from_rgb(0x9E, 0x45, 0x2B),
        Color32::from_rgb(0x2F, 0x52, 0x8F),
    ];
    BACKDROPS[(hash as usize) % BACKDROPS.len()]
}

/// Renders the Wallet Card Editor screen (`wallet_card_editor.cpp`).
pub fn render_card_editor(
    ui: &mut Ui,
    state: &mut CardEditorState,
    backend: &mut dyn WalletBackend,
    overview: &WalletOverview,
    testnet: bool,
    palette: &Palette,
) -> CardEditorOutcome {
    if !state.initialized {
        state.load_from_backend(backend);
    }
    let mut outcome = CardEditorOutcome::None;

    // Header navigation bar.
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            state.reset();
            outcome = CardEditorOutcome::Back;
        }
        ui.label(
            RichText::new("Customize Wallet Card")
                .size(15.5)
                .strong()
                .color(palette.window_bold_fg),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let save_enabled = state.draft.ready || state.dirty;
            if ui
                .add_enabled(
                    save_enabled,
                    egui::Button::new(
                        RichText::new(tr::lng_settings_save())
                            .strong()
                            .color(palette.window_fg_active),
                    )
                    .fill(palette.window_bg_active)
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(6)),
                )
                .clicked()
            {
                let inputs = state.to_wire_inputs();
                let bg = state.draft.background_address.as_deref();
                match backend.set_card_stickers(&inputs, bg) {
                    Ok(saved) => {
                        state.draft = saved;
                        state.dirty = false;
                        state.error = None;
                        outcome = CardEditorOutcome::Saved(format!(
                            "Wallet card saved ({} sticker{})",
                            state.draft.stickers.len(),
                            if state.draft.stickers.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ));
                    }
                    Err(err) => {
                        state.error = Some(format!("{} ({err})", tr::lng_wallet_card_save_error()));
                    }
                }
            }
        });
    });
    ui.add_space(6.0);

    if let Some(ref err) = state.error {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.attention_fg))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(
                    egui::Label::new(RichText::new(err).size(12.0).color(palette.attention_fg))
                        .wrap(),
                );
            });
        ui.add_space(6.0);
    }

    ScrollArea::vertical()
        .id_salt("tg_wallet_card_editor_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // 1. Live Interactive Wallet Card Preview (`walletCardHeight = 190px`, `walletCardRadius = 16px`).
            render_interactive_card_canvas(ui, state, overview, testnet, palette);

            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{} ({}/{})",
                        tr::lng_wallet_card_drag_hint(),
                        state.draft.stickers.len(),
                        WALLET_CARD_MAX_STICKERS
                    ))
                    .size(12.0)
                    .color(palette.window_sub_text_fg),
                )
                .wrap(),
            );
            ui.add_space(10.0);

            // 2. Selected sticker transform & background color controls.
            if let Some(sel_addr) = state.selected_sticker.clone()
                && let Some(idx) = state
                    .draft
                    .stickers
                    .iter()
                    .position(|s| s.nft_address.eq_ignore_ascii_case(&sel_addr))
            {
                let is_bg = state
                    .draft
                    .background_address
                    .as_deref()
                    .is_some_and(|bg| bg.eq_ignore_ascii_case(&sel_addr));
                let title = state.draft.stickers[idx].title.clone();
                let mut remove_clicked = false;
                let mut toggle_bg_clicked = false;

                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("Selected: {title}"))
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            let bg_label = if is_bg {
                                tr::lng_wallet_card_remove_color()
                            } else {
                                tr::lng_wallet_card_apply_color()
                            };
                            if ui.small_button(bg_label).clicked() {
                                toggle_bg_clicked = true;
                            }
                            if ui
                                .small_button(
                                    RichText::new(tr::lng_wallet_card_remove())
                                        .color(palette.attention_fg),
                                )
                                .clicked()
                            {
                                remove_clicked = true;
                            }
                        });
                        ui.add_space(6.0);

                        let coords = &mut state.draft.stickers[idx].coordinates;
                        let mut changed = false;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("X %:").size(12.0));
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut coords.x, 8.0..=92.0)
                                        .max_decimals(0)
                                        .show_value(true),
                                )
                                .changed();
                            ui.label(RichText::new("Y %:").size(12.0));
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut coords.y, 12.0..=88.0)
                                        .max_decimals(0)
                                        .show_value(true),
                                )
                                .changed();
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Size:").size(12.0));
                            let mut size_pct = coords.w;
                            if ui
                                .add(
                                    egui::Slider::new(&mut size_pct, 10.0..=36.0)
                                        .max_decimals(0)
                                        .show_value(true),
                                )
                                .changed()
                            {
                                coords.w = size_pct;
                                coords.h = (size_pct * 1.45).clamp(12.0, 56.0);
                                changed = true;
                            }
                            ui.label(RichText::new("Rotate:").size(12.0));
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut coords.rotation, 0.0..=359.0)
                                        .suffix("°")
                                        .max_decimals(0),
                                )
                                .changed();
                            if ui.small_button("Snap 45°").clicked() {
                                coords.rotation = (coords.rotation / CARD_STICKER_SNAP_ANGLE)
                                    .round()
                                    * CARD_STICKER_SNAP_ANGLE
                                    % 360.0;
                                changed = true;
                            }
                        });
                        if changed {
                            state.dirty = true;
                        }
                    });

                if toggle_bg_clicked {
                    state.toggle_background(&sel_addr);
                }
                if remove_clicked {
                    state.remove_sticker(&sel_addr);
                }
                ui.add_space(10.0);
            }

            // 3. Available Collectible Gifts Picker (`wallet_card_stickers.cpp`).
            ui.label(
                RichText::new("Available Collectible Gifts")
                    .size(14.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(6.0);

            let collectibles = backend.collectibles();
            let gifts: Vec<&NftItem> = collectibles
                .iter()
                .filter(|n| matches!(n.kind, NftKind::TelegramGift))
                .collect();

            if gifts.is_empty() {
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "No collectible gifts found in this wallet yet. Receive or import a Telegram collectible gift to decorate your card.",
                                )
                                .size(12.5)
                                .color(palette.window_sub_text_fg),
                            )
                            .wrap(),
                        );
                    });
            } else {
                for gift in gifts {
                    let placed = state
                        .draft
                        .stickers
                        .iter()
                        .any(|s| s.nft_address.eq_ignore_ascii_case(&gift.address));
                    let is_bg = state
                        .draft
                        .background_address
                        .as_deref()
                        .is_some_and(|bg| bg.eq_ignore_ascii_case(&gift.address));
                    Frame::NONE
                        .fill(if placed {
                            palette.dialogs_bg_active
                        } else {
                            palette.window_bg_over
                        })
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let row_title_fg = if placed {
                                palette.window_fg_active
                            } else {
                                palette.window_bold_fg
                            };
                            let row_sub_fg = if placed {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 220)
                            } else {
                                palette.window_sub_text_fg
                            };
                            let title_text = nft_display_title(gift);
                            let sub = if is_bg {
                                format!("{} · Card Background", gift.collection_name)
                            } else if placed {
                                format!("{} · On Card", gift.collection_name)
                            } else {
                                gift.collection_name.clone()
                            };
                            let narrow = ui.available_width() < 460.0;
                            let render_actions =
                                |ui: &mut Ui, state: &mut CardEditorState| {
                                    if placed {
                                        let bg_lbl = if is_bg {
                                            tr::lng_wallet_card_remove_color()
                                        } else {
                                            tr::lng_wallet_card_apply_color()
                                        };
                                        if ui.small_button(bg_lbl).clicked() {
                                            state.toggle_background(&gift.address);
                                        }
                                        if ui
                                            .small_button(
                                                RichText::new(tr::lng_wallet_card_remove())
                                                    .color(palette.attention_fg),
                                            )
                                            .clicked()
                                        {
                                            state.remove_sticker(&gift.address);
                                        }
                                    } else if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(tr::lng_wallet_card_add())
                                                    .size(12.0)
                                                    .color(palette.window_fg_active),
                                            )
                                            .fill(palette.window_bg_active)
                                            .stroke(Stroke::NONE)
                                            .corner_radius(CornerRadius::same(6)),
                                        )
                                        .clicked()
                                    {
                                        let _ = state.add_sticker(gift);
                                    }
                                };
                            if narrow {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(&title_text)
                                            .size(13.5)
                                            .strong()
                                            .color(row_title_fg),
                                    );
                                    ui.label(RichText::new(&sub).size(11.5).color(row_sub_fg));
                                    ui.add_space(6.0);
                                    ui.horizontal_wrapped(|ui| {
                                        render_actions(ui, state);
                                    });
                                });
                            } else {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(&title_text)
                                                .size(13.5)
                                                .strong()
                                                .color(row_title_fg),
                                        );
                                        ui.label(RichText::new(&sub).size(11.5).color(row_sub_fg));
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            render_actions(ui, state);
                                        },
                                    );
                                });
                            }
                        });
                    ui.add_space(6.0);
                }
            }
        });

    outcome
}

fn render_interactive_card_canvas(
    ui: &mut Ui,
    state: &mut CardEditorState,
    overview: &WalletOverview,
    testnet: bool,
    palette: &Palette,
) {
    let card_w = ui.available_width().clamp(260.0, 420.0);
    let card_h = 190.0_f32;
    let card_fill = card_backdrop_color(state.draft.background_address.as_deref(), palette);

    let (rect, canvas_resp) = ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click());
    let painter = ui.painter_at(rect);

    // Card surface (`walletCardRadius = 16px`).
    painter.rect_filled(rect, CornerRadius::same(16), card_fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(16),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 48)),
        egui::StrokeKind::Inside,
    );

    // Subtle sheen highlight strip.
    let sheen = Rect::from_min_size(rect.min, Vec2::new(rect.width(), card_h * 0.42));
    painter.rect_filled(
        sheen,
        CornerRadius {
            nw: 16,
            ne: 16,
            sw: 0,
            se: 0,
        },
        Color32::from_rgba_unmultiplied(255, 255, 255, 18),
    );

    // Paint balance & address anchors matching `wallet_own_card.cpp`.
    let balance_str = format_ton(overview.balance_nano as i64);
    let fiat_usd = (overview.balance_nano as f64 / NANO_PER_TON as f64) * overview.usd_per_ton;
    let friendly = overview
        .address
        .map(|a| a.to_friendly(false, testnet))
        .unwrap_or_default();
    let short_addr = super::address_text::short_address(&friendly, 16);

    painter.text(
        Pos2::new(rect.left() + 20.0, rect.top() + 24.0),
        Align2::LEFT_CENTER,
        format!("TON Wallet · {}", overview.version.label()),
        FontId::proportional(12.5),
        Color32::from_rgba_unmultiplied(255, 255, 255, 215),
    );
    painter.text(
        Pos2::new(rect.left() + 20.0, rect.top() + 74.0),
        Align2::LEFT_CENTER,
        format!("{balance_str} TON"),
        FontId::proportional(24.0),
        palette.window_fg_active,
    );
    painter.text(
        Pos2::new(rect.left() + 20.0, rect.top() + 102.0),
        Align2::LEFT_CENTER,
        format!("≈ ${fiat_usd:.2} USD"),
        FontId::proportional(13.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 200),
    );
    painter.text(
        Pos2::new(rect.left() + 20.0, rect.bottom() - 20.0),
        Align2::LEFT_CENTER,
        short_addr,
        FontId::monospace(11.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 220),
    );

    // Paint placed collectible stickers with interactive click-to-select & subtle selection ring.
    let mut clicked_sticker: Option<String> = None;
    for (idx, sticker) in state.draft.stickers.iter().enumerate() {
        let char_count = sticker.title.chars().count();
        let short_tag = if char_count > 13 {
            let prefix: String = sticker.title.chars().take(12).collect();
            format!("* {prefix}…")
        } else {
            format!("* {}", sticker.title)
        };
        let label_galley =
            painter.layout_no_wrap(short_tag, FontId::proportional(11.0), Color32::WHITE);
        let min_w = (label_galley.size().x + 16.0).min(rect.width() - 24.0);
        let sw = (rect.width() * (sticker.coordinates.w as f32 / 100.0))
            .max(min_w)
            .min(rect.width() - 20.0);
        let sh = (rect.height() * (sticker.coordinates.h as f32 / 100.0)).clamp(34.0, 84.0);
        let raw_cx = rect.left() + rect.width() * (sticker.coordinates.x as f32 / 100.0);
        let raw_cy = rect.top() + rect.height() * (sticker.coordinates.y as f32 / 100.0);
        let cx = raw_cx.clamp(rect.left() + sw * 0.5 + 8.0, rect.right() - sw * 0.5 - 8.0);
        let cy = raw_cy.clamp(rect.top() + sh * 0.5 + 8.0, rect.bottom() - sh * 0.5 - 8.0);
        let srect = Rect::from_center_size(Pos2::new(cx, cy), Vec2::new(sw, sh));

        let is_selected = state
            .selected_sticker
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(&sticker.nft_address));
        let sel_t = ui.ctx().animate_bool_with_time(
            egui::Id::unique(("tg_wallet_card_sticker_sel", idx, &sticker.nft_address)),
            is_selected,
            SELECTION_ANIM_SECS,
        );

        let badge_fill =
            Color32::from_rgba_unmultiplied(255, 255, 255, (46.0 + sel_t * 36.0).round() as u8);
        painter.rect_filled(srect, CornerRadius::same(10), badge_fill);

        let ring_stroke = if sel_t > 0.05 {
            Stroke::new(
                1.5 + sel_t * 0.5,
                Color32::from_rgba_unmultiplied(255, 255, 255, (140.0 + sel_t * 115.0) as u8),
            )
        } else {
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 95))
        };
        painter.rect_stroke(
            srect,
            CornerRadius::same(10),
            ring_stroke,
            egui::StrokeKind::Inside,
        );

        let text_pos = Pos2::new(
            srect.center().x - label_galley.size().x * 0.5,
            srect.center().y - label_galley.size().y * 0.5,
        );
        painter.galley(text_pos, label_galley, Color32::WHITE);

        if canvas_resp.clicked()
            && let Some(pos) = canvas_resp.interact_pointer_pos()
            && srect.contains(pos)
        {
            clicked_sticker = Some(sticker.nft_address.clone());
        }
    }

    if let Some(addr) = clicked_sticker {
        state.selected_sticker = Some(addr);
    } else if canvas_resp.clicked()
        && let Some(pos) = canvas_resp.interact_pointer_pos()
        && let Some(sel_addr) = state.selected_sticker.clone()
        && let Some(entry) = state
            .draft
            .stickers
            .iter_mut()
            .find(|s| s.nft_address.eq_ignore_ascii_case(&sel_addr))
    {
        // Clicking open space on the card moves the selected sticker directly there.
        let nx = (((pos.x - rect.left()) / rect.width()) * 100.0).clamp(10.0, 90.0);
        let ny = (((pos.y - rect.top()) / rect.height()) * 100.0).clamp(14.0, 86.0);
        entry.coordinates.x = f64::from(nx);
        entry.coordinates.y = f64::from(ny);
        state.dirty = true;
    }
}

// ============================================================================
// 2. Move-to-Gifts Sheet (`wallet_move_to_gifts.cpp`)
// ============================================================================

/// Stateful controller for the Move-to-Gifts deposit/display sheet.
#[derive(Debug, Clone, Default)]
pub struct MoveToGiftsState {
    /// Collectible NFT item currently being moved (`None` = sheet closed).
    pub target_nft: Option<NftItem>,
    /// Whether to also display the gift on the user's Telegram Gifts list (`payments.displayStarGift`).
    pub also_display_in_gifts: bool,
    /// Whether to hide sender details (`payments.depositStarGift.hide_sender`).
    pub hide_sender: bool,
    /// Verified server deposit quote awaiting explicit user confirmation press.
    pub pending_quote: Option<MoveToGiftsQuote>,
    /// Final receipt once the user explicitly confirmed the move.
    pub receipt: Option<MoveToGiftsReceipt>,
    /// Inline validation or backend error.
    pub error: Option<String>,
}

impl MoveToGiftsState {
    /// Opens the Move-to-Gifts sheet for `item`.
    pub fn open_for(&mut self, item: NftItem) {
        self.target_nft = Some(item);
        self.also_display_in_gifts = true;
        self.hide_sender = false;
        self.pending_quote = None;
        self.receipt = None;
        self.error = None;
    }

    /// Closes and resets the Move-to-Gifts sheet.
    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// Returns `true` while the sheet is active.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.target_nft.is_some()
    }
}

/// Outcome returned by [`render_move_to_gifts_sheet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveToGiftsOutcome {
    /// Keep sheet open.
    None,
    /// User closed/cancelled the sheet.
    Closed,
    /// Move completed after explicit user confirmation; emit toast.
    Moved(String),
}

/// Renders the Move-to-Gifts deposit/display confirmation flow inside `ui`.
pub fn render_move_to_gifts_sheet(
    ui: &mut Ui,
    state: &mut MoveToGiftsState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
) -> MoveToGiftsOutcome {
    let Some(item) = state.target_nft.clone() else {
        return MoveToGiftsOutcome::None;
    };
    let mut outcome = MoveToGiftsOutcome::None;

    // Step 3: Completion receipt.
    if let Some(receipt) = state.receipt.clone() {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.window_bg_active))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(tr::lng_gift_transfer_to_gifts_done())
                        .size(15.5)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Collectible: {}", nft_display_title(&item)))
                        .size(13.0)
                        .color(palette.window_fg),
                );
                ui.label(
                    RichText::new(format!("Tx hash: {}", receipt.message_hash_hex))
                        .monospace()
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );
                if receipt.shown_in_gifts {
                    ui.label(
                        RichText::new("Visible on your Telegram Profile -> Gifts")
                            .size(12.0)
                            .color(palette.window_active_text_fg),
                    );
                }
                ui.add_space(10.0);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Done")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active),
                    )
                    .clicked()
                {
                    state.close();
                    outcome = MoveToGiftsOutcome::Closed;
                }
            });
        return outcome;
    }

    // Step 2: Explicit User Confirmation (HARD MONEY RULE: never auto-submits).
    if let Some(quote) = state.pending_quote.clone() {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.5, palette.window_bg_active))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!("Confirm {}", tr::lng_gift_transfer_to_gifts()))
                        .size(15.5)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Gift: {} ({})", quote.title, quote.slug))
                        .size(13.0)
                        .color(palette.window_fg),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Verified escrow: {}", quote.escrow_address))
                            .monospace()
                            .size(11.0)
                            .color(palette.window_sub_text_fg),
                    )
                    .wrap(),
                );
                ui.label(
                    RichText::new(format!(
                        "Deposit gas: {} TON · Est. fee: {} TON",
                        format_ton(quote.amount_nano as i64),
                        format_ton(quote.fee_nano as i64),
                    ))
                    .size(12.5)
                    .color(palette.window_fg),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut state.also_display_in_gifts,
                    tr::lng_wallet_collectible_show_in_gifts(),
                );
                if let Some(ref err) = state.error {
                    ui.add_space(4.0);
                    ui.label(RichText::new(err).size(12.0).color(palette.attention_fg));
                }
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(tr::lng_gift_transfer_to_gifts_button())
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        match backend.confirm_move_to_gifts(&quote, state.also_display_in_gifts) {
                            Ok(receipt) => {
                                state.pending_quote = None;
                                state.error = None;
                                state.receipt = Some(receipt);
                                outcome = MoveToGiftsOutcome::Moved(
                                    tr::lng_gift_transfer_to_gifts_done(),
                                );
                            }
                            Err(err) => {
                                state.error = Some(err);
                            }
                        }
                    }
                    if ui.button(tr::lng_cancel()).clicked() {
                        state.pending_quote = None;
                        state.error = None;
                    }
                });
            });
        return outcome;
    }

    // Step 1: Review & Prepare deposit payload (`payments.depositStarGift`).
    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(tr::lng_gift_transfer_to_gifts())
                    .size(15.5)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(tr::lng_gift_transfer_to_gifts_text())
                        .size(13.0)
                        .color(palette.window_fg),
                )
                .wrap(),
            );
            ui.add_space(6.0);
            Frame::NONE
                .fill(palette.window_bg)
                .stroke(Stroke::new(1.0, palette.divider))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(nft_display_title(&item))
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} My Profile -> Gifts",
                            tr::lng_gift_transfer_to_gifts_appear()
                        ))
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                    );
                });
            ui.add_space(8.0);
            ui.checkbox(
                &mut state.also_display_in_gifts,
                tr::lng_wallet_collectible_show_in_gifts(),
            );
            ui.checkbox(&mut state.hide_sender, "Hide sender details");

            if let Some(ref err) = state.error {
                ui.add_space(4.0);
                ui.label(RichText::new(err).size(12.0).color(palette.attention_fg));
            }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Review Move to Gifts")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6)),
                    )
                    .clicked()
                {
                    let slug = if !item.key.is_empty() {
                        item.key.clone()
                    } else {
                        nft_display_title(&item)
                    };
                    match backend.prepare_move_to_gifts(&item.address, &slug, state.hide_sender) {
                        Ok(quote) => {
                            state.pending_quote = Some(quote);
                            state.error = None;
                        }
                        Err(err) => {
                            state.error = Some(err);
                        }
                    }
                }
                if ui.button(tr::lng_cancel()).clicked() {
                    state.close();
                    outcome = MoveToGiftsOutcome::Closed;
                }
            });
        });

    outcome
}
