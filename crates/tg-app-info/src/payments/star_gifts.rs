//! Star Gifts UI: gift picker grid, per-peer gifts list with filters & collections,
//! unique gift actions (upgrade, transfer, wear/unwear, resale pricing, pin/hide/convert),
//! resale marketplace, live auctions, and crafting forge.
//!
//! Port origin: `boxes/star_gift_box.cpp`, `boxes/transfer_gift_box.cpp`,
//! `info/peer_gifts/*`, `boxes/star_gift_resale_box.cpp`,
//! `boxes/star_gift_auction_box.cpp`, and `boxes/star_gift_craft_box.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    TextEdit, Ui, Vec2,
};
use tg_data::CollectibleId;
use tg_payments::credits::CreditsAmount;
use tg_payments::star_gifts::{
    COLLECTION_NAME_LIMIT, GiftCollection, PeerGiftsFilter, StarGiftLifecycleState,
};

use crate::payments::backend::{
    CatalogGiftItem, PaymentsBackend, ReceivedGiftAction, ReceivedGiftItem, StarGiftsBundle,
};
use crate::payments::credits::format_stars_count;
use crate::payments::marketplace_backend::{StarGiftsMarketplaceBackend, UniqueGiftDetailBundle};
use crate::payments::star_gifts_marketplace::{
    AuctionsView, CraftForgeView, ResaleMarketplaceView,
};
use crate::payments::ui_helpers::{
    PREMIUM_PURPLE, STAR_GOLD, STAR_SURFACE, SUCCESS_GREEN, format_unix_date, paint_gift_artwork,
    paint_star_emblem, pill_button, primary_button, rgb_to_color32, section_divider,
    subsection_title, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Filter tab for the Star Gifts screen (`kPriceTabAll` / `kPriceTabMy` / Marketplace / Auctions / Craft).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StarGiftsTab {
    /// All catalog gifts.
    #[default]
    AllCatalog,
    /// Limited-supply catalog gifts only.
    Limited,
    /// Upgradable catalog gifts only.
    Upgradable,
    /// Received gifts on the peer's profile with filters & collections.
    Received,
    /// Resale marketplace listings (`boxes/star_gift_resale_box.cpp`).
    Resale,
    /// Live Star Gift auctions (`boxes/star_gift_auction_box.cpp`).
    Auctions,
    /// Star Gift crafting forge (`boxes/star_gift_craft_box.cpp`).
    Craft,
}

/// Withdrawal target mode in the TON blockchain withdrawal dialog (`boxes/transfer_gift_box.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TonWithdrawalMode {
    /// Generate a Fragment NFT withdrawal link (`payments.getStarGiftWithdrawalUrl`).
    #[default]
    FragmentUrl,
    /// Direct TON wallet withdrawal (`payments.withdrawStarGift`).
    TonWallet,
}

/// Active dialog over the Star Gifts screen.
#[derive(Debug, Clone)]
pub enum StarGiftModal {
    /// Send a catalog gift with optional message, anonymous flag, and pre-paid upgrade toggle.
    SendGift {
        /// Selected catalog gift item.
        item: CatalogGiftItem,
        /// Recipient display name.
        recipient: String,
        /// Optional gift message text (max 255 chars).
        message: String,
        /// Hide sender name on recipient's profile.
        hide_name: bool,
        /// Pre-pay collectible upgrade (if `gift.upgradable`).
        include_upgrade: bool,
    },
    /// View a received gift with attributes & Upgrade/Convert/Transfer/Wear/Resale/Pin actions.
    ReceivedDetail(ReceivedGiftItem),
    /// Transfer a unique collectible Star Gift to a chosen recipient (`boxes/transfer_gift_box.cpp`).
    TransferGift {
        /// Collectible gift being transferred.
        item: ReceivedGiftItem,
        /// Selected recipient username or display name.
        recipient: String,
        /// Known contact/peer candidates for quick selection.
        candidates: Vec<String>,
        /// Optional inline validation/RPC error.
        error: Option<String>,
    },
    /// Withdraw a unique collectible Star Gift to the TON blockchain / Fragment
    /// (`boxes/transfer_gift_box.cpp` — confirmation & 2FA SRP password flow).
    WithdrawToTon {
        /// Collectible gift being withdrawn.
        item: ReceivedGiftItem,
        /// Selected withdrawal mode (Fragment URL vs direct TON wallet).
        mode: TonWithdrawalMode,
        /// 2FA cloud password input (masked).
        password: String,
        /// Whether to keep the collectible visible on the profile after TON withdrawal.
        keep_on_profile: bool,
        /// Optional inline validation or RPC error.
        error: Option<String>,
        /// Returned Fragment / TON withdrawal URL when `get_star_gift_withdrawal_url` succeeds.
        result_url: Option<String>,
        /// Returned confirmation message when `withdraw_star_gift` succeeds.
        result_notice: Option<String>,
    },
    /// Confirm permanent deletion of a received Star Gift (`payments.deleteStarGift`).
    ConfirmDeleteGift {
        /// Saved gift to delete.
        item: ReceivedGiftItem,
        /// Optional inline error from the backend.
        error: Option<String>,
    },
    /// Inspect a unique collectible fetched by slug (`payments.getUniqueStarGift`).
    UniqueSlugDetail(UniqueGiftDetailBundle),
}

/// Interactive Star Gifts picker, collections, and marketplace view-model (`boxes/star_gift_box.cpp`).
pub struct StarGiftsView {
    /// User's available Stars balance.
    pub balance: CreditsAmount,
    /// Default recipient display name.
    pub default_recipient: String,
    /// Catalog gifts list.
    pub catalog: Vec<CatalogGiftItem>,
    /// Received gifts list (filtered by `peer_filter` when loaded from marketplace backend).
    pub received: Vec<ReceivedGiftItem>,
    /// Active filter tab.
    pub active_tab: StarGiftsTab,
    /// Active modal box (Send Gift, Received Gift Detail, or Unique Slug Detail).
    pub modal: Option<StarGiftModal>,
    /// Per-peer saved gifts filter & sorting (`Info::PeerGifts::Filter`).
    pub peer_filter: PeerGiftsFilter,
    /// Loaded Star Gift collections for the current peer.
    pub collections: Vec<GiftCollection>,
    /// Currently worn unique collectible ID, if any.
    pub worn_collectible_id: Option<CollectibleId>,
    /// New collection title input (`<= 12` chars).
    pub new_collection_title: String,
    /// Slug lookup input (`payments.getUniqueStarGift`).
    pub slug_lookup_input: String,
    /// Transfer target username/name input for `payments.transferStarGift`.
    pub transfer_recipient_input: String,
    /// Resale price input (Stars) for `payments.updateStarGiftPrice`.
    pub resale_price_input: String,
    /// Whether to keep original inscription details when upgrading.
    pub upgrade_keep_details: bool,
    /// Whether per-peer gifts & collections have been synced with the marketplace backend.
    pub peer_gifts_loaded: bool,
    /// Whether notifications for new Star Gifts on this peer/channel are enabled
    /// (`payments.toggleChatStarGiftNotifications`).
    pub chat_notifications_enabled: bool,
    /// Resale marketplace sub-view.
    pub resale_view: ResaleMarketplaceView,
    /// Auctions sub-view.
    pub auctions_view: AuctionsView,
    /// Crafting forge sub-view.
    pub craft_view: CraftForgeView,
}

