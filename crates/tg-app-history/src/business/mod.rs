//! Telegram Business and Telegram Premium screens for `tg-app`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/sections/settings_business.cpp` ([`hub`])
//! - `Telegram/SourceFiles/settings/business/settings_working_hours.cpp` ([`working_hours`])
//! - `Telegram/SourceFiles/settings/business/settings_location.cpp` ([`location_intro`])
//! - `Telegram/SourceFiles/settings/business/settings_chat_intro.cpp` ([`location_intro`])
//! - `Telegram/SourceFiles/settings/business/settings_quick_replies.cpp`,
//!   `settings_shortcut_messages.cpp`, `data/business/data_shortcut_messages.cpp` ([`quick_replies`])
//! - `Telegram/SourceFiles/settings/business/settings_greeting.cpp`,
//!   `settings_away_message.cpp`, `settings_chatbots.cpp`, `settings_chat_links.cpp`,
//!   `settings_recipients_helper.cpp` ([`automations`], [`ui_helpers`])
//! - `Telegram/SourceFiles/data/business/data_business_common.{h,cpp}`,
//!   `data_business_info.{h,cpp}`, `data_business_chatbots.{h,cpp}` ([`model`])
//! - `Telegram/SourceFiles/settings/sections/settings_premium.cpp`,
//!   `boxes/premium_limits_box.cpp`, `data/data_premium_limits.cpp`,
//!   `data/data_emoji_statuses.h`, `info/profile/info_profile_emoji_status_panel.h`,
//!   `menu/menu_emoji_status.cpp` ([`premium`])
//! - Decoupled [`backend::BusinessBackend`] trait with offline fake in [`mock`]

pub mod automations;
pub use tg_app_core::business::backend;
pub mod hub;
pub mod location_intro;
pub mod mock;
pub use tg_app_core::business::model;
pub mod premium;
pub mod quick_replies;
pub mod ui_helpers;
pub mod working_hours;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers,
    Order, Pos2, Rect, ScrollArea, Sense, Stroke, Ui, Vec2,
};

use crate::business::automations::{
    AutomationAction, AwayMessageView, ChatLinksView, ChatbotsView, GreetingView,
};
use crate::business::backend::BusinessBackend;
use crate::business::hub::{BusinessHubAction, BusinessHubView};
use crate::business::location_intro::{ChatIntroView, LocationView};
use crate::business::mock::MockBusinessBackend;
use crate::business::model::{
    BusinessFeatureKey, BusinessOverviewSnapshot, BusinessShortcutId, PremiumLimitKind,
};
use crate::business::premium::{
    EmojiStatusView, PremiumLimitsView, PremiumSettingsAction, PremiumSettingsView,
};
use crate::business::quick_replies::QuickRepliesView;
use crate::business::working_hours::WorkingHoursView;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

/// Active screen displayed inside [`BusinessScreenHost`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BusinessScreen {
    /// Telegram Business hub (`settings_business.cpp`).
    #[default]
    Hub,
    /// Opening Hours editor with time zones (`settings_working_hours.cpp`).
    WorkingHours,
    /// Business Location & map pin (`settings_location.cpp`).
    Location,
    /// Start Page / Chat Intro (`settings_chat_intro.cpp`).
    ChatIntro,
    /// Quick Replies list, Shortcut Messages editor & `/` compose preview.
    QuickReplies,
    /// Greeting Messages (`settings_greeting.cpp`).
    Greeting,
    /// Away Messages (`settings_away_message.cpp`).
    Away,
    /// Connected Chatbots (`settings_chatbots.cpp`).
    Chatbots,
    /// Business Chat Links (`settings_chat_links.cpp`).
    ChatLinks,
    /// Telegram Premium settings (`settings_premium.cpp`).
    Premium,
    /// Premium Limit Boxes gallery (`boxes/premium_limits_box.cpp`).
    PremiumLimits,
    /// Premium Badge & Emoji Status picker (`data_emoji_statuses.h`).
    EmojiStatus,
}

impl BusinessScreen {
    /// Display title rendered in the top bar.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Hub => "Telegram Business",
            Self::WorkingHours => "Opening Hours",
            Self::Location => "Business Location",
            Self::ChatIntro => "Start Page",
            Self::QuickReplies => "Quick Replies",
            Self::Greeting => "Greeting Messages",
            Self::Away => "Away Messages",
            Self::Chatbots => "Chatbots",
            Self::ChatLinks => "Links to Chat",
            Self::Premium => "Telegram Premium",
            Self::PremiumLimits => "Premium Limits",
            Self::EmojiStatus => "Emoji Status & Badge",
        }
    }
}

/// Multi-screen Telegram Business & Telegram Premium controller with navigation stack,
/// lazy sub-screen views, backend persistence, and toast feedback.
pub struct BusinessScreenHost {
    backend: Box<dyn BusinessBackend>,
    started_at: Instant,
    snapshot: BusinessOverviewSnapshot,
    active_screen: BusinessScreen,
    back_stack: Vec<BusinessScreen>,
    working_hours_view: WorkingHoursView,
    location_view: LocationView,
    chat_intro_view: ChatIntroView,
    quick_replies_view: QuickRepliesView,
    greeting_view: GreetingView,
    away_view: AwayMessageView,
    chatbots_view: ChatbotsView,
    chat_links_view: ChatLinksView,
    premium_view: PremiumSettingsView,
    limits_view: PremiumLimitsView,
    emoji_status_view: EmojiStatusView,
    toast_message: Option<(String, i64)>,
    pending_open_payments_premium: bool,
}

impl BusinessScreenHost {
    /// Creates a new [`BusinessScreenHost`] backed by `backend`.
    ///
    /// # Errors
    /// Returns an error if the initial [`BusinessBackend::load_overview`] fails.
    pub fn try_new(backend: Box<dyn BusinessBackend>) -> Result<Self, String> {
        let snapshot = backend.load_overview()?;
        let working_hours_view =
            WorkingHoursView::new(snapshot.details.hours.clone(), snapshot.timezones.clone());
        let location_view = LocationView::new(snapshot.details.location.clone());
        let chat_intro_view = ChatIntroView::new(
            snapshot.details.intro.clone(),
            snapshot.intro_stickers.clone(),
        );
        let quick_replies_view = QuickRepliesView::new(snapshot.shortcuts.clone());
        let greeting_view = GreetingView::new(snapshot.greeting.clone());
        let away_view = AwayMessageView::new(snapshot.away.clone());
        let chatbots_view = ChatbotsView::new(snapshot.chatbots.clone());
        let chat_links_view = ChatLinksView::new(
            snapshot.chat_links.clone(),
            snapshot.account_username.clone(),
        );

        Ok(Self {
            backend,
            started_at: Instant::now(),
            snapshot,
            active_screen: BusinessScreen::Hub,
            back_stack: Vec::new(),
            working_hours_view,
            location_view,
            chat_intro_view,
            quick_replies_view,
            greeting_view,
            away_view,
            chatbots_view,
            chat_links_view,
            premium_view: PremiumSettingsView::new(),
            limits_view: PremiumLimitsView::default(),
            emoji_status_view: EmojiStatusView::new(),
            toast_message: None,
            pending_open_payments_premium: false,
        })
    }

    /// Creates a [`BusinessScreenHost`] pre-wired to the offline [`MockBusinessBackend`].
    #[must_use]
    pub fn with_mock() -> Self {
        Self::try_new(Box::new(MockBusinessBackend::new()))
            .expect("MockBusinessBackend::load_overview is infallible")
    }

    /// Returns the currently active [`BusinessScreen`].
    #[must_use]
    pub const fn active_screen(&self) -> BusinessScreen {
        self.active_screen
    }

    /// Returns the current [`BusinessOverviewSnapshot`].
    #[must_use]
    pub const fn snapshot(&self) -> &BusinessOverviewSnapshot {
        &self.snapshot
    }

    /// Returns mutable access to [`WorkingHoursView`].
    pub fn working_hours_view_mut(&mut self) -> &mut WorkingHoursView {
        &mut self.working_hours_view
    }

    /// Returns mutable access to [`QuickRepliesView`].
    pub fn quick_replies_view_mut(&mut self) -> &mut QuickRepliesView {
        &mut self.quick_replies_view
    }

    /// Returns mutable access to [`PremiumSettingsView`].
    pub fn premium_view_mut(&mut self) -> &mut PremiumSettingsView {
        &mut self.premium_view
    }

    /// Returns mutable access to [`PremiumLimitsView`].
    pub fn limits_view_mut(&mut self) -> &mut PremiumLimitsView {
        &mut self.limits_view
    }

    /// Switches directly to a top-level screen (clearing the back stack).
    pub fn select_screen(&mut self, screen: BusinessScreen) {
        self.back_stack.clear();
        self.active_screen = screen;
        self.refresh_snapshot();
    }

    /// Pushes `screen` onto the navigation stack.
    pub fn push_screen(&mut self, screen: BusinessScreen) {
        if self.active_screen != screen {
            self.back_stack.push(self.active_screen);
            self.active_screen = screen;
            self.refresh_snapshot();
        }
    }

    /// Pops one screen from the navigation stack, returning `true` if navigated back.
    pub fn go_back(&mut self) -> bool {
        if self.active_screen == BusinessScreen::QuickReplies
            && self.quick_replies_view.active_shortcut_id.is_some()
        {
            self.quick_replies_view.active_shortcut_id = None;
            return true;
        }
        if let Some(prev) = self.back_stack.pop() {
            self.active_screen = prev;
            self.refresh_snapshot();
            true
        } else {
            false
        }
    }

    /// Opens the Shortcut Messages editor for `shortcut_id`.
    pub fn open_shortcut_editor(&mut self, shortcut_id: BusinessShortcutId) {
        self.refresh_snapshot();
        self.quick_replies_view.open_shortcut(shortcut_id);
        self.push_screen(BusinessScreen::QuickReplies);
    }

    /// Consumes any pending request to navigate to `PaymentsScreen::Premium` (read-only invoice
    /// preview triggered from the Subscribe CTA in [`PremiumSettingsView`]).
    #[must_use]
    pub fn take_open_payments_premium(&mut self) -> bool {
        std::mem::take(&mut self.pending_open_payments_premium)
    }

    /// Displays a transient toast notification for 3 seconds.
    pub fn show_toast(&mut self, msg: String, now_ms: i64) {
        self.toast_message = Some((msg, now_ms + 3_000));
    }

    fn refresh_snapshot(&mut self) {
        if let Ok(fresh) = self.backend.load_overview() {
            self.quick_replies_view
                .sync_shortcuts(fresh.shortcuts.clone());
            self.snapshot = fresh;
        }
    }