impl StarGiftsView {
    /// Creates a [`StarGiftsView`] from a [`StarGiftsBundle`].
    #[must_use]
    pub fn new(bundle: StarGiftsBundle) -> Self {
        Self {
            balance: bundle.balance,
            default_recipient: bundle.default_recipient,
            catalog: bundle.catalog,
            received: bundle.received,
            active_tab: StarGiftsTab::AllCatalog,
            modal: None,
            peer_filter: PeerGiftsFilter::default(),
            collections: Vec::new(),
            worn_collectible_id: None,
            new_collection_title: String::new(),
            slug_lookup_input: String::new(),
            transfer_recipient_input: String::new(),
            resale_price_input: "350".to_owned(),
            upgrade_keep_details: true,
            peer_gifts_loaded: false,
            chat_notifications_enabled: false,
            resale_view: ResaleMarketplaceView::default(),
            auctions_view: AuctionsView::default(),
            craft_view: CraftForgeView::default(),
        }
    }

    /// Returns catalog gifts matching the active catalog filter tab.
    #[must_use]
    pub fn filtered_catalog(&self) -> Vec<&CatalogGiftItem> {
        self.catalog
            .iter()
            .filter(|item| match self.active_tab {
                StarGiftsTab::AllCatalog => true,
                StarGiftsTab::Limited => item.gift.limited_count > 0,
                StarGiftsTab::Upgradable => item.gift.upgradable,
                StarGiftsTab::Received
                | StarGiftsTab::Resale
                | StarGiftsTab::Auctions
                | StarGiftsTab::Craft => false,
            })
            .collect()
    }

    /// Reloads the peer's filtered saved gifts and collections from `mp`.
    pub fn reload_peer_gifts(
        &mut self,
        mp: &dyn StarGiftsMarketplaceBackend,
    ) -> Result<(), String> {
        let page = mp.load_peer_gifts("", &self.peer_filter, "")?;
        self.received = page.items;
        self.collections = page.collections;
        self.worn_collectible_id = page.worn_collectible_id;
        self.chat_notifications_enabled = mp.chat_star_gift_notifications_enabled("");
        self.peer_gifts_loaded = true;
        Ok(())
    }

    /// Opens the Send Gift modal for `item`.
    pub fn open_send_gift(&mut self, item: CatalogGiftItem) {
        self.modal = Some(StarGiftModal::SendGift {
            item,
            recipient: self.default_recipient.clone(),
            message: String::new(),
            hide_name: false,
            include_upgrade: false,
        });
    }

    /// Opens the Received Gift detail box for `item`.
    pub fn open_received_detail(&mut self, item: ReceivedGiftItem) {
        if let Some(ref u) = item.saved.info.unique
            && u.stars_for_resale > 0
        {
            self.resale_price_input = u.stars_for_resale.to_string();
        }
        self.modal = Some(StarGiftModal::ReceivedDetail(item));
    }

    /// Opens the Transfer Gift recipient picker modal (`boxes/transfer_gift_box.cpp`).
    pub fn open_transfer_gift(&mut self, item: ReceivedGiftItem, candidates: Vec<String>) {
        let initial = candidates.first().cloned().unwrap_or_default();
        self.modal = Some(StarGiftModal::TransferGift {
            item,
            recipient: initial,
            candidates,
            error: None,
        });
    }

    /// Opens the TON blockchain / Fragment withdrawal confirmation modal
    /// (`boxes/transfer_gift_box.cpp`).
    pub fn open_withdraw_to_ton(&mut self, item: ReceivedGiftItem) {
        self.modal = Some(StarGiftModal::WithdrawToTon {
            item,
            mode: TonWithdrawalMode::FragmentUrl,
            password: String::new(),
            keep_on_profile: true,
            error: None,
            result_url: None,
            result_notice: None,
        });
    }

    /// Opens the permanent gift deletion confirmation modal (`payments.deleteStarGift`).
    pub fn open_delete_gift_confirm(&mut self, item: ReceivedGiftItem) {
        self.modal = Some(StarGiftModal::ConfirmDeleteGift { item, error: None });
    }

    /// Computes total Stars cost for sending `item` with optional pre-paid upgrade.
    #[must_use]
    pub const fn compute_send_cost(item: &CatalogGiftItem, include_upgrade: bool) -> i64 {
        if include_upgrade && item.gift.upgradable {
            item.gift.stars.saturating_add(item.gift.stars_to_upgrade)
        } else {
            item.gift.stars
        }
    }