    /// Renders the Business & Premium panel using wall-clock elapsed time.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette, show_close: bool) -> bool {
        let now_ms = self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
        self.show_at(ui, palette, show_close, now_ms)
    }

    /// Renders the panel at an explicit timestamp `now_ms` (deterministic for tests & screenshots).
    pub fn show_at(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        show_close: bool,
        now_ms: i64,
    ) -> bool {
        let mut closed = false;
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, palette.window_bg);

        // Top navigation bar (48px)
        let top_h = 48.0;
        let top_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), top_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let can_back = !self.back_stack.is_empty()
                            || (self.active_screen == BusinessScreen::QuickReplies
                                && self.quick_replies_view.active_shortcut_id.is_some());
                        if can_back
                            && ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("<")
                                            .size(16.0)
                                            .strong()
                                            .color(palette.window_active_text_fg),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                        {
                            self.go_back();
                        }
                        ui.label(
                            egui::RichText::new(self.active_screen.title())
                                .size(15.5)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        if show_close {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("X")
                                                    .size(14.0)
                                                    .strong()
                                                    .color(palette.window_sub_text_fg),
                                            )
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        closed = true;
                                    }
                                },
                            );
                        }
                    });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(top_rect.left(), top_rect.bottom()),
                    Pos2::new(top_rect.right(), top_rect.bottom()),
                ],
                Stroke::new(1.0, palette.divider),
            );
        });

        // Quick section tab strip (36px)
        let tab_h = 36.0;
        let tab_rect = Rect::from_min_size(
            Pos2::new(rect.left(), top_rect.bottom()),
            Vec2::new(rect.width(), tab_h),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(tab_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(12, 0))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let tabs = [
                            (BusinessScreen::Hub, "Business"),
                            (BusinessScreen::WorkingHours, "Hours"),
                            (BusinessScreen::QuickReplies, "Replies"),
                            (BusinessScreen::Chatbots, "Bots"),
                            (BusinessScreen::Premium, "Premium"),
                            (BusinessScreen::PremiumLimits, "Limits"),
                            (BusinessScreen::EmojiStatus, "Status"),
                        ];
                        for (screen, label) in tabs {
                            let selected = self.active_screen == screen;
                            let col = if selected {
                                palette.window_active_text_fg
                            } else {
                                palette.window_sub_text_fg
                            };
                            let galley = ui.fonts_mut(|f| {
                                f.layout_no_wrap(label.to_owned(), FontId::proportional(12.0), col)
                            });
                            let (r, resp) = ui.allocate_exact_size(
                                Vec2::new(galley.size().x + 4.0, tab_h),
                                Sense::click(),
                            );
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                self.select_screen(screen);
                            }
                            ui.painter().galley(
                                Pos2::new(r.left() + 2.0, r.center().y - galley.size().y / 2.0),
                                galley,
                                col,
                            );
                            if selected {
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(r.left(), r.bottom() - 2.0),
                                        Pos2::new(r.right(), r.bottom() - 2.0),
                                    ],
                                    Stroke::new(2.5, palette.window_active_text_fg),
                                );
                            }
                        }
                    });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(tab_rect.left(), tab_rect.bottom()),
                    Pos2::new(tab_rect.right(), tab_rect.bottom()),
                ],
                Stroke::new(1.0, palette.divider),
            );
        });

        // Scrollable screen body
        let body_rect = Rect::from_min_max(Pos2::new(rect.left(), tab_rect.bottom()), rect.max);
        let mut pending_toast: Option<String> = None;
        let mut pending_push: Option<BusinessScreen> = None;
        let mut pending_shortcut: Option<BusinessShortcutId> = None;
        let mut pending_limits_kind: Option<PremiumLimitKind> = None;
        let mut pending_toggle_premium: Option<bool> = None;
        let mut pending_toggle_sponsored: Option<bool> = None;

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_business_scroll", format!("{:?}", self.active_screen)))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match self.active_screen {
                        BusinessScreen::Hub => {
                            if let Some(act) = BusinessHubView::show(ui, &self.snapshot, palette) {
                                match act {
                                    BusinessHubAction::OpenFeature(key) => {
                                        pending_push = Some(screen_for_feature(key));
                                    }
                                    BusinessHubAction::ToggleSponsored(val) => {
                                        pending_toggle_sponsored = Some(val);
                                    }
                                    BusinessHubAction::OpenPremium => {
                                        pending_push = Some(BusinessScreen::Premium);
                                    }
                                    BusinessHubAction::OpenPremiumLimits => {
                                        pending_push = Some(BusinessScreen::PremiumLimits);
                                    }
                                    BusinessHubAction::OpenEmojiStatus => {
                                        pending_push = Some(BusinessScreen::EmojiStatus);
                                    }
                                    BusinessHubAction::ShowToast(msg) => {
                                        pending_toast = Some(msg);
                                        pending_push = Some(BusinessScreen::Premium);
                                    }
                                }
                            }
                        }
                        BusinessScreen::WorkingHours => {
                            pending_toast =
                                self.working_hours_view
                                    .show(ui, self.backend.as_mut(), palette);
                        }
                        BusinessScreen::Location => {
                            pending_toast =
                                self.location_view.show(ui, self.backend.as_mut(), palette);
                        }
                        BusinessScreen::ChatIntro => {
                            pending_toast =
                                self.chat_intro_view
                                    .show(ui, self.backend.as_mut(), palette);
                        }
                        BusinessScreen::QuickReplies => {
                            pending_toast =
                                self.quick_replies_view
                                    .show(ui, self.backend.as_mut(), palette);
                        }
                        BusinessScreen::Greeting => {
                            let hello_count = self
                                .snapshot
                                .shortcuts
                                .values()
                                .find(|s| s.name == "hello")
                                .map_or(0, |s| s.count());
                            if let Some(act) = self.greeting_view.show(
                                ui,
                                hello_count,
                                &self.snapshot.available_users,
                                self.backend.as_mut(),
                                palette,
                            ) {
                                match act {
                                    AutomationAction::EditShortcut(id) => {
                                        pending_shortcut = Some(id);
                                    }
                                    AutomationAction::Toast(msg) => pending_toast = Some(msg),
                                }
                            }
                        }
                        BusinessScreen::Away => {
                            let away_count = self
                                .snapshot
                                .shortcuts
                                .values()
                                .find(|s| s.name == "away")
                                .map_or(0, |s| s.count());
                            if let Some(act) = self.away_view.show(
                                ui,
                                away_count,
                                &self.snapshot.available_users,
                                self.backend.as_mut(),
                                palette,
                            ) {
                                match act {
                                    AutomationAction::EditShortcut(id) => {
                                        pending_shortcut = Some(id);
                                    }
                                    AutomationAction::Toast(msg) => pending_toast = Some(msg),
                                }
                            }
                        }
                        BusinessScreen::Chatbots => {
                            pending_toast = self.chatbots_view.show(
                                ui,
                                &self.snapshot.available_users,
                                self.backend.as_mut(),
                                palette,
                            );
                        }
                        BusinessScreen::ChatLinks => {
                            pending_toast =
                                self.chat_links_view
                                    .show(ui, self.backend.as_mut(), palette);
                        }
                        BusinessScreen::Premium => {
                            if let Some(act) = self.premium_view.show(ui, &self.snapshot, palette) {
                                match act {
                                    PremiumSettingsAction::OpenLimits(kind) => {
                                        pending_limits_kind = Some(kind);
                                        pending_push = Some(BusinessScreen::PremiumLimits);
                                    }
                                    PremiumSettingsAction::OpenEmojiStatus => {
                                        pending_push = Some(BusinessScreen::EmojiStatus);
                                    }
                                    PremiumSettingsAction::OpenBusinessHub => {
                                        pending_push = Some(BusinessScreen::Hub);
                                    }
                                    PremiumSettingsAction::OpenPaymentsPremium => {
                                        self.pending_open_payments_premium = true;
                                    }
                                    PremiumSettingsAction::SetPremium(val) => {
                                        pending_toggle_premium = Some(val);
                                    }
                                    PremiumSettingsAction::Toast(msg) => pending_toast = Some(msg),
                                }
                            }
                        }
                        BusinessScreen::PremiumLimits => {
                            if let Some(act) = self.limits_view.show(
                                ui,
                                &mut self.snapshot,
                                self.backend.as_mut(),
                                palette,
                            ) {
                                match act {
                                    PremiumSettingsAction::OpenPaymentsPremium => {
                                        self.pending_open_payments_premium = true;
                                    }
                                    PremiumSettingsAction::Toast(msg) => pending_toast = Some(msg),
                                    _ => {}
                                }
                            }
                        }
                        BusinessScreen::EmojiStatus => {
                            pending_toast = self.emoji_status_view.show(
                                ui,
                                &mut self.snapshot,
                                self.backend.as_mut(),
                                palette,
                            );
                        }
                    }
                    ui.add_space(16.0);
                });
        });

        if let Some(val) = pending_toggle_sponsored
            && self.backend.set_sponsored_disabled(val).is_ok()
        {
            self.snapshot.sponsored_disabled = val;
            pending_toast = Some(if val {
                "Sponsored messages hidden.".to_owned()
            } else {
                "Sponsored messages visible.".to_owned()
            });
        }
        if let Some(val) = pending_toggle_premium
            && self.backend.set_premium_enabled(val).is_ok()
        {
            self.snapshot.is_premium = val;
            pending_toast = Some(if val {
                "Simulated Telegram Premium: Active.".to_owned()
            } else {
                "Simulated Telegram Premium: Free tier.".to_owned()
            });
        }
        if let Some(kind) = pending_limits_kind {
            self.limits_view.active_kind = kind;
        }
        if let Some(sc_id) = pending_shortcut {
            self.open_shortcut_editor(sc_id);
        } else if let Some(next) = pending_push {
            self.push_screen(next);
        }
        if pending_toast.is_some() {
            self.refresh_snapshot();
        }
        if let Some(msg) = pending_toast {
            self.show_toast(msg, now_ms);
        }

        // Transient bottom toast banner
        if let Some((ref text, expires_at)) = self.toast_message {
            if now_ms >= expires_at {
                self.toast_message = None;
            } else {
                let toast_w = (rect.width() - 40.0).min(460.0);
                let toast_rect = Rect::from_min_size(
                    Pos2::new(rect.center().x - toast_w / 2.0, rect.bottom() - 48.0),
                    Vec2::new(toast_w, 34.0),
                );
                ui.painter().rect_filled(
                    toast_rect,
                    CornerRadius::same(8),
                    Color32::from_rgba_unmultiplied(0x1E, 0x24, 0x2B, 235),
                );
                ui.painter().text(
                    toast_rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(11.5),
                    Color32::WHITE,
                );
                ui.ctx().request_repaint();
            }
        }

        closed
    }
}