    /// Renders the Star Gifts screen (header, filter/marketplace tabs, active body, and modals).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        if !self.peer_gifts_loaded
            && let Some(mp) = backend.as_marketplace_backend()
        {
            let _ = self.reload_peer_gifts(mp);
        }

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("Star Gifts & Marketplace")
                                .size(16.5)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "Recipient: {}  •  Balance: {} Stars{}",
                                self.default_recipient,
                                format_stars_count(self.balance.whole()),
                                self.worn_collectible_id
                                    .map(|id| format!("  •  Wearing #{}", id.raw()))
                                    .unwrap_or_default()
                            ))
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                        );
                    });
                });

                ui.add_space(10.0);

                // Category & marketplace tabs
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    let tabs = [
                        (StarGiftsTab::AllCatalog, "All Gifts"),
                        (StarGiftsTab::Limited, "Limited"),
                        (StarGiftsTab::Upgradable, "Upgradable"),
                        (StarGiftsTab::Received, "My Gifts & Collections"),
                        (StarGiftsTab::Resale, "Resale"),
                        (StarGiftsTab::Auctions, "Auctions"),
                        (StarGiftsTab::Craft, "Craft"),
                    ];
                    for (tab, label) in tabs {
                        let active = self.active_tab == tab;
                        let bg = if active {
                            palette.window_bg_active
                        } else {
                            palette.window_bg_over
                        };
                        let fg = if active {
                            palette.window_fg_active
                        } else {
                            palette.window_fg
                        };
                        if pill_button(ui, label, bg, fg, None).clicked() {
                            self.active_tab = tab;
                            self.modal = None;
                        }
                    }
                });
            });

        // Show active Send Gift or Received/Unique Gift detail box above the content.
        if let Some(modal_toast) = self.show_modal_if_any(ui, backend, palette) {
            toast = Some(modal_toast);
        }

        match self.active_tab {
            StarGiftsTab::Received => {
                if let Some(msg) = self.show_peer_gifts_controls(ui, backend, palette) {
                    toast = Some(msg);
                }
                self.show_received_grid(ui, palette);
            }
            StarGiftsTab::Resale => {
                if let Some(mp) = backend.as_marketplace_backend()
                    && let Some(msg) = self.resale_view.show(ui, mp, palette)
                {
                    toast = Some(msg);
                }
            }
            StarGiftsTab::Auctions => {
                if let Some(mp) = backend.as_marketplace_backend()
                    && let Some(msg) = self.auctions_view.show(ui, mp, palette)
                {
                    toast = Some(msg);
                }
            }
            StarGiftsTab::Craft => {
                if let Some(mp) = backend.as_marketplace_backend()
                    && let Some(msg) = self.craft_view.show(ui, mp, palette)
                {
                    toast = Some(msg);
                }
            }
            StarGiftsTab::AllCatalog | StarGiftsTab::Limited | StarGiftsTab::Upgradable => {
                self.show_catalog_grid(ui, palette);
                section_divider(ui, palette);
                Frame::NONE
                    .inner_margin(Margin::symmetric(18, 4))
                    .show(ui, |ui| {
                        subsection_title(
                            ui,
                            "Recently Received Gifts (Click to Inspect Actions)",
                            palette,
                        );
                    });
                self.show_received_grid(ui, palette);
            }
        }

        toast
    }

    fn show_peer_gifts_controls(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mp = backend.as_marketplace_backend()?;
        let mut toast = None;
        let mut changed = false;

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 4))
            .show(ui, |ui| {
                // Collections strip (`info_peer_gifts_widget.cpp`)
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                    let all_active = self.peer_filter.collection_id.is_none();
                    if pill_button(
                        ui,
                        "All Collections",
                        if all_active {
                            PREMIUM_PURPLE
                        } else {
                            palette.window_bg_over
                        },
                        if all_active {
                            Color32::WHITE
                        } else {
                            palette.window_fg
                        },
                        None,
                    )
                    .clicked()
                        && !all_active
                    {
                        self.peer_filter.collection_id = None;
                        changed = true;
                    }
                    for col in self.collections.clone() {
                        let active = self.peer_filter.collection_id == Some(col.id);
                        let lbl = format!("{} ({})", col.title, col.count);
                        if pill_button(
                            ui,
                            &lbl,
                            if active {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_over
                            },
                            if active {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            None,
                        )
                        .clicked()
                        {
                            self.peer_filter.collection_id = Some(col.id);
                            changed = true;
                        }
                    }
                });

                ui.add_space(6.0);

                // Create / Delete collection bar
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.new_collection_title)
                            .desired_width(130.0)
                            .hint_text(format!("New folder (≤{COLLECTION_NAME_LIMIT})")),
                    );
                    if pill_button(
                        ui,
                        "+ Create Collection",
                        palette.window_bg_over,
                        palette.window_active_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        match mp.create_star_gift_collection("", &self.new_collection_title, &[]) {
                            Ok(created) => {
                                self.new_collection_title.clear();
                                toast = Some(format!("Created collection \"{}\".", created.title));
                                changed = true;
                            }
                            Err(err) => toast = Some(err),
                        }
                    }
                    if let Some(cid) = self.peer_filter.collection_id
                        && pill_button(
                            ui,
                            "Delete Selected",
                            with_alpha(palette.attention_fg, 0.14),
                            palette.attention_fg,
                            None,
                        )
                        .clicked()
                    {
                        match mp.delete_star_gift_collection("", cid) {
                            Ok(msg) => {
                                self.peer_filter.collection_id = None;
                                toast = Some(msg);
                                changed = true;
                            }
                            Err(err) => toast = Some(err),
                        }
                    }
                });

                ui.add_space(6.0);

                // PeerGiftsFilter checkboxes & sort (`Info::PeerGifts::Filter`)
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 4.0);
                    if ui
                        .checkbox(&mut self.peer_filter.sort_by_value, "Sort by Value")
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .checkbox(&mut self.peer_filter.skip_unlimited, "Hide Unlimited")
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .checkbox(&mut self.peer_filter.skip_unique, "Hide Unique")
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .checkbox(&mut self.peer_filter.skip_unsaved, "Hide Unsaved")
                        .changed()
                    {
                        changed = true;
                    }
                    let notify_label = tg_lang::tr::lng_peer_gifts_notify();
                    if ui
                        .checkbox(&mut self.chat_notifications_enabled, notify_label)
                        .changed()
                    {
                        match mp.toggle_chat_star_gift_notifications(
                            "",
                            self.chat_notifications_enabled,
                        ) {
                            Ok(enabled) => {
                                self.chat_notifications_enabled = enabled;
                                toast = Some(if enabled {
                                    tg_lang::tr::lng_peer_gifts_notify_enabled().to_owned()
                                } else {
                                    "Gift notifications disabled.".to_owned()
                                });
                            }
                            Err(err) => {
                                self.chat_notifications_enabled = !self.chat_notifications_enabled;
                                toast = Some(err);
                            }
                        }
                    }
                });

                ui.add_space(6.0);

                // Slug lookup (`payments.getUniqueStarGift`)
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.slug_lookup_input)
                            .desired_width(180.0)
                            .hint_text("Lookup slug (e.g. CrystalPrism-42)"),
                    );
                    if pill_button(
                        ui,
                        "Inspect Slug",
                        palette.window_bg_over,
                        palette.window_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        match mp.get_unique_star_gift(&self.slug_lookup_input) {
                            Ok(detail) => {
                                self.modal = Some(StarGiftModal::UniqueSlugDetail(detail));
                            }
                            Err(err) => toast = Some(err),
                        }
                    }
                });
            });

        if changed && let Err(err) = self.reload_peer_gifts(mp) {
            toast = Some(err);
        }

        toast
    }

    fn show_catalog_grid(&mut self, ui: &mut Ui, palette: &Palette) {
        let items: Vec<CatalogGiftItem> = self.filtered_catalog().into_iter().cloned().collect();
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 4))
            .show(ui, |ui| {
                let cols = 3usize;
                let gap = 10.0;
                let avail = ui.available_width().max(300.0);
                let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                let card_h = 152.0;

                for chunk in items.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for item in chunk {
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click());
                            if resp.hovered() && !item.gift.sold_out {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }

                            let accent = item
                                .gift
                                .background
                                .map(|b| rgb_to_color32(b.center))
                                .unwrap_or(palette.window_bg_active);
                            let edge = item
                                .gift
                                .background
                                .map(|b| rgb_to_color32(b.edge))
                                .unwrap_or(palette.window_bg_active);

                            let card_bg = if resp.hovered() {
                                palette.window_bg_over
                            } else {
                                palette.window_bg
                            };
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(12), card_bg);
                            ui.painter().rect_stroke(
                                rect,
                                CornerRadius::same(12),
                                Stroke::new(1.0, palette.divider),
                                egui::StrokeKind::Inside,
                            );

                            let art_rect = Rect::from_min_size(
                                Pos2::new(rect.left() + 8.0, rect.top() + 8.0),
                                Vec2::new(card_w - 16.0, 82.0),
                            );
                            ui.painter()
                                .rect_filled(art_rect, CornerRadius::same(10), edge);
                            ui.painter().circle_filled(
                                art_rect.center(),
                                34.0,
                                with_alpha(accent, 0.85),
                            );
                            paint_gift_artwork(ui.painter(), art_rect, &item.artwork_kind, edge);

                            if item.gift.sold_out {
                                paint_ribbon(
                                    ui.painter(),
                                    art_rect,
                                    "SOLD OUT",
                                    palette.attention_fg,
                                );
                            } else if item.gift.is_auction() {
                                paint_ribbon(ui.painter(), art_rect, "AUCTION", PREMIUM_PURPLE);
                            } else if item.gift.limited_count > 0 {
                                paint_ribbon(
                                    ui.painter(),
                                    art_rect,
                                    &format!("{} left", item.gift.limited_left),
                                    Color32::from_rgb(0x1D, 0x26, 0x33),
                                );
                            } else if item.gift.upgradable {
                                paint_ribbon(ui.painter(), art_rect, "UPGRADABLE", PREMIUM_PURPLE);
                            }

                            ui.painter().text(
                                Pos2::new(rect.center().x, art_rect.bottom() + 14.0),
                                Align2::CENTER_CENTER,
                                &item.title,
                                FontId::proportional(12.5),
                                palette.window_bold_fg,
                            );

                            let pill_rect = Rect::from_center_size(
                                Pos2::new(rect.center().x, rect.bottom() - 18.0),
                                Vec2::new(82.0, 22.0),
                            );
                            ui.painter().rect_filled(
                                pill_rect,
                                CornerRadius::same(11),
                                STAR_SURFACE,
                            );
                            paint_star_emblem(
                                ui.painter(),
                                Pos2::new(pill_rect.center().x - 16.0, pill_rect.center().y),
                                6.5,
                                STAR_GOLD,
                            );
                            ui.painter().text(
                                Pos2::new(pill_rect.center().x - 4.0, pill_rect.center().y),
                                Align2::LEFT_CENTER,
                                format!("{}", item.gift.stars),
                                FontId::proportional(12.0),
                                STAR_GOLD,
                            );

                            if resp.clicked() && !item.gift.sold_out {
                                self.open_send_gift(item.clone());
                            }
                        }
                    });
                    ui.add_space(gap);
                }
            });
    }

    fn show_received_grid(&mut self, ui: &mut Ui, palette: &Palette) {
        let items = self.received.clone();
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                for item in items {
                    let width = ui.available_width().max(220.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 66.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(10),
                        if resp.hovered() {
                            palette.window_bg_over
                        } else {
                            palette.window_bg
                        },
                    );
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(10),
                        Stroke::new(1.0, palette.divider),
                        egui::StrokeKind::Inside,
                    );

                    let art_bg = if let Some(ref u) = item.saved.info.unique {
                        rgb_to_color32(u.backdrop.center_color)
                    } else if let Some(bg) = item.saved.info.background {
                        rgb_to_color32(bg.center)
                    } else {
                        palette.window_bg_active
                    };
                    let art_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 8.0, rect.top() + 8.0),
                        Vec2::splat(50.0),
                    );
                    ui.painter()
                        .rect_filled(art_rect, CornerRadius::same(8), art_bg);
                    paint_gift_artwork(ui.painter(), art_rect, &item.artwork_kind, art_bg);

                    let worn = item
                        .saved
                        .info
                        .unique
                        .as_ref()
                        .is_some_and(|u| self.worn_collectible_id == Some(u.id));
                    let state_label = lifecycle_badge(item.saved.lifecycle_state());
                    let suffix = if worn { "  •  WEARING" } else { "" };
                    ui.painter().text(
                        Pos2::new(art_rect.right() + 12.0, rect.top() + 21.0),
                        Align2::LEFT_CENTER,
                        format!("{}  •  {}{suffix}", item.title, state_label),
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    ui.painter().text(
                        Pos2::new(art_rect.right() + 12.0, rect.top() + 43.0),
                        Align2::LEFT_CENTER,
                        format!("From {}  •  \"{}\"", item.sender_name, item.saved.message),
                        FontId::proportional(11.5),
                        palette.window_sub_text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 12.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        if item.saved.pinned {
                            "Pinned  >"
                        } else if item.saved.hidden {
                            "Hidden  >"
                        } else {
                            "Inspect  >"
                        },
                        FontId::proportional(12.0),
                        palette.window_active_text_fg,
                    );

                    if resp.clicked() {
                        self.open_received_detail(item);
                    }
                    ui.add_space(6.0);
                }
            });
    }

    fn show_modal_if_any(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let modal = self.modal.clone()?;
        let mut toast = None;
        let mut close = false;
        let mut need_refresh_id: Option<u64> = None;

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.5, palette.window_bg_active))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| match modal {
                        StarGiftModal::SendGift {
                            item,
                            mut recipient,
                            mut message,
                            mut hide_name,
                            mut include_upgrade,
                        } => {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("Send Gift: {}", item.title))
                                        .size(15.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Cancel",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("To:")
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                                ui.add(TextEdit::singleline(&mut recipient).desired_width(200.0));
                            });
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "Gift Message ({}/255)",
                                    message.chars().count()
                                ))
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                            );
                            ui.add(
                                TextEdit::multiline(&mut message)
                                    .desired_rows(2)
                                    .desired_width(ui.available_width())
                                    .hint_text("Write an optional greeting..."),
                            );
                            if message.chars().count() > 255 {
                                message = message.chars().take(255).collect();
                            }
                            ui.add_space(6.0);
                            ui.checkbox(&mut hide_name, "Hide my name (send anonymously)");
                            if item.gift.upgradable {
                                ui.checkbox(
                                    &mut include_upgrade,
                                    format!(
                                        "Make it unique — pre-pay collectible upgrade (+{} Stars)",
                                        item.gift.stars_to_upgrade
                                    ),
                                );
                            }
                            ui.add_space(8.0);
                            let total_cost = Self::compute_send_cost(&item, include_upgrade);
                            if primary_button(
                                ui,
                                &format!("Send Gift for {total_cost} Stars"),
                                palette.window_bg_active,
                                palette.window_fg_active,
                                38.0,
                            )
                            .clicked()
                            {
                                match backend.send_star_gift(
                                    item.gift.id,
                                    &recipient,
                                    &message,
                                    hide_name,
                                    include_upgrade,
                                ) {
                                    Ok(msg) => {
                                        toast = Some(msg);
                                        close = true;
                                    }
                                    Err(err) => toast = Some(err),
                                }
                            }

                            if !close {
                                self.modal = Some(StarGiftModal::SendGift {
                                    item,
                                    recipient,
                                    message,
                                    hide_name,
                                    include_upgrade,
                                });
                            }
                        }
                        StarGiftModal::ReceivedDetail(item) => {
                            let state = item.saved.lifecycle_state();
                            let gift_id = item.saved.info.id;
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&item.title)
                                        .size(15.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Close",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "From {}  •  {}  •  Status: {}",
                                    item.sender_name,
                                    format_unix_date(item.saved.date),
                                    lifecycle_badge(state)
                                ))
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                            );
                            if !item.saved.message.is_empty() {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(format!("\"{}\"", item.saved.message))
                                        .size(12.5)
                                        .italics()
                                        .color(palette.window_fg),
                                );
                            }

                            if let Some(ref unique) = item.saved.info.unique {
                                ui.add_space(8.0);
                                Frame::NONE
                                    .fill(palette.window_bg_over)
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(Margin::same(10))
                                    .show(ui, |ui| {
                                        attr_row(
                                            ui,
                                            "Model",
                                            &unique.model.attribute.name,
                                            &unique.model.attribute.rarity_text(),
                                            unique
                                                .model
                                                .attribute
                                                .rarity_type()
                                                .badge_color()
                                                .map(rgb_to_color32),
                                            palette,
                                        );
                                        attr_row(
                                            ui,
                                            "Backdrop",
                                            &unique.backdrop.attribute.name,
                                            &unique.backdrop.attribute.rarity_text(),
                                            None,
                                            palette,
                                        );
                                        attr_row(
                                            ui,
                                            "Pattern",
                                            &unique.pattern.attribute.name,
                                            &unique.pattern.attribute.rarity_text(),
                                            None,
                                            palette,
                                        );
                                    });

                                // Transfer & Resale controls for unique collectibles
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.add(
                                        TextEdit::singleline(&mut self.transfer_recipient_input)
                                            .desired_width(150.0)
                                            .hint_text("Transfer to @user"),
                                    );
                                    if pill_button(
                                        ui,
                                        "Transfer Gift",
                                        palette.window_bg_active,
                                        palette.window_fg_active,
                                        None,
                                    )
                                    .clicked()
                                    {
                                        if let Some(mp) = backend.as_marketplace_backend() {
                                            match mp.transfer_star_gift(
                                                gift_id,
                                                &self.transfer_recipient_input,
                                            ) {
                                                Ok(msg) => {
                                                    toast = Some(msg);
                                                    close = true;
                                                    need_refresh_id = Some(0);
                                                }
                                                Err(err) => toast = Some(err),
                                            }
                                        } else {
                                            toast = backend
                                                .apply_received_gift_action(
                                                    gift_id,
                                                    ReceivedGiftAction::Transfer,
                                                )
                                                .ok();
                                        }
                                    }
                                });

                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.add(
                                        TextEdit::singleline(&mut self.resale_price_input)
                                            .desired_width(90.0)
                                            .hint_text("Stars price"),
                                    );
                                    if pill_button(
                                        ui,
                                        if unique.is_for_resale() {
                                            "Update Resale Price"
                                        } else {
                                            "List for Resale"
                                        },
                                        STAR_GOLD,
                                        Color32::WHITE,
                                        None,
                                    )
                                    .clicked()
                                        && let Some(mp) = backend.as_marketplace_backend()
                                    {
                                        match self.resale_price_input.trim().parse::<i64>() {
                                            Ok(stars) if stars > 0 => {
                                                match mp.update_star_gift_price(
                                                    gift_id,
                                                    CreditsAmount::stars(stars),
                                                ) {
                                                    Ok(msg) => {
                                                        toast = Some(msg);
                                                        need_refresh_id = Some(gift_id);
                                                    }
                                                    Err(err) => toast = Some(err),
                                                }
                                            }
                                            _ => {
                                                toast = Some(
                                                    "Enter a positive Stars resale price."
                                                        .to_owned(),
                                                );
                                            }
                                        }
                                    }
                                    if unique.is_for_resale()
                                        && pill_button(
                                            ui,
                                            "Unlist",
                                            palette.window_bg_over,
                                            palette.attention_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        && let Some(mp) = backend.as_marketplace_backend()
                                    {
                                        match mp
                                            .update_star_gift_price(gift_id, CreditsAmount::ZERO_STARS)
                                        {
                                            Ok(msg) => {
                                                toast = Some(msg);
                                                need_refresh_id = Some(gift_id);
                                            }
                                            Err(err) => toast = Some(err),
                                        }
                                    }
                                });
                            }

                            ui.add_space(10.0);
                            if state == StarGiftLifecycleState::Upgradable {
                                ui.checkbox(
                                    &mut self.upgrade_keep_details,
                                    "Keep original gift sender & message inscription",
                                );
                                ui.add_space(4.0);
                            }

                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                                if state == StarGiftLifecycleState::Upgradable
                                    && pill_button(
                                        ui,
                                        &format!(
                                            "Upgrade ({} Stars)",
                                            item.saved.info.stars_to_upgrade
                                        ),
                                        PREMIUM_PURPLE,
                                        Color32::WHITE,
                                        None,
                                    )
                                    .clicked()
                                {
                                    if let Some(mp) = backend.as_marketplace_backend() {
                                        match mp
                                            .upgrade_star_gift(gift_id, self.upgrade_keep_details)
                                        {
                                            Ok(msg) => {
                                                toast = Some(msg);
                                                need_refresh_id = Some(gift_id);
                                            }
                                            Err(err) => toast = Some(err),
                                        }
                                    } else {
                                        toast = backend
                                            .apply_received_gift_action(
                                                gift_id,
                                                ReceivedGiftAction::Upgrade,
                                            )
                                            .ok();
                                    }
                                }

                                if let Some(ref unique) = item.saved.info.unique {
                                    let is_worn = self.worn_collectible_id == Some(unique.id);
                                    let wear_lbl = if is_worn {
                                        "Take Off (Unwear)"
                                    } else {
                                        "Wear as Emoji Status"
                                    };
                                    if pill_button(
                                        ui,
                                        wear_lbl,
                                        if is_worn {
                                            palette.window_bg_over
                                        } else {
                                            SUCCESS_GREEN
                                        },
                                        if is_worn {
                                            palette.window_fg
                                        } else {
                                            Color32::WHITE
                                        },
                                        Some(palette.divider),
                                    )
                                    .clicked()
                                        && let Some(mp) = backend.as_marketplace_backend()
                                    {
                                        let target_id = if is_worn { None } else { Some(unique.id) };
                                        match mp.wear_unique_gift(target_id, None) {
                                            Ok(msg) => {
                                                toast = Some(msg);
                                                need_refresh_id = Some(gift_id);
                                            }
                                            Err(err) => toast = Some(err),
                                        }
                                    }
                                }

                                if matches!(
                                    state,
                                    StarGiftLifecycleState::Regular
                                        | StarGiftLifecycleState::Upgradable
                                ) && pill_button(
                                    ui,
                                    &format!("Convert to {} Stars", item.saved.stars_converted),
                                    STAR_GOLD,
                                    Color32::WHITE,
                                    None,
                                )
                                .clicked()
                                {
                                    if let Some(mp) = backend.as_marketplace_backend() {
                                        match mp.convert_star_gift(gift_id) {
                                            Ok(msg) => {
                                                toast = Some(msg);
                                                close = true;
                                                need_refresh_id = Some(0);
                                            }
                                            Err(err) => toast = Some(err),
                                        }
                                    } else {
                                        toast = backend
                                            .apply_received_gift_action(
                                                gift_id,
                                                ReceivedGiftAction::ConvertToStars,
                                            )
                                            .ok();
                                    }
                                }
                                if state == StarGiftLifecycleState::UniqueOwned
                                    && pill_button(
                                        ui,
                                        "Transfer Gift...",
                                        palette.window_bg_active,
                                        palette.window_fg_active,
                                        None,
                                    )
                                    .clicked()
                                {
                                    let candidates = backend.transfer_recipient_candidates();
                                    self.open_transfer_gift(item.clone(), candidates);
                                }
                                if state == StarGiftLifecycleState::UniqueOwned
                                    && pill_button(
                                        ui,
                                        &tg_lang::tr::lng_gift_transfer_via_blockchain(),
                                        Color32::from_rgb(0x00, 0x88, 0xCC),
                                        Color32::WHITE,
                                        None,
                                    )
                                    .clicked()
                                {
                                    self.open_withdraw_to_ton(item.clone());
                                }

                                let pin_lbl = if item.saved.pinned {
                                    "Unpin from Profile"
                                } else {
                                    "Pin to Profile"
                                };
                                if pill_button(
                                    ui,
                                    pin_lbl,
                                    palette.window_bg_over,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    if let Some(mp) = backend.as_marketplace_backend() {
                                        match mp.toggle_star_gifts_pinned_to_top(&[gift_id]) {
                                            Ok(msg) => {
                                                toast = Some(msg);
                                                need_refresh_id = Some(gift_id);
                                            }
                                            Err(err) => toast = Some(err),
                                        }
                                    } else if let Ok(msg) = backend.apply_received_gift_action(
                                        gift_id,
                                        ReceivedGiftAction::TogglePin,
                                    ) {
                                        toast = Some(msg);
                                        need_refresh_id = Some(gift_id);
                                    }
                                }

                                let vis_lbl = if item.saved.hidden {
                                    "Show on Profile"
                                } else {
                                    "Hide from Profile"
                                };
                                if pill_button(
                                    ui,
                                    vis_lbl,
                                    palette.window_bg_over,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                    && let Some(mp) = backend.as_marketplace_backend()
                                {
                                    match mp.save_star_gift(gift_id, !item.saved.hidden) {
                                        Ok(msg) => {
                                            toast = Some(msg);
                                            need_refresh_id = Some(gift_id);
                                        }
                                        Err(err) => toast = Some(err),
                                    }
                                }

                                if item.saved.mine
                                    && pill_button(
                                        ui,
                                        "Delete Gift",
                                        with_alpha(palette.attention_fg, 0.12),
                                        palette.attention_fg,
                                        Some(with_alpha(palette.attention_fg, 0.35)),
                                    )
                                    .clicked()
                                {
                                    self.open_delete_gift_confirm(item.clone());
                                }
                            });
                        }
                        StarGiftModal::UniqueSlugDetail(detail) => {
                            ui.horizontal(|ui| {
                                let heading = detail
                                    .gift
                                    .unique
                                    .as_ref()
                                    .map(|u| u.display_name())
                                    .unwrap_or_else(|| format!("Gift #{}", detail.gift.id));
                                ui.label(
                                    egui::RichText::new(heading)
                                        .size(15.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Close",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "Owner: {}{}",
                                    detail.owner_name,
                                    if detail.is_worn {
                                        "  •  Currently Worn"
                                    } else {
                                        ""
                                    }
                                ))
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                            );
                            if let Some(ref val) = detail.value_info {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Est. Value: {} {}  •  Floor: {}  •  Listings: {} TG / {} Fragment",
                                        val.value_price,
                                        val.currency,
                                        val.minimum_price,
                                        val.for_sale_on_telegram,
                                        val.for_sale_on_fragment
                                    ))
                                    .size(11.5)
                                    .color(STAR_GOLD),
                                );
                            }
                        }
                        StarGiftModal::TransferGift {
                            item,
                            mut recipient,
                            candidates,
                            mut error,
                        } => {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("Transfer Gift: {}", item.title))
                                        .size(15.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Cancel",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(
                                    "Choose a recipient from your contacts or enter a @username:",
                                )
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                TextEdit::singleline(&mut recipient)
                                    .desired_width(ui.available_width())
                                    .hint_text("@username or contact name"),
                            );
                            if !candidates.is_empty() {
                                ui.add_space(6.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                    for cand in &candidates {
                                        let selected = recipient.eq_ignore_ascii_case(cand);
                                        if pill_button(
                                            ui,
                                            cand,
                                            if selected {
                                                palette.window_bg_active
                                            } else {
                                                palette.window_bg_over
                                            },
                                            if selected {
                                                palette.window_fg_active
                                            } else {
                                                palette.window_fg
                                            },
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            recipient = cand.clone();
                                            error = None;
                                        }
                                    }
                                });
                            }
                            if let Some(ref err) = error {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(11.5)
                                        .color(palette.attention_fg),
                                );
                            }
                            ui.add_space(10.0);
                            if primary_button(
                                ui,
                                "Confirm Transfer",
                                palette.window_bg_active,
                                palette.window_fg_active,
                                36.0,
                            )
                            .clicked()
                            {
                                match backend.transfer_received_gift(item.saved.info.id, &recipient)
                                {
                                    Ok(msg) => {
                                        toast = Some(msg);
                                        if let Ok(bundle) = backend.load_star_gifts() {
                                            self.received = bundle.received;
                                        }
                                        close = true;
                                    }
                                    Err(err) => {
                                        error = Some(err);
                                    }
                                }
                            }
                            if !close {
                                self.modal = Some(StarGiftModal::TransferGift {
                                    item,
                                    recipient,
                                    candidates,
                                    error,
                                });
                            }
                        }
                        StarGiftModal::WithdrawToTon {
                            item,
                            mut mode,
                            mut password,
                            mut keep_on_profile,
                            mut error,
                            mut result_url,
                            mut result_notice,
                        } => {
                            let ton_blue = Color32::from_rgb(0x00, 0x88, 0xCC);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(
                                        tg_lang::tr::lng_gift_transfer_confirm_title(),
                                    )
                                    .size(14.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Cancel",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);

                            // Collectible summary banner
                            let serial_summary = item
                                .saved
                                .info
                                .unique
                                .as_ref()
                                .map(|u| {
                                    format!(
                                        "{}  •  Model: {}  •  Slug: {}",
                                        u.display_name(),
                                        u.model.attribute.name,
                                        u.slug
                                    )
                                })
                                .unwrap_or_else(|| item.title.clone());
                            Frame::NONE
                                .fill(palette.window_bg_over)
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(8))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(serial_summary)
                                            .size(12.0)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    );
                                    ui.add_space(2.0);
                                    ui.label(
                                        egui::RichText::new(
                                            "Exporting converts this collectible into an on-chain TON NFT. Review all details and verify your Two-Step Verification password before confirming.",
                                        )
                                        .size(11.0)
                                        .color(palette.window_sub_text_fg),
                                    );
                                });

                            ui.add_space(6.0);
                            // Mode selector: Fragment URL vs Direct TON Wallet
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                                let modes = [
                                    (TonWithdrawalMode::FragmentUrl, "Fragment Withdrawal URL"),
                                    (TonWithdrawalMode::TonWallet, "Withdraw to TON Wallet"),
                                ];
                                for (m, lbl) in modes {
                                    let active = mode == m;
                                    if pill_button(
                                        ui,
                                        lbl,
                                        if active {
                                            ton_blue
                                        } else {
                                            palette.window_bg_over
                                        },
                                        if active {
                                            Color32::WHITE
                                        } else {
                                            palette.window_fg
                                        },
                                        Some(palette.divider),
                                    )
                                    .clicked()
                                    {
                                        mode = m;
                                        error = None;
                                    }
                                }
                            });

                            if mode == TonWithdrawalMode::TonWallet {
                                ui.add_space(4.0);
                                ui.checkbox(
                                    &mut keep_on_profile,
                                    "Keep collectible visible on my Telegram profile",
                                );
                            }

                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(tg_lang::tr::lng_gift_transfer_password_title())
                                    .size(12.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.label(
                                egui::RichText::new(
                                    tg_lang::tr::lng_gift_transfer_password_description(),
                                )
                                .size(11.0)
                                .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(3.0);
                            ui.add(
                                TextEdit::singleline(&mut password)
                                    .password(true)
                                    .desired_width(ui.available_width())
                                    .hint_text("Enter 2FA cloud password"),
                            );

                            if let Some(ref err) = error {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(11.5)
                                        .color(palette.attention_fg),
                                );
                            }

                            if let Some(ref url) = result_url {
                                ui.add_space(6.0);
                                Frame::NONE
                                    .fill(with_alpha(ton_blue, 0.12))
                                    .stroke(Stroke::new(1.0, with_alpha(ton_blue, 0.45)))
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new("Fragment Withdrawal Link Ready")
                                                .size(11.5)
                                                .strong()
                                                .color(ton_blue),
                                        );
                                        ui.add_space(2.0);
                                        ui.label(
                                            egui::RichText::new(url)
                                                .size(11.0)
                                                .monospace()
                                                .color(palette.window_bold_fg),
                                        );
                                        ui.add_space(4.0);
                                        if pill_button(
                                            ui,
                                            "Copy Withdrawal URL",
                                            ton_blue,
                                            Color32::WHITE,
                                            None,
                                        )
                                        .clicked()
                                        {
                                            ui.ctx().copy_text(url.clone());
                                            toast =
                                                Some("Withdrawal URL copied to clipboard.".to_owned());
                                        }
                                    });
                            }

                            if let Some(ref notice) = result_notice {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(notice)
                                        .size(12.0)
                                        .strong()
                                        .color(SUCCESS_GREEN),
                                );
                            }

                            ui.add_space(6.0);
                            let confirm_label = match mode {
                                TonWithdrawalMode::FragmentUrl => {
                                    "Get Fragment Withdrawal Link".to_owned()
                                }
                                TonWithdrawalMode::TonWallet => {
                                    tg_lang::tr::lng_gift_transfer_confirm_button()
                                }
                            };
                            if primary_button(
                                ui,
                                &confirm_label,
                                ton_blue,
                                Color32::WHITE,
                                36.0,
                            )
                            .clicked()
                            {
                                if let Some(mp) = backend.as_marketplace_backend() {
                                    match mode {
                                        TonWithdrawalMode::FragmentUrl => {
                                            match mp.get_star_gift_withdrawal_url(
                                                item.saved.info.id,
                                                &password,
                                            ) {
                                                Ok(url) => {
                                                    result_url = Some(url);
                                                    error = None;
                                                    toast = Some(
                                                        "Generated Fragment withdrawal URL."
                                                            .to_owned(),
                                                    );
                                                }
                                                Err(err) => error = Some(err),
                                            }
                                        }
                                        TonWithdrawalMode::TonWallet => {
                                            match mp.withdraw_star_gift(
                                                item.saved.info.id,
                                                &password,
                                                keep_on_profile,
                                            ) {
                                                Ok(msg) => {
                                                    result_notice = Some(msg.clone());
                                                    error = None;
                                                    toast = Some(msg);
                                                    need_refresh_id = Some(0);
                                                }
                                                Err(err) => error = Some(err),
                                            }
                                        }
                                    }
                                } else {
                                    error = Some(
                                        "Star Gift TON withdrawal backend is unavailable."
                                            .to_owned(),
                                    );
                                }
                            }

                            if !close {
                                self.modal = Some(StarGiftModal::WithdrawToTon {
                                    item,
                                    mode,
                                    password,
                                    keep_on_profile,
                                    error,
                                    result_url,
                                    result_notice,
                                });
                            }
                        }
                        StarGiftModal::ConfirmDeleteGift { item, mut error } => {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("Delete Gift: {}", item.title))
                                        .size(15.0)
                                        .strong()
                                        .color(palette.attention_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Cancel",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "Are you sure you want to permanently delete \"{}\"? This action cannot be undone.",
                                    item.title
                                ))
                                .size(12.5)
                                .color(palette.window_fg),
                            );
                            if let Some(ref err) = error {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(11.5)
                                        .color(palette.attention_fg),
                                );
                            }
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if pill_button(
                                    ui,
                                    "Confirm Delete Gift",
                                    palette.attention_fg,
                                    Color32::WHITE,
                                    None,
                                )
                                .clicked()
                                {
                                    if let Some(mp) = backend.as_marketplace_backend() {
                                        match mp.delete_star_gift(item.saved.info.id) {
                                            Ok(msg) => {
                                                toast = Some(msg);
                                                close = true;
                                                need_refresh_id = Some(0);
                                            }
                                            Err(err) => error = Some(err),
                                        }
                                    } else {
                                        error = Some(
                                            "Deleting Star Gifts is unavailable on this backend."
                                                .to_owned(),
                                        );
                                    }
                                }
                                if pill_button(
                                    ui,
                                    "Keep Gift",
                                    palette.window_bg_over,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    close = true;
                                }
                            });
                            if !close {
                                self.modal = Some(StarGiftModal::ConfirmDeleteGift { item, error });
                            }
                        }
                    });
            });

        if let Some(refresh_gid) = need_refresh_id {
            if let Some(mp) = backend.as_marketplace_backend() {
                let _ = self.reload_peer_gifts(mp);
            } else if let Ok(bundle) = backend.load_star_gifts() {
                self.received = bundle.received;
            }
            if refresh_gid != 0
                && !close
                && let Some(updated) = self
                    .received
                    .iter()
                    .find(|g| g.saved.info.id == refresh_gid)
                    .cloned()
            {
                self.modal = Some(StarGiftModal::ReceivedDetail(updated));
            }
        }

        if close {
            self.modal = None;
        }
        toast
    }
}