const fn screen_for_feature(key: BusinessFeatureKey) -> BusinessScreen {
    match key {
        BusinessFeatureKey::GreetingMessage => BusinessScreen::Greeting,
        BusinessFeatureKey::AwayMessage => BusinessScreen::Away,
        BusinessFeatureKey::QuickReplies => BusinessScreen::QuickReplies,
        BusinessFeatureKey::BusinessHours => BusinessScreen::WorkingHours,
        BusinessFeatureKey::BusinessLocation => BusinessScreen::Location,
        BusinessFeatureKey::ChatLinks => BusinessScreen::ChatLinks,
        BusinessFeatureKey::ChatIntro => BusinessScreen::ChatIntro,
        BusinessFeatureKey::BusinessBots => BusinessScreen::Chatbots,
        BusinessFeatureKey::FolderTags => BusinessScreen::Premium,
    }
}

/// Self-contained overlay host for `app.rs` (single-line hook in `TgforkApp::ui`).
///
/// Press `Ctrl+Shift+B` in the running desktop app to open or close the Telegram Business
/// & Premium panel over the workspace.
pub struct AppBusinessHost {
    open: bool,
    screen_host: BusinessScreenHost,
}

impl AppBusinessHost {
    fn get_or_init_state(ctx: &Context) -> Arc<Mutex<Self>> {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Id::unique("tg_app_business_host"), || {
                Arc::new(Mutex::new(Self {
                    open: false,
                    screen_host: BusinessScreenHost::with_mock(),
                }))
            })
            .clone()
        })
    }

    /// Returns `(is_business_account, sorted_quick_reply_shortcuts)` from the shared Business host.
    #[must_use]
    pub fn quick_replies_snapshot(
        ctx: &Context,
    ) -> (bool, Vec<crate::business::model::QuickReplyShortcut>) {
        let state_arc = Self::get_or_init_state(ctx);
        let Ok(guard) = state_arc.lock() else {
            return (false, Vec::new());
        };
        let mut shortcuts: Vec<crate::business::model::QuickReplyShortcut> = guard
            .screen_host
            .snapshot
            .shortcuts
            .values()
            .cloned()
            .collect();
        shortcuts.sort_by(|a, b| a.name.cmp(&b.name));
        (guard.screen_host.snapshot.is_premium, shortcuts)
    }

    /// Opens the floating Business & Premium overlay on its hub screen.
    pub fn open_overlay(ctx: &Context) {
        let state_arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = state_arc.lock() {
            guard.screen_host.active_screen = BusinessScreen::Hub;
            guard.screen_host.back_stack.clear();
            guard.open = true;
        }
    }

    /// Returns `true` when the floating Business & Premium overlay is currently open.
    #[must_use]
    pub fn is_open(ctx: &Context) -> bool {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc.lock().is_ok_and(|g| g.open)
    }

    /// Opens the overlay directly on the Emoji Status selector (main menu "Set Emoji Status"
    /// for Premium users, `window_main_menu.cpp` -> `EmojiStatusPanel`).
    pub fn open_emoji_status(ctx: &Context) {
        let state_arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = state_arc.lock() {
            guard.screen_host.back_stack.clear();
            guard.screen_host.active_screen = BusinessScreen::EmojiStatus;
            guard.open = true;
        }
    }

    /// Returns the overlay's active screen while it is open.
    #[must_use]
    pub fn open_screen(ctx: &Context) -> Option<BusinessScreen> {
        let state_arc = Self::get_or_init_state(ctx);
        state_arc
            .lock()
            .ok()
            .and_then(|g| g.open.then_some(g.screen_host.active_screen))
    }

    /// Closes the overlay (passcode lock: `Application::closeAdditionalWindows`).
    pub fn close(ctx: &Context) {
        let state_arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = state_arc.lock() {
            guard.open = false;
        }
    }

    /// Opens the floating Business & Premium overlay directly on [`PremiumLimitsView`] focused on `kind`.
    pub fn open_premium_limits(ctx: &Context, kind: crate::business::model::PremiumLimitKind) {
        let state_arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = state_arc.lock() {
            guard.screen_host.limits_view.active_kind = kind;
            guard.screen_host.back_stack.clear();
            guard.screen_host.back_stack.push(BusinessScreen::Hub);
            guard.screen_host.back_stack.push(BusinessScreen::Premium);
            guard.screen_host.active_screen = BusinessScreen::PremiumLimits;
            guard.open = true;
        }
    }

    /// Drives keyboard toggle (`Ctrl+Shift+B`) and renders the floating Business & Premium panel.
    pub fn update_and_render(ctx: &Context, palette: &Palette, _theme_mode: ThemeMode) {
        let toggle_pressed = ctx.input(|i| {
            i.modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT)
                && i.key_pressed(Key::B)
        });

        let state_arc = Self::get_or_init_state(ctx);

        let Ok(mut guard) = state_arc.lock() else {
            return;
        };
        if toggle_pressed {
            guard.open = !guard.open;
        }
        if !guard.open {
            return;
        }

        let screen_rect = ctx.content_rect();
        let panel_w = (screen_rect.width() * 0.62).clamp(460.0, 640.0);
        let panel_h = (screen_rect.height() - 48.0).max(440.0);
        let panel_pos = Pos2::new(
            screen_rect.center().x - panel_w / 2.0,
            screen_rect.top() + 24.0,
        );

        let mut close_requested = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Area::new(Id::unique("tg_business_overlay_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(screen_rect.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen_rect, CornerRadius::ZERO, palette.layer_backdrop);
                if resp.clicked() {
                    close_requested = true;
                }
            });

        let mut open_payments_premium = false;
        egui::Area::new(Id::unique("tg_business_overlay_panel"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter().rect_filled(
                    panel_rect.expand(1.0),
                    CornerRadius::same(10),
                    palette.divider,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    if guard.screen_host.show(ui, palette, true) {
                        close_requested = true;
                    }
                    if guard.screen_host.take_open_payments_premium() {
                        open_payments_premium = true;
                        close_requested = true;
                    }
                });
            });

        if close_requested {
            guard.open = false;
        }
        drop(guard);
        if open_payments_premium {
            tg_app_info::payments::AppPaymentsHost::open_screen(
                ctx,
                tg_app_info::payments::PaymentsScreen::Premium,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui::epaint::{Primitive, TextureId};
    use egui::{Pos2, RawInput, Rect, Vec2};

    use super::*;
    use crate::business::model::{
        AWAY_END_TIME_MIN, AWAY_START_TIME_MIN, ActiveEmojiStatus, AwaySchedule, AwayScheduleType,
        AwaySettings, BotLookupState, BusinessChatTypes, BusinessChats, BusinessLocation,
        BusinessRecipients, CHAT_INTRO_DESCRIPTION_LIMIT, CHAT_INTRO_TITLE_LIMIT, ChatIntro,
        ChatbotsPermissions, EmojiStatusDuration, GeoPoint, GreetingSettings, IN_NEXT_DAY_MAX,
        PremiumLimitKind, PremiumLimitsData, QuickReplyAttachment, SECONDS_PER_DAY,
        SECONDS_PER_WEEK, SHORTCUT_MESSAGES_LIMIT, Timezone, WorkingInterval, WorkingIntervals,
        chatbots_permission_groups, extract_day_intervals, find_closest_timezone_id,
        format_day_time, is_full_open, join_intervals, match_slash_quick_replies,
        parse_bot_username_query, remove_day_intervals, replace_day_intervals, valid_shortcut_name,
    };

    #[test]
    fn working_intervals_sort_merge_and_week_tail_wrap() {
        // Overlapping and adjacent intervals collapse cleanly, empty intervals dropped.
        let raw = WorkingIntervals::new(vec![
            WorkingInterval::new(14 * 3600, 18 * 3600),
            WorkingInterval::new(9 * 3600, 12 * 3600),
            WorkingInterval::new(12 * 3600, 15 * 3600), // bridges 9-12 and 14-18
            WorkingInterval::new(500, 500),             // invalid empty
            WorkingInterval::new(1000, 500),            // invalid reversed
        ]);
        let norm = raw.normalized();
        assert_eq!(norm.list, vec![WorkingInterval::new(9 * 3600, 18 * 3600)]);

        // Sunday night spillover past kWeek (22:00 Sun -> 02:00 Mon) wraps to Monday 00:00..02:00
        // and merges with an existing Monday 01:00..05:00 interval!
        let wrapped = WorkingIntervals::new(vec![
            WorkingInterval::new(6 * SECONDS_PER_DAY + 22 * 3600, SECONDS_PER_WEEK + 2 * 3600),
            WorkingInterval::new(3600, 5 * 3600),
        ])
        .normalized();
        assert_eq!(
            wrapped.list,
            vec![
                WorkingInterval::new(0, 5 * 3600),
                WorkingInterval::new(6 * SECONDS_PER_DAY + 22 * 3600, SECONDS_PER_WEEK),
            ]
        );
    }

    #[test]
    fn extract_day_intervals_handles_overnight_spillover_and_boundaries() {
        // Friday 20:00 to Saturday 03:00 (3h spillover <= 6h IN_NEXT_DAY_MAX).
        let schedule = WorkingIntervals::new(vec![WorkingInterval::new(
            4 * SECONDS_PER_DAY + 20 * 3600,
            5 * SECONDS_PER_DAY + 3 * 3600,
        )])
        .normalized();

        // Friday shows 20:00 - 03:00 (next day).
        let fri = extract_day_intervals(&schedule, 4);
        assert_eq!(
            fri.list,
            vec![WorkingInterval::new(20 * 3600, SECONDS_PER_DAY + 3 * 3600)]
        );
        assert_eq!(join_intervals(&fri), "20:00 - 03:00 (next day)");

        // Saturday omits the 00:00-03:00 spillover owned by Friday!
        let sat = extract_day_intervals(&schedule, 5);
        assert!(sat.list.is_empty());

        // If spillover exceeds 6 hours (e.g. Friday 20:00 to Saturday 08:00), Friday clips to 24:00
        // and Saturday receives 00:00-08:00.
        let long_span = WorkingIntervals::new(vec![WorkingInterval::new(
            4 * SECONDS_PER_DAY + 20 * 3600,
            5 * SECONDS_PER_DAY + 8 * 3600,
        )])
        .normalized();
        let fri_long = extract_day_intervals(&long_span, 4);
        let sat_long = extract_day_intervals(&long_span, 5);
        assert_eq!(
            fri_long.list,
            vec![WorkingInterval::new(20 * 3600, SECONDS_PER_DAY)]
        );
        assert_eq!(sat_long.list, vec![WorkingInterval::new(0, 8 * 3600)]);

        // Full 24-hour checks (`00:00-23:59` and `00:00-24:00`).
        assert!(is_full_open(&WorkingIntervals::new(vec![
            WorkingInterval::new(0, SECONDS_PER_DAY)
        ])));
        assert!(is_full_open(&WorkingIntervals::new(vec![
            WorkingInterval::new(0, SECONDS_PER_DAY - 60)
        ])));
        assert!(!is_full_open(&WorkingIntervals::new(vec![
            WorkingInterval::new(9 * 3600, 18 * 3600)
        ])));
    }

    #[test]
    fn remove_and_replace_day_intervals_preserve_sunday_into_monday_spillover() {
        // Sunday 21:00 -> Monday 02:00 (wrapped in normalized form as [0..2h] and [6d+21h..7d]),
        // plus Monday 10:00..17:00.
        let week = WorkingIntervals::new(vec![
            WorkingInterval::new(6 * SECONDS_PER_DAY + 21 * 3600, SECONDS_PER_WEEK + 2 * 3600),
            WorkingInterval::new(10 * 3600, 17 * 3600),
        ])
        .normalized();

        // Removing Monday (day 0) must keep Sunday's 00:00..02:00 spillover intact!
        let after_remove_mon = remove_day_intervals(&week, 0);
        assert_eq!(
            after_remove_mon.list,
            vec![
                WorkingInterval::new(0, 2 * 3600),
                WorkingInterval::new(6 * SECONDS_PER_DAY + 21 * 3600, SECONDS_PER_WEEK),
            ]
        );
        // Sunday still extracts as 21:00 - 02:00 (next day)!
        let sun = extract_day_intervals(&after_remove_mon, 6);
        assert_eq!(
            sun.list,
            vec![WorkingInterval::new(21 * 3600, SECONDS_PER_DAY + 2 * 3600)]
        );

        // Replacing Tuesday (day 1) with split shift 09:00-13:00 & 14:00-18:00.
        let updated = replace_day_intervals(
            &after_remove_mon,
            1,
            WorkingIntervals::new(vec![
                WorkingInterval::new(9 * 3600, 13 * 3600),
                WorkingInterval::new(14 * 3600, 18 * 3600),
            ]),
        );
        let tue = extract_day_intervals(&updated, 1);
        assert_eq!(join_intervals(&tue), "09:00 - 13:00, 14:00 - 18:00");
        assert_eq!(format_day_time(SECONDS_PER_DAY, true), "00:00 (next day)");
        assert_eq!(format_day_time(SECONDS_PER_DAY, false), "00:00");
        assert_eq!(
            format_day_time(SECONDS_PER_DAY + IN_NEXT_DAY_MAX, true),
            "06:00 (next day)"
        );
    }

    #[test]
    fn timezone_formatting_and_closest_lookup() {
        let zones = vec![
            Timezone {
                id: "America/New_York".to_owned(),
                name: "New York".to_owned(),
                utc_offset: -5 * 3600,
            },
            Timezone {
                id: "Asia/Kolkata".to_owned(),
                name: "Kolkata".to_owned(),
                utc_offset: 5 * 3600 + 1800,
            },
            Timezone {
                id: "Europe/Berlin".to_owned(),
                name: "Berlin".to_owned(),
                utc_offset: 3600,
            },
        ];
        assert_eq!(zones[0].full_name(), "(UTC-5:00) New York");
        assert_eq!(zones[1].full_name(), "(UTC+5:30) Kolkata");
        assert_eq!(
            find_closest_timezone_id(&zones, 2 * 3600).as_deref(),
            Some("Europe/Berlin")
        );
    }

    #[test]
    fn recipients_greeting_and_away_rules() {
        // Empty included forces all_but_excluded = true.
        let r = BusinessRecipients {
            included: BusinessChats::default(),
            excluded: BusinessChats {
                types: BusinessChatTypes::CONTACTS,
                user_ids: vec![2001],
            },
            all_but_excluded: false,
        }
        .make_valid();
        assert!(r.all_but_excluded);
        assert_eq!(r.summary(), "All except 2");

        // Away custom interval bounds (`StartTimeMin` / `EndTimeMin`).
        assert!(AwaySchedule::is_bad_custom_interval(WorkingInterval::new(
            AWAY_START_TIME_MIN - 1,
            AWAY_END_TIME_MIN + 3600,
        )));
        assert!(AwaySchedule::is_bad_custom_interval(WorkingInterval::new(
            AWAY_START_TIME_MIN + 7200,
            AWAY_START_TIME_MIN + 7200,
        )));
        assert!(!AwaySchedule::is_bad_custom_interval(WorkingInterval::new(
            AWAY_START_TIME_MIN,
            AWAY_END_TIME_MIN,
        )));

        let mut backend = MockBusinessBackend::new();
        assert!(
            backend
                .save_away_settings(AwaySettings {
                    recipients: r.clone(),
                    schedule: AwaySchedule {
                        schedule_type: AwayScheduleType::Custom,
                        custom_interval: WorkingInterval::new(100, 200),
                    },
                    shortcut_id: 2,
                    offline_only: false,
                })
                .is_err()
        );
        assert!(
            backend
                .save_greeting_settings(GreetingSettings {
                    recipients: r,
                    no_activity_days: 14,
                    shortcut_id: 1,
                })
                .is_ok()
        );
        assert_eq!(
            backend.load_overview().unwrap().greeting.no_activity_days,
            14
        );
    }

    #[test]
    fn shortcut_name_validation_slash_matching_and_message_limits() {
        assert!(valid_shortcut_name("hello"));
        assert!(valid_shortcut_name("order_status_2026"));
        assert!(valid_shortcut_name("a·b")); // U+00B7 middle dot allowed by C++
        assert!(valid_shortcut_name("a\u{200C}b")); // U+200C ZWNJ allowed by C++
        assert!(valid_shortcut_name("සිංහල")); // Sinhala U+0D80..=U+0DFF allowed by C++
        assert!(!valid_shortcut_name(""));
        assert!(!valid_shortcut_name(&"x".repeat(33)));
        assert!(!valid_shortcut_name("two words"));
        assert!(!valid_shortcut_name("/slash"));

        let mut backend = MockBusinessBackend::new();
        let sc = backend.emplace_shortcut("refund").unwrap();
        for i in 0..SHORTCUT_MESSAGES_LIMIT {
            backend
                .add_shortcut_message(sc.id, &format!("Line {i}"), QuickReplyAttachment::None)
                .unwrap();
        }
        // 21st message rejected!
        assert!(
            backend
                .add_shortcut_message(sc.id, "Too many", QuickReplyAttachment::None)
                .is_err()
        );

        let snap = backend.load_overview().unwrap();
        let all: Vec<_> = snap.shortcuts.values().cloned().collect();
        assert!(match_slash_quick_replies(&all, "hello").is_none());
        let slash_all = match_slash_quick_replies(&all, "/").unwrap();
        assert!(slash_all.len() >= 5);
        let slash_ref = match_slash_quick_replies(&all, "/ref").unwrap();
        assert_eq!(slash_ref[0].name, "refund");
    }

    #[test]
    fn chatbots_query_parsing_search_and_permissions() {
        let p1 = parse_bot_username_query("@DeskAssistBot");
        assert!(p1.valid && p1.value == "DeskAssistBot");
        let p2 = parse_bot_username_query("https://t.me/BookingFlowBot");
        assert!(p2.valid && p2.value == "BookingFlowBot");
        let p3 = parse_bot_username_query("bad bot name!");
        assert!(!p3.valid);

        let backend = MockBusinessBackend::new();
        let found = backend.search_business_bots("@DeskAssistBot");
        assert_eq!(found.state, BotLookupState::Ready);
        assert_eq!(found.bots.len(), 1);
        assert_eq!(found.bots[0].username, "DeskAssistBot");

        let unsupported = backend.search_business_bots("@BasicPingBot");
        assert_eq!(unsupported.state, BotLookupState::Unsupported);
        assert!(unsupported.bots.is_empty());

        let mut perms = ChatbotsPermissions::default();
        assert!(perms.contains(ChatbotsPermissions::VIEW_MESSAGES));
        perms.set(ChatbotsPermissions::VIEW_MESSAGES, false);
        assert!(perms.contains(ChatbotsPermissions::VIEW_MESSAGES)); // locked on
        perms.set(ChatbotsPermissions::MANAGE_STORIES, true);
        assert!(perms.contains(ChatbotsPermissions::MANAGE_STORIES));
        assert_eq!(chatbots_permission_groups().len(), 4);
    }

    #[test]
    fn location_chat_intro_chat_links_limits_and_emoji_status() {
        let mut backend = MockBusinessBackend::new();

        // Chat intro char limits
        assert!(
            backend
                .save_chat_intro(ChatIntro {
                    title: "a".repeat(CHAT_INTRO_TITLE_LIMIT + 1),
                    description: String::new(),
                    sticker_id: None,
                })
                .is_err()
        );
        assert!(
            backend
                .save_chat_intro(ChatIntro {
                    title: "Hi".to_owned(),
                    description: "b".repeat(CHAT_INTRO_DESCRIPTION_LIMIT + 1),
                    sticker_id: None,
                })
                .is_err()
        );

        // Location save & empty address clears coordinates
        backend
            .save_location(BusinessLocation {
                address: "   ".to_owned(),
                point: Some(GeoPoint { lat: 1.0, lon: 2.0 }),
            })
            .unwrap();
        assert!(!backend.load_overview().unwrap().details.location.is_set());
        assert!(
            backend
                .load_overview()
                .unwrap()
                .details
                .location
                .point
                .is_none()
        );

        // Chat links CRUD
        let created = backend
            .create_chat_link("Summer Promo", "Tell me about summer deals")
            .unwrap();
        assert_eq!(created.display_name(), "Summer Promo");
        assert_eq!(created.clicks_label(), "No clicks");
        let edited = backend
            .edit_chat_link(&created.link, "", "Updated text")
            .unwrap();
        assert!(edited.display_name().starts_with("t.me/m/"));
        backend.delete_chat_link(&created.link).unwrap();

        // Exact data_premium_limits.cpp bounds
        let limits = PremiumLimitsData::default();
        assert_eq!(PremiumLimitKind::Folders.bounds(&limits), (10, 30, ""));
        assert_eq!(PremiumLimitKind::PinnedChats.bounds(&limits), (5, 10, ""));
        assert_eq!(PremiumLimitKind::Channels.bounds(&limits), (500, 1000, ""));
        assert_eq!(PremiumLimitKind::SavedGifs.bounds(&limits), (200, 400, ""));
        assert_eq!(PremiumLimitKind::FileSize.bounds(&limits), (2, 4, " GB"));

        // Emoji status set & reset
        backend
            .set_emoji_status(ActiveEmojiStatus {
                status_id: Some(7011),
                duration: EmojiStatusDuration::TwoHours,
            })
            .unwrap();
        let snap = backend.load_overview().unwrap();
        assert_eq!(snap.emoji_status.status_id, Some(7011));
        assert_eq!(snap.emoji_status.duration.seconds(), 7200);

        // Inactive channels & public links cleanup
        assert_eq!(backend.leave_inactive_channels(&[8001, 8002]).unwrap(), 2);
        backend.revoke_public_link(8101).unwrap();
        let snap2 = backend.load_overview().unwrap();
        assert_eq!(snap2.inactive_channels.len(), 1);
        assert_eq!(snap2.public_link_channels.len(), 2);
    }

    // -------------------------------------------------------------------------
    // Software egui PNG renderer for deterministic documentation screenshots
    // -------------------------------------------------------------------------

    struct SoftwareTexture {
        width: usize,
        height: usize,
        pixels: Vec<[u8; 4]>,
    }

    fn render_egui_to_png(
        logical_w: u32,
        logical_h: u32,
        ppp: f32,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> Vec<u8> {
        let ctx = egui::Context::default();
        let palette = Palette::day();
        ctx.set_visuals(palette.to_egui_visuals(ThemeMode::Day));

        let mut textures: HashMap<TextureId, SoftwareTexture> = HashMap::new();
        let mut last_output = None;

        for frame in 0..3 {
            let raw_input = RawInput {
                screen_rect: Some(Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(logical_w as f32, logical_h as f32),
                )),
                time: Some(frame as f64 * 0.016),
                ..Default::default()
            };
            ctx.set_pixels_per_point(ppp);
            let mut output = ctx.run_ui(raw_input, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| draw(ui));
            });

            for (id, deltas) in &output.textures_delta.set {
                for delta in deltas {
                    let egui::ImageData::Color(ref color_img) = delta.image;
                    let [iw, ih] = color_img.size;
                    if let Some([ox, oy]) = delta.pos {
                        if let Some(tex) = textures.get_mut(id) {
                            for y in 0..ih {
                                for x in 0..iw {
                                    let dst_x = ox + x;
                                    let dst_y = oy + y;
                                    if dst_x < tex.width && dst_y < tex.height {
                                        tex.pixels[dst_y * tex.width + dst_x] =
                                            color_img.pixels[y * iw + x].to_array();
                                    }
                                }
                            }
                        }
                    } else {
                        let pixels = color_img.pixels.iter().map(|c| c.to_array()).collect();
                        textures.insert(
                            *id,
                            SoftwareTexture {
                                width: iw,
                                height: ih,
                                pixels,
                            },
                        );
                    }
                }
            }
            for free_id in &output.textures_delta.free {
                textures.remove(free_id);
            }
            output.textures_delta.clear();
            last_output = Some(output);
        }

        let output = last_output.expect("egui produced output");
        let clipped = ctx.tessellate(output.shapes, ppp);

        let phys_w = ((logical_w as f32) * ppp).round() as usize;
        let phys_h = ((logical_h as f32) * ppp).round() as usize;
        let mut fb = vec![[255u8, 255u8, 255u8, 255u8]; phys_w * phys_h];

        for prim in clipped {
            let Primitive::Mesh(mesh) = prim.primitive else {
                continue;
            };
            let tex = textures.get(&mesh.texture_id);
            let clip_min_x = (prim.clip_rect.min.x * ppp).floor().max(0.0) as i32;
            let clip_min_y = (prim.clip_rect.min.y * ppp).floor().max(0.0) as i32;
            let clip_max_x = (prim.clip_rect.max.x * ppp).ceil().min(phys_w as f32) as i32;
            let clip_max_y = (prim.clip_rect.max.y * ppp).ceil().min(phys_h as f32) as i32;

            for tri in mesh.indices.as_chunks::<3>().0 {
                let v0 = mesh.vertices[tri[0] as usize];
                let v1 = mesh.vertices[tri[1] as usize];
                let v2 = mesh.vertices[tri[2] as usize];

                let x0 = v0.pos.x * ppp;
                let y0 = v0.pos.y * ppp;
                let x1 = v1.pos.x * ppp;
                let y1 = v1.pos.y * ppp;
                let x2 = v2.pos.x * ppp;
                let y2 = v2.pos.y * ppp;

                let area = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
                if area.abs() < 1e-6 {
                    continue;
                }
                let inv_area = 1.0 / area;

                let min_x = x0.min(x1).min(x2).floor() as i32;
                let max_x = x0.max(x1).max(x2).ceil() as i32;
                let min_y = y0.min(y1).min(y2).floor() as i32;
                let max_y = y0.max(y1).max(y2).ceil() as i32;

                let bx0 = min_x.max(clip_min_x);
                let bx1 = max_x.min(clip_max_x - 1);
                let by0 = min_y.max(clip_min_y);
                let by1 = max_y.min(clip_max_y - 1);
                if bx0 > bx1 || by0 > by1 {
                    continue;
                }

                let c0 = v0.color.to_array();
                let c1 = v1.color.to_array();
                let c2 = v2.color.to_array();

                for py in by0..=by1 {
                    let cy = py as f32 + 0.5;
                    for px in bx0..=bx1 {
                        let cx = px as f32 + 0.5;
                        let w0 = ((x1 - cx) * (y2 - cy) - (y1 - cy) * (x2 - cx)) * inv_area;
                        let w1 = ((x2 - cx) * (y0 - cy) - (y2 - cy) * (x0 - cx)) * inv_area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                            continue;
                        }

                        let vr =
                            (w0 * f32::from(c0[0]) + w1 * f32::from(c1[0]) + w2 * f32::from(c2[0]))
                                .clamp(0.0, 255.0);
                        let vg =
                            (w0 * f32::from(c0[1]) + w1 * f32::from(c1[1]) + w2 * f32::from(c2[1]))
                                .clamp(0.0, 255.0);
                        let vb =
                            (w0 * f32::from(c0[2]) + w1 * f32::from(c1[2]) + w2 * f32::from(c2[2]))
                                .clamp(0.0, 255.0);
                        let va =
                            (w0 * f32::from(c0[3]) + w1 * f32::from(c1[3]) + w2 * f32::from(c2[3]))
                                .clamp(0.0, 255.0);

                        let (sr, sg, sb, sa) = if let Some(t) = tex {
                            let u = (w0 * v0.uv.x + w1 * v1.uv.x + w2 * v2.uv.x).clamp(0.0, 1.0);
                            let v = (w0 * v0.uv.y + w1 * v1.uv.y + w2 * v2.uv.y).clamp(0.0, 1.0);
                            let [tr, tg, tb, ta] = sample_bilinear(t, u, v);
                            (
                                vr * tr / 255.0,
                                vg * tg / 255.0,
                                vb * tb / 255.0,
                                va * ta / 255.0,
                            )
                        } else {
                            (vr, vg, vb, va)
                        };

                        if sa <= 0.5 {
                            continue;
                        }
                        let dst = &mut fb[(py as usize) * phys_w + (px as usize)];
                        let inv_a = 1.0 - sa / 255.0;
                        dst[0] = (sr + f32::from(dst[0]) * inv_a).round().clamp(0.0, 255.0) as u8;
                        dst[1] = (sg + f32::from(dst[1]) * inv_a).round().clamp(0.0, 255.0) as u8;
                        dst[2] = (sb + f32::from(dst[2]) * inv_a).round().clamp(0.0, 255.0) as u8;
                        dst[3] = 255;
                    }
                }
            }
        }

        let mut raw = Vec::with_capacity(phys_w * phys_h * 4);
        for px in &fb {
            raw.extend_from_slice(px);
        }
        let img = image::RgbaImage::from_raw(phys_w as u32, phys_h as u32, raw)
            .expect("valid RGBA image");
        let mut png_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        img.write_to(&mut cursor, image::ImageFormat::Png)
            .expect("encode PNG");
        png_bytes
    }

    fn sample_bilinear(tex: &SoftwareTexture, u: f32, v: f32) -> [f32; 4] {
        if tex.width == 0 || tex.height == 0 {
            return [255.0, 255.0, 255.0, 255.0];
        }
        let fx = (u * (tex.width as f32) - 0.5).clamp(0.0, (tex.width - 1) as f32);
        let fy = (v * (tex.height as f32) - 0.5).clamp(0.0, (tex.height - 1) as f32);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(tex.width - 1);
        let y1 = (y0 + 1).min(tex.height - 1);
        let tx = fx - (x0 as f32);
        let ty = fy - (y0 as f32);

        let p00 = tex.pixels[y0 * tex.width + x0];
        let p10 = tex.pixels[y0 * tex.width + x1];
        let p01 = tex.pixels[y1 * tex.width + x0];
        let p11 = tex.pixels[y1 * tex.width + x1];

        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let top = f32::from(p00[i]) * (1.0 - tx) + f32::from(p10[i]) * tx;
            let bot = f32::from(p01[i]) * (1.0 - tx) + f32::from(p11[i]) * tx;
            out[i] = top * (1.0 - ty) + bot * ty;
        }
        out
    }

    #[test]
    fn renders_business_hub_hours_quick_replies_and_premium_screenshots() {
        let write_files = std::env::var("RUSTGRAM_UPDATE_SCREENSHOTS").as_deref() == Ok("1")
            || std::env::var("TGFORK_UPDATE_SCREENSHOTS").as_deref() == Ok("1");
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        if write_files {
            let _ = std::fs::create_dir_all(&docs_dir);
        }
        let palette = Palette::day();

        // 1. Telegram Business Hub (`crates/tg-app/docs/business_hub.png`)
        let mut host = BusinessScreenHost::with_mock();
        host.select_screen(BusinessScreen::Hub);
        let hub_png = render_egui_to_png(580, 960, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(hub_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("business_hub.png"), &hub_png)
                .expect("write business_hub.png");
        }

        // 2. Working Hours Editor (`crates/tg-app/docs/business_working_hours.png`)
        host.select_screen(BusinessScreen::WorkingHours);
        host.working_hours_view_mut().open_day_editor(0); // Show Monday interval editor open
        let hours_png = render_egui_to_png(580, 920, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(hours_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("business_working_hours.png"), &hours_png)
                .expect("write business_working_hours.png");
        }

        // 3. Quick Replies + "/" Compose Preview (`crates/tg-app/docs/business_quick_replies.png`)
        host.select_screen(BusinessScreen::QuickReplies);
        let replies_png = render_egui_to_png(580, 920, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(replies_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("business_quick_replies.png"), &replies_png)
                .expect("write business_quick_replies.png");
        }

        // 4. Telegram Premium Settings (`crates/tg-app/docs/business_premium.png`)
        host.select_screen(BusinessScreen::Premium);
        let premium_png = render_egui_to_png(580, 980, 1.5, |ui| {
            host.show_at(ui, &palette, false, 1_000);
        });
        assert!(premium_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("business_premium.png"), &premium_png)
                .expect("write business_premium.png");
        }
    }
}