fn paint_ribbon(painter: &egui::Painter, art_rect: Rect, text: &str, bg: Color32) {
    let badge = Rect::from_min_size(
        Pos2::new(art_rect.right() - 72.0, art_rect.top() + 5.0),
        Vec2::new(67.0, 17.0),
    );
    painter.rect_filled(badge, CornerRadius::same(8), with_alpha(bg, 0.9));
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(9.0),
        Color32::WHITE,
    );
}

fn attr_row(
    ui: &mut Ui,
    label: &str,
    value: &str,
    rarity: &str,
    badge_color: Option<Color32>,
    palette: &Palette,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(12.0)
                .color(palette.window_sub_text_fg),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let col = badge_color.unwrap_or(palette.window_active_text_fg);
            ui.label(egui::RichText::new(rarity).size(11.0).strong().color(col));
            ui.label(
                egui::RichText::new(value)
                    .size(12.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
        });
    });
}

fn lifecycle_badge(state: StarGiftLifecycleState) -> &'static str {
    match state {
        StarGiftLifecycleState::Regular => "Regular Gift",
        StarGiftLifecycleState::Upgradable => "Upgradable",
        StarGiftLifecycleState::UniqueOwned => "Unique Collectible",
        StarGiftLifecycleState::ListedForResale => "Listed for Resale",
        StarGiftLifecycleState::ExportedOnChain => "On-Chain NFT",
        StarGiftLifecycleState::Burned => "Burned",
        StarGiftLifecycleState::Refunded => "Refunded",
    }
}

/// Returns whether `state` represents a unique collectible (helper for tests & UI).
#[must_use]
pub const fn is_collectible_state(state: StarGiftLifecycleState) -> bool {
    matches!(
        state,
        StarGiftLifecycleState::UniqueOwned
            | StarGiftLifecycleState::ListedForResale
            | StarGiftLifecycleState::ExportedOnChain
    )
}
