//! Telegram Bot UI widgets, modal flows, and standalone interactive demo screen built on
//! top of `tg-bots`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/history_item_components.cpp` & `history_view_element.cpp`
//!   ([`inline_keyboard`])
//! - `Telegram/SourceFiles/chat_helpers/bot_keyboard.cpp` & `history/history_widget.cpp`
//!   ([`reply_keyboard`])
//! - `Telegram/SourceFiles/inline_bots/inline_results_widget.cpp`, `inline_results_inner.cpp`,
//!   & `inline_bot_layout_internal.cpp` ([`inline_results`])
//! - `Telegram/SourceFiles/history/view/controls/history_view_bot_menu_button.cpp`,
//!   `chat_helpers/field_autocomplete.cpp`, `history/view/history_view_empty_list_bubble.cpp`,
//!   & `inline_bots/bot_attach_web_view.cpp` ([`menus`])
//! - `Telegram/SourceFiles/boxes/url_auth_box.cpp`, `api/api_bot.cpp`, &
//!   `window/window_peer_menu.cpp` ([`boxes`])
//! - Decoupled [`backend::BotsBackend`] trait with offline demo fixture in [`mock`]

pub use tg_app_core::bots::backend;
pub mod boxes;
pub mod inline_keyboard;
pub mod inline_results;
pub mod menus;
pub mod mock;
pub mod reply_keyboard;
pub mod ui_helpers;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Id, Key, Modifiers, Order, Pos2,
    Rect, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2,
};
use tg_bots::callback::{
    AcceptUrlAuthDecision, BotButtonAction, CallbackAnswerAction, CallbackAnswerCache,
    CallbackCacheKey, CallbackQueryRequest, SwitchInlineDecision, UrlAuthAcceptOutcome,
    UrlAuthDecision, UrlAuthSource, build_accept_url_auth, handle_callback_answer,
    handle_url_auth_accept_result, handle_url_auth_request_error, handle_url_auth_request_result,
};
use tg_bots::commands::{BotMenuButton, PrivacyPolicyAction};
use tg_bots::inline::SwitchPmButton;
use tg_bots::mini_app::{AttachMenuStore, AttachMenuToggleState};
use tg_bots::stand_in::FullMsgId;

use crate::bots::backend::{
    BotChatMessage, BotChatSnapshot, BotMessageAttachment, BotPeerCandidate, BotsBackend,
};
use crate::bots::boxes::{BotModalBox, BotModalOutcome, paint_bot_toast, show_bot_modal_box};
use crate::bots::inline_keyboard::{
    INLINE_KEYBOARD_TOP_SKIP, InlineKeyboardActivation, InlineKeyboardParams, InlineKeyboardState,
    show_inline_keyboard,
};
use crate::bots::inline_results::{InlineResultsEvent, InlineResultsPanel};
use crate::bots::menus::{
    AttachMenuPopupEvent, BotCommandsPopupEvent, BotCommandsPopupParams, BotInfoHeaderEvent,
    BotMenuButtonAction, paint_bot_menu_button, show_attach_menu_bots_popup,
    show_bot_commands_popup, show_bot_info_header,
};
use crate::bots::mock::{DEMO_GAME_MSG_ID, DEMO_INLINE_KB_MSG_ID, MockBotsBackend};
use crate::bots::reply_keyboard::{
    ReplyKeyboardActivation, ReplyKeyboardWidget, paint_reply_keyboard_toggle_button,
};
use crate::bots::ui_helpers::{box_action_button, paint_procedural_art, pill_button, with_alpha};
use tg_app_core::core_types::platform_call::{PLATFORM_CALL_REPAINT_INTERVAL, PlatformCallState};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

/// Active view preset inside [`BotsDemoScreen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BotsDemoTab {
    /// Interactive bot chat canvas with inline keyboards, game card, and reply keyboard.
    #[default]
    BotChat,
    /// Empty bot chat showing the "What can this bot do?" description header & Start card.
    EmptyBotIntro,
    /// Inline bot query pre-filled with gallery mosaic results (`@catalog_demo_bot photos`).
    InlineGallery,
    /// Inline bot query pre-filled with list results (`@catalog_demo_bot catalog`).
    InlineList,
}

/// Scheduled simulated callback completion so the loading spinner is visibly observable.
#[derive(Debug, Clone)]
struct PendingCallback {
    msg_id: FullMsgId,
    row: usize,
    col: usize,
    request: CallbackQueryRequest,
    deliver_at_ms: u64,
}

/// Standalone interactive chat-like demo screen wiring every `tg-app::bots` widget to
/// a [`BotsBackend`].
pub struct BotsDemoScreen {
    backend: Box<dyn BotsBackend>,
    started_at: Instant,
    chat: BotChatSnapshot,
    attach_menu: AttachMenuStore,
    candidates: Vec<BotPeerCandidate>,
    active_tab: BotsDemoTab,
    /// Inline keyboard loading/pressed state.
    pub inline_kb_state: InlineKeyboardState,
    /// Reply keyboard panel state.
    pub reply_keyboard: ReplyKeyboardWidget,
    /// Inline bot results panel state.
    pub inline_results: InlineResultsPanel,
    /// Callback query answer cache (`cache_time` seconds).
    pub callback_cache: CallbackAnswerCache,
    /// Pending simulated async callback RPCs.
    pending_callbacks: Vec<PendingCallback>,
    /// Compose input buffer.
    pub compose_text: String,
    /// Whether the bot slash-commands popup is open above the compose bar.
    pub commands_popup_open: bool,
    /// Whether the attach menu bots popup is open above the compose bar.
    pub attach_popup_open: bool,
    /// Active modal dialog box, if any.
    pub active_modal: Option<BotModalBox>,
    /// Transient toast notification `(message, expires_at_ms)`.
    pub toast: Option<(String, u64)>,
    /// Last copied text (observable in tests & UI feedback).
    pub last_copied_text: Option<String>,
    /// Last opened URL (observable in tests & UI feedback).
    pub last_opened_url: Option<String>,
    /// Injectable OS geolocation provider for `RequestLocation` reply buttons.
    geo_provider: Arc<dyn tg_platform::geolocation::GeoLocationProvider>,
    /// In-flight worker-thread geolocation lookup started via [`BotModalBox::spawn_resolve_location_with`].
    location_pending: Option<boxes::PendingBotLocationCall>,
}

impl BotsDemoScreen {
    /// Creates a new [`BotsDemoScreen`] backed by `backend`.
    #[must_use]
    pub fn new(backend: Box<dyn BotsBackend>) -> Self {
        let chat = backend.load_bot_chat();
        let attach_menu = backend.load_attach_menu();
        let candidates = backend.candidate_peers();
        let mut reply_keyboard = ReplyKeyboardWidget::new();
        if let Some((msg_id, ref markup)) = chat.active_reply_keyboard {
            reply_keyboard.update_markup(Some(msg_id), Some(markup), true);
        }

        Self {
            backend,
            started_at: Instant::now(),
            chat,
            attach_menu,
            candidates,
            active_tab: BotsDemoTab::BotChat,
            inline_kb_state: InlineKeyboardState::new(),
            reply_keyboard,
            inline_results: InlineResultsPanel::new(),
            callback_cache: CallbackAnswerCache::new(),
            pending_callbacks: Vec::new(),
            compose_text: String::new(),
            commands_popup_open: false,
            attach_popup_open: false,
            active_modal: None,
            toast: None,
            last_copied_text: None,
            last_opened_url: None,
            geo_provider: tg_app_core::core_types::geo::default_geolocation_provider(),
            location_pending: None,
        }
    }

    /// Overrides the OS geolocation provider used when the user accepts a bot's `RequestLocation`.
    pub fn set_geo_provider(
        &mut self,
        provider: Arc<dyn tg_platform::geolocation::GeoLocationProvider>,
    ) {
        self.geo_provider = provider;
    }

    /// Returns `true` while a bot `RequestLocation` geolocation lookup is running on a worker thread.
    #[must_use]
    pub const fn is_location_pending(&self) -> bool {
        self.location_pending.is_some()
    }

    /// Starts resolving the user's current location on a worker thread via
    /// [`BotModalBox::spawn_resolve_location_with`] (backed by [`BotModalBox::resolve_location_with`]),
    /// so the UI frame loop never blocks on OS geolocation.
    pub fn start_location_share(&mut self, repaint: Option<Context>, now_ms: u64) -> bool {
        if self.location_pending.is_some() {
            return false;
        }
        let call = BotModalBox::spawn_resolve_location_with(
            self.chat.bot_name.clone(),
            Arc::clone(&self.geo_provider),
            repaint,
        );
        self.location_pending = Some(call);
        self.show_toast("Getting your location\u{2026}", now_ms);
        true
    }

    /// Polls an in-flight worker-thread bot location lookup and sends the location message once ready.
    pub fn poll_location_share(&mut self, now_ms: u64) -> Option<bool> {
        let outcome = match self.location_pending.as_ref()?.poll() {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => (
                Err("the location service stopped responding".to_owned()),
                None,
            ),
        };
        self.location_pending = None;
        Some(self.finish_location_share(outcome, now_ms))
    }

    /// Blocks up to `timeout` for an in-flight bot location lookup (tests only, never a UI frame).
    #[doc(hidden)]
    pub fn wait_location_share(
        &mut self,
        timeout: std::time::Duration,
        now_ms: u64,
    ) -> Option<bool> {
        let outcome = match self.location_pending.as_ref()?.wait(timeout) {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => (
                Err("the location service stopped responding".to_owned()),
                None,
            ),
        };
        self.location_pending = None;
        Some(self.finish_location_share(outcome, now_ms))
    }

    fn finish_location_share(
        &mut self,
        outcome: (
            Result<Option<tg_platform::geolocation::GeoLocation>, String>,
            Option<String>,
        ),
        now_ms: u64,
    ) -> bool {
        match outcome.0 {
            Ok(Some(loc)) if loc.point.is_valid() => {
                let summary = format!(
                    "Location: {:.5}, {:.5}",
                    loc.point.latitude, loc.point.longitude
                );
                if let Ok(msg) = self
                    .backend
                    .send_bot_command(self.chat.peer_id, &summary, None)
                {
                    self.chat.messages.push(msg);
                }
                self.show_toast(format!("Shared location ({summary})"), now_ms);
                true
            }
            Ok(_) => {
                let msg = outcome.1.unwrap_or_else(|| {
                    "Windows Location Services are disabled or unavailable.".to_owned()
                });
                self.show_toast(msg, now_ms);
                false
            }
            Err(err) => {
                self.show_toast(err, now_ms);
                false
            }
        }
    }

    /// Creates a demo screen pre-wired to [`MockBotsBackend`].
    #[must_use]
    pub fn with_mock() -> Self {
        Self::new(Box::new(MockBotsBackend::new()))
    }

    /// Returns the active demo tab.
    #[must_use]
    pub const fn active_tab(&self) -> BotsDemoTab {
        self.active_tab
    }

    /// Returns a read-only view of the loaded chat snapshot.
    #[must_use]
    pub const fn chat(&self) -> &BotChatSnapshot {
        &self.chat
    }

    /// Returns a read-only view of the attach menu store.
    #[must_use]
    pub const fn attach_menu(&self) -> &AttachMenuStore {
        &self.attach_menu
    }

    /// Displays a transient toast notification for 3 seconds starting at `now_ms`.
    pub fn show_toast(&mut self, msg: impl Into<String>, now_ms: u64) {
        let Some(msg) = tg_app_core::user_error::visible_toast_text(msg.into()) else {
            return;
        };
        self.toast = Some((msg, now_ms.saturating_add(3_000)));
    }

    /// Switches the demo tab and prepares its corresponding widgets deterministically.
    pub fn select_tab(&mut self, tab: BotsDemoTab, now_ms: u64) {
        self.active_tab = tab;
        self.commands_popup_open = false;
        self.attach_popup_open = false;
        self.inline_results.gallery_override = None;

        match tab {
            BotsDemoTab::BotChat => {
                self.compose_text.clear();
                self.inline_results.visible = false;
                if let Some((msg_id, ref markup)) = self.chat.active_reply_keyboard {
                    self.reply_keyboard
                        .update_markup(Some(msg_id), Some(markup), true);
                }
            }
            BotsDemoTab::EmptyBotIntro => {
                self.compose_text.clear();
                self.inline_results.visible = false;
                self.reply_keyboard.visible = false;
            }
            BotsDemoTab::InlineGallery => {
                self.reply_keyboard.visible = false;
                self.compose_text = format!("@{} photos", self.chat.bot_username);
                let _ = self.inline_results.fetch_immediately(
                    self.chat.bot_id,
                    self.chat.bot_username.clone(),
                    self.chat.peer_id,
                    "photos",
                    now_ms,
                    self.backend.as_mut(),
                );
            }
            BotsDemoTab::InlineList => {
                self.reply_keyboard.visible = false;
                self.compose_text = format!("@{} catalog", self.chat.bot_username);
                let _ = self.inline_results.fetch_immediately(
                    self.chat.bot_id,
                    self.chat.bot_username.clone(),
                    self.chat.peer_id,
                    "catalog",
                    now_ms,
                    self.backend.as_mut(),
                );
            }
        }
    }

    /// Opens the URL Auth confirmation box using the demo bot's login button.
    pub fn open_demo_url_auth_box(&mut self, now_ms: u64) {
        let source = UrlAuthSource::Button {
            peer_id: self.chat.peer_id,
            msg_id: DEMO_INLINE_KB_MSG_ID,
            button_id: 901,
            url: "https://portal.example.org/oauth/callback".to_owned(),
        };
        self.begin_url_auth(source, now_ms);
    }

    /// Opens the Share Game Score box for the demo game message.
    pub fn open_demo_share_game_box(&mut self) {
        for msg in &self.chat.messages {
            if let BotMessageAttachment::Game { game, high_scores } = &msg.attachment {
                let backend = self.backend.as_ref();
                self.active_modal = Some(BotModalBox::share_game_score(
                    msg.id,
                    self.chat.bot_username.clone(),
                    game.clone(),
                    high_scores,
                    |uid| backend.resolve_user_name(uid),
                ));
                return;
            }
        }
    }

    /// Toggles the bot's compose menu button between `Commands` and `WebApp`.
    pub fn toggle_menu_button_kind(&mut self, now_ms: u64) {
        let next = if self.chat.bot_info.menu_button.is_web_app() {
            BotMenuButton::Commands
        } else {
            BotMenuButton::WebApp {
                text: "Open Shop".to_owned(),
                url: "https://shop.example.org/miniapp".to_owned(),
            }
        };
        self.backend.set_menu_button(next.clone());
        self.chat.bot_info.apply_menu_button(next.clone());
        let label = if next.is_web_app() {
            "Menu button switched to Web App"
        } else {
            "Menu button switched to Commands"
        };
        self.show_toast(label, now_ms);
    }

    /// Processes timers (inline debounce & simulated callback completion) at `now_ms`.
    pub fn tick(&mut self, now_ms: u64) {
        let _ = self.poll_location_share(now_ms);
        let _ = self
            .inline_results
            .poll_debounce(now_ms, self.backend.as_mut());

        if let Some((_, expires)) = self.toast
            && now_ms >= expires
        {
            self.toast = None;
        }

        let mut remaining = Vec::new();
        let mut ready = Vec::new();
        for pending in self.pending_callbacks.drain(..) {
            if now_ms >= pending.deliver_at_ms {
                ready.push(pending);
            } else {
                remaining.push(pending);
            }
        }
        self.pending_callbacks = remaining;

        for item in ready {
            self.inline_kb_state
                .clear_loading(item.msg_id, item.row, item.col);
            self.execute_callback_now(&item.request, now_ms);
        }
    }

    /// Renders the demo screen using wall-clock elapsed time.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette, show_close: bool) -> bool {
        let now_ms = self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.show_at(ui, palette, show_close, now_ms)
    }

    /// Renders the demo screen deterministically at `now_ms` (used by unit tests & screenshots).
    pub fn show_at(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        show_close: bool,
        now_ms: u64,
    ) -> bool {
        self.tick(now_ms);
        let time_secs = now_ms as f64 / 1_000.0;
        let mut close_clicked = false;
        let full_rect = ui.max_rect();

        ui.painter()
            .rect_filled(full_rect, CornerRadius::ZERO, palette.window_bg);

        // 1. Top Bar (Bot Identity + Close)
        let top_bar_h = 52.0;
        let (top_rect, _) =
            ui.allocate_exact_size(Vec2::new(full_rect.width(), top_bar_h), Sense::hover());
        ui.painter()
            .rect_filled(top_rect, CornerRadius::ZERO, palette.window_bg);
        ui.painter().line_segment(
            [top_rect.left_bottom(), top_rect.right_bottom()],
            Stroke::new(1.0, palette.divider),
        );

        let av_c = Pos2::new(top_rect.left() + 26.0, top_rect.center().y);
        ui.painter()
            .circle_filled(av_c, 18.0, palette.window_bg_active);
        ui.painter().text(
            av_c,
            Align2::CENTER_CENTER,
            "CA",
            FontId::proportional(13.0),
            palette.window_fg_active,
        );
        ui.painter().text(
            Pos2::new(top_rect.left() + 54.0, top_rect.top() + 17.0),
            Align2::LEFT_CENTER,
            &self.chat.bot_name,
            FontId::proportional(14.5),
            palette.window_bold_fg,
        );
        ui.painter().text(
            Pos2::new(top_rect.left() + 54.0, top_rect.top() + 35.0),
            Align2::LEFT_CENTER,
            format!("@{} • bot • Ctrl+Shift+K Demo", self.chat.bot_username),
            FontId::proportional(11.5),
            palette.window_sub_text_fg,
        );

        // Quick right-side actions in top bar
        let right_area = Rect::from_min_max(
            Pos2::new(top_rect.right() - 260.0, top_rect.top()),
            top_rect.max,
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(right_area), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                if show_close
                    && pill_button(
                        ui,
                        "Close",
                        palette.window_bg_over,
                        palette.window_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                {
                    close_clicked = true;
                }
                if pill_button(
                    ui,
                    if self.chat.bot_info.menu_button.is_web_app() {
                        "Menu: WebApp"
                    } else {
                        "Menu: Cmds"
                    },
                    with_alpha(palette.window_bg_active, 0.14),
                    palette.window_active_text_fg,
                    None,
                )
                .clicked()
                {
                    self.toggle_menu_button_kind(now_ms);
                }
            });
        });

        // 2. Scenario Tabs Strip
        let tab_bar_h = 40.0;
        let (tab_rect, _) =
            ui.allocate_exact_size(Vec2::new(full_rect.width(), tab_bar_h), Sense::hover());
        ui.painter()
            .rect_filled(tab_rect, CornerRadius::ZERO, palette.window_bg);
        ui.painter().line_segment(
            [tab_rect.left_bottom(), tab_rect.right_bottom()],
            Stroke::new(1.0, palette.divider),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(tab_rect.shrink2(Vec2::new(10.0, 5.0))),
            |ui| {
                ui.horizontal(|ui| {
                    for &(tab, label) in &[
                        (BotsDemoTab::BotChat, "Bot Chat"),
                        (BotsDemoTab::EmptyBotIntro, "Empty Bot Intro"),
                        (BotsDemoTab::InlineGallery, "Inline Mosaic"),
                        (BotsDemoTab::InlineList, "Inline List"),
                    ] {
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
                            self.select_tab(tab, now_ms);
                        }
                    }
                    if pill_button(
                        ui,
                        "URL Auth Box",
                        palette.window_bg_over,
                        palette.window_active_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.open_demo_url_auth_box(now_ms);
                    }
                    if pill_button(
                        ui,
                        "Share Game",
                        palette.window_bg_over,
                        palette.window_active_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.open_demo_share_game_box();
                    }
                });
            },
        );

        // Compute bottom compose + reply keyboard height so chat canvas occupies the rest cleanly.
        let compose_h = 52.0;
        let reply_kb_h = if self.reply_keyboard.is_shown() {
            154.0
        } else {
            0.0
        };
        let chat_h = (full_rect.bottom() - ui.cursor().top() - compose_h - reply_kb_h).max(180.0);

        // 3. Chat Canvas
        let (chat_rect, _) =
            ui.allocate_exact_size(Vec2::new(full_rect.width(), chat_h), Sense::hover());
        ui.painter()
            .rect_filled(chat_rect, CornerRadius::ZERO, palette.chat_canvas_bg);

        let mut inline_activation: Option<InlineKeyboardActivation> = None;
        let mut info_header_event: Option<BotInfoHeaderEvent> = None;
        let mut open_share_game = false;

        ui.scope_builder(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_bots_demo_chat_scroll", self.active_tab as u8))
                .max_height(chat_rect.height())
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(12.0);
                    if self.active_tab == BotsDemoTab::EmptyBotIntro {
                        ui.add_space(16.0);
                        info_header_event = show_bot_info_header(
                            ui,
                            &self.chat.bot_name,
                            &self.chat.bot_username,
                            &self.chat.bot_info,
                            palette,
                        );
                    } else {
                        let bubble_max_w = (chat_rect.width() - 48.0).clamp(260.0, 440.0);
                        let messages = self.chat.messages.clone();
                        for msg in &messages {
                            if let Some(act) = self.paint_chat_message(
                                ui,
                                msg,
                                bubble_max_w,
                                time_secs,
                                palette,
                                &mut open_share_game,
                            ) {
                                inline_activation = Some(act);
                            }
                            ui.add_space(10.0);
                        }
                    }
                    ui.add_space(12.0);
                });
        });

        // Floating overlays anchored just above the compose bar inside `chat_rect`
        let mut inline_res_event = None;
        let mut cmd_event = None;
        let mut attach_event = None;

        if self.inline_results.should_render() {
            let panel_w = (chat_rect.width() - 24.0).clamp(320.0, 560.0);
            let max_h = (chat_rect.height() - 16.0).clamp(180.0, 390.0);
            let panel_rect = Rect::from_min_max(
                Pos2::new(chat_rect.left() + 12.0, chat_rect.bottom() - max_h - 6.0),
                Pos2::new(chat_rect.left() + 12.0 + panel_w, chat_rect.bottom() - 6.0),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(panel_rect));
            inline_res_event = self.inline_results.show(
                &mut child,
                panel_w,
                max_h,
                palette,
                self.backend.as_mut(),
            );
        } else if self.commands_popup_open {
            let pop_w = (chat_rect.width() - 32.0).clamp(280.0, 400.0);
            let pop_h = 276.0_f32.min(chat_rect.height() - 16.0);
            let pop_rect = Rect::from_min_max(
                Pos2::new(chat_rect.left() + 12.0, chat_rect.bottom() - pop_h - 6.0),
                Pos2::new(chat_rect.left() + 12.0 + pop_w, chat_rect.bottom() - 6.0),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pop_rect));
            let cmds = self.backend.commands_for_chat();
            cmd_event = show_bot_commands_popup(
                &mut child,
                BotCommandsPopupParams {
                    commands: &cmds,
                    filter_prefix: &self.compose_text,
                    peer_kind: self.chat.peer_kind,
                    peer_bot_status: self.chat.peer_bot_status,
                    bot_username: &self.chat.bot_username,
                    max_width: pop_w,
                },
                palette,
            );
        } else if self.attach_popup_open {
            let pop_w = 284.0;
            let pop_h = 300.0_f32.min(chat_rect.height() - 12.0);
            let pop_rect = Rect::from_min_max(
                Pos2::new(
                    chat_rect.right() - pop_w - 12.0,
                    chat_rect.bottom() - pop_h - 6.0,
                ),
                Pos2::new(chat_rect.right() - 12.0, chat_rect.bottom() - 6.0),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pop_rect));
            attach_event = show_attach_menu_bots_popup(
                &mut child,
                &self.attach_menu,
                self.chat.peer_id,
                self.chat.peer_kind,
                palette,
            );
        }

        // 4. Compose Bar (BotMenuButton + Input w/ Reply Keyboard Placeholder + KB Toggle + Attach + Send)
        let (compose_rect, _) =
            ui.allocate_exact_size(Vec2::new(full_rect.width(), compose_h), Sense::hover());
        ui.painter()
            .rect_filled(compose_rect, CornerRadius::ZERO, palette.window_bg);
        ui.painter().line_segment(
            [compose_rect.left_top(), compose_rect.right_top()],
            Stroke::new(1.0, palette.divider),
        );

        let mut menu_btn_action = None;
        let mut send_clicked = false;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(compose_rect.shrink2(Vec2::new(10.0, 9.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    menu_btn_action = paint_bot_menu_button(
                        ui,
                        &self.chat.bot_info.menu_button,
                        !self.chat.bot_info.commands.is_empty(),
                        self.commands_popup_open,
                        false,
                        palette,
                    );

                    ui.add_space(4.0);
                    let placeholder = self
                        .reply_keyboard
                        .placeholder()
                        .unwrap_or("Write a message or @catalog_demo_bot query…")
                        .to_owned();
                    let input_w = (ui.available_width() - 156.0).max(120.0);
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.compose_text)
                            .hint_text(placeholder)
                            .desired_width(input_w),
                    );
                    if resp.changed() {
                        let text = self.compose_text.clone();
                        let matched = self.inline_results.sync_from_compose_text(
                            &text,
                            self.chat.bot_id,
                            &self.chat.bot_username,
                            self.chat.peer_id,
                            now_ms,
                        );
                        if matched {
                            self.commands_popup_open = false;
                            self.attach_popup_open = false;
                        } else if text.starts_with('/') {
                            self.commands_popup_open = true;
                        }
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        send_clicked = true;
                    }

                    // Reply keyboard toggle button
                    if self.reply_keyboard.has_markup()
                        && paint_reply_keyboard_toggle_button(
                            ui,
                            palette,
                            self.reply_keyboard.is_shown(),
                            true,
                        )
                        .clicked()
                    {
                        self.reply_keyboard.toggle_visible();
                    }

                    // Attach menu bots button
                    if pill_button(
                        ui,
                        "Attach",
                        if self.attach_popup_open {
                            palette.window_bg_active
                        } else {
                            palette.window_bg_over
                        },
                        if self.attach_popup_open {
                            palette.window_fg_active
                        } else {
                            palette.window_fg
                        },
                        None,
                    )
                    .clicked()
                    {
                        self.attach_popup_open = !self.attach_popup_open;
                        self.commands_popup_open = false;
                        self.inline_results.visible = false;
                    }

                    // Send button
                    if box_action_button(
                        ui,
                        "Send",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        56.0,
                        32.0,
                    )
                    .clicked()
                    {
                        send_clicked = true;
                    }
                });
            },
        );

        // 5. Reply Keyboard Drawer below compose bar
        let reply_activation = self
            .reply_keyboard
            .show(ui, Some(self.chat.bot_id), None, palette);

        // Dispatch widget events
        if let Some(act) = inline_activation {
            self.handle_inline_activation(act, now_ms);
        }
        if let Some(act) = reply_activation {
            self.handle_reply_activation(act, now_ms);
        }
        if open_share_game {
            self.open_demo_share_game_box();
        }
        if let Some(ev) = info_header_event {
            match ev {
                BotInfoHeaderEvent::StartBot => {
                    if let Ok(msg) =
                        self.backend
                            .send_bot_command(self.chat.peer_id, "/start", None)
                    {
                        self.chat.messages.push(msg);
                    }
                    self.select_tab(BotsDemoTab::BotChat, now_ms);
                    self.show_toast("Sent /start to Catalog Assistant", now_ms);
                }
                BotInfoHeaderEvent::OpenPrivacyPolicy(action) => {
                    let msg = match action {
                        PrivacyPolicyAction::OpenUrl(url) => {
                            self.last_opened_url = Some(url.clone());
                            format!("Opening privacy policy: {url}")
                        }
                        PrivacyPolicyAction::SendPrivacyCommand(cmd) => {
                            format!("Sending {cmd} command")
                        }
                        PrivacyPolicyAction::OpenDefaultTelegramPolicy => {
                            "Opening default Telegram Bot Privacy Policy".to_owned()
                        }
                    };
                    self.show_toast(msg, now_ms);
                }
                BotInfoHeaderEvent::OpenMainApp => {
                    self.show_toast("Launching Catalog Assistant Main Mini App…", now_ms);
                }
            }
        }
        if let Some(ev) = menu_btn_action {
            match ev {
                BotMenuButtonAction::ToggleCommandsPopup => {
                    self.commands_popup_open = !self.commands_popup_open;
                    self.attach_popup_open = false;
                    self.inline_results.visible = false;
                }
                BotMenuButtonAction::OpenWebApp { text, url } => {
                    self.last_opened_url = Some(url.clone());
                    self.show_toast(
                        format!("Opening Bot Menu Web App \"{text}\" ({url})"),
                        now_ms,
                    );
                }
            }
        }
        if let Some(ev) = cmd_event {
            match ev {
                BotCommandsPopupEvent::ExecuteCommand(cmd) => {
                    self.commands_popup_open = false;
                    self.compose_text.clear();
                    if let Ok(msg) = self.backend.send_bot_command(self.chat.peer_id, &cmd, None) {
                        self.chat.messages.push(msg);
                    }
                    self.show_toast(format!("Executed command {cmd}"), now_ms);
                }
                BotCommandsPopupEvent::Dismissed => {
                    self.commands_popup_open = false;
                }
            }
        }
        if let Some(ev) = attach_event {
            match ev {
                AttachMenuPopupEvent::StandardAttach(kind) => {
                    self.attach_popup_open = false;
                    self.show_toast(format!("Attach: {kind}"), now_ms);
                }
                AttachMenuPopupEvent::LaunchBot(bot) => {
                    self.attach_popup_open = false;
                    if bot.disclaimer_required
                        && !self.attach_menu.is_disclaimer_accepted(bot.bot_id)
                    {
                        let allow_write = bot.request_write_access;
                        self.active_modal =
                            Some(BotModalBox::AttachBotDisclaimer { bot, allow_write });
                    } else {
                        self.show_toast(
                            format!("Launched Attach Menu Mini App: {}", bot.short_name),
                            now_ms,
                        );
                    }
                }
                AttachMenuPopupEvent::AddBot(bot_id) => {
                    if let Ok(store) = self
                        .backend
                        .toggle_attach_menu_bot(bot_id, AttachMenuToggleState::Added)
                    {
                        self.attach_menu = store;
                        self.show_toast("Added bot to Attach Menu", now_ms);
                    }
                }
                AttachMenuPopupEvent::RemoveBot(bot_id) => {
                    if let Ok(store) = self
                        .backend
                        .toggle_attach_menu_bot(bot_id, AttachMenuToggleState::Removed)
                    {
                        self.attach_menu = store;
                        self.show_toast("Removed bot from Attach Menu", now_ms);
                    }
                }
                AttachMenuPopupEvent::Dismissed => {
                    self.attach_popup_open = false;
                }
            }
        }
        if let Some(ev) = inline_res_event {
            match ev {
                InlineResultsEvent::SendResult { bot_id, result } => {
                    if let Ok(msg) =
                        self.backend
                            .send_inline_result(bot_id, self.chat.peer_id, &result)
                    {
                        self.chat.messages.push(msg);
                    }
                    self.compose_text.clear();
                    self.inline_results.visible = false;
                    self.active_tab = BotsDemoTab::BotChat;
                    self.show_toast(format!("Sent inline result: {}", result.title), now_ms);
                }
                InlineResultsEvent::SwitchPm { button, .. } => {
                    self.inline_results.visible = false;
                    let note = match button {
                        SwitchPmButton::StartPm {
                            text, start_param, ..
                        } => format!("Switching to Bot PM: {text} (/start {start_param})"),
                        SwitchPmButton::WebView { text, url } => {
                            self.last_opened_url = Some(url.clone());
                            format!("Opening switch_webview: {text} ({url})")
                        }
                    };
                    self.show_toast(note, now_ms);
                }
                InlineResultsEvent::Dismissed => {
                    self.inline_results.visible = false;
                }
            }
        }
        if send_clicked && !self.compose_text.trim().is_empty() {
            let txt = self.compose_text.trim().to_owned();
            if let Ok(msg) = self.backend.send_bot_command(self.chat.peer_id, &txt, None) {
                self.chat.messages.push(msg);
            }
            self.compose_text.clear();
            self.commands_popup_open = false;
            self.inline_results.visible = false;
        }

        // 6. Active Modal Box overlay (URL Confirm, URL Auth, Callback Alert, Request*, Share Game)
        if let Some(mut modal) = self.active_modal.take() {
            if let Some(outcome) =
                show_bot_modal_box(ui, full_rect, &mut modal, &self.candidates, palette)
            {
                self.handle_modal_outcome(outcome, Some(ui.ctx().clone()), now_ms);
            } else {
                self.active_modal = Some(modal);
            }
        }

        if self.location_pending.is_some() {
            ui.ctx()
                .request_repaint_after(PLATFORM_CALL_REPAINT_INTERVAL);
        }

        // 7. Transient Toast Banner
        if let Some((ref msg, _)) = self.toast {
            paint_bot_toast(ui.painter(), full_rect, msg);
        }

        close_clicked
    }

    fn paint_chat_message(
        &mut self,
        ui: &mut Ui,
        msg: &BotChatMessage,
        max_w: f32,
        time_secs: f64,
        palette: &Palette,
        open_share_game: &mut bool,
    ) -> Option<InlineKeyboardActivation> {
        let mut activation = None;
        let bubble_bg = if msg.outgoing {
            palette.msg_out_bg
        } else {
            palette.msg_in_bg
        };

        let left_pad = if msg.outgoing {
            (ui.available_width() - max_w - 16.0).max(16.0)
        } else {
            16.0
        };

        ui.horizontal(|ui| {
            ui.add_space(left_pad);
            ui.vertical(|ui| {
                ui.set_max_width(max_w);

                let frame = egui::Frame::new()
                    .fill(bubble_bg)
                    .corner_radius(CornerRadius::same(12))
                    .stroke(Stroke::new(1.0, with_alpha(palette.divider, 0.7)))
                    .inner_margin(egui::Margin::symmetric(12, 9));

                let resp = frame.show(ui, |ui| {
                    ui.set_width(max_w - 24.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&msg.author)
                                .size(12.5)
                                .strong()
                                .color(palette.window_active_text_fg),
                        );
                        if let Some(ref via) = msg.via_bot {
                            ui.label(
                                egui::RichText::new(format!("via {via}"))
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(&msg.time_label)
                                    .size(10.5)
                                    .color(palette.msg_date_fg),
                            );
                        });
                    });

                    // Optional game or inline result card inside the bubble
                    match &msg.attachment {
                        BotMessageAttachment::None => {}
                        BotMessageAttachment::Game { game, high_scores } => {
                            ui.add_space(6.0);
                            let card_w = ui.available_width();
                            let (art_rect, _) =
                                ui.allocate_exact_size(Vec2::new(card_w, 86.0), Sense::hover());
                            paint_procedural_art(
                                ui.painter(),
                                art_rect,
                                game.id.0,
                                CornerRadius::same(8),
                                Some("HTML5 GAME"),
                            );
                            ui.painter().text(
                                Pos2::new(art_rect.left() + 10.0, art_rect.bottom() - 28.0),
                                Align2::LEFT_CENTER,
                                &game.title,
                                FontId::proportional(15.0),
                                Color32::WHITE,
                            );
                            ui.painter().text(
                                Pos2::new(art_rect.left() + 10.0, art_rect.bottom() - 12.0),
                                Align2::LEFT_CENTER,
                                &game.description,
                                FontId::proportional(10.5),
                                Color32::from_rgba_unmultiplied(255, 255, 255, 225),
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if let Some(top) = high_scores.first() {
                                    let top_name = self.backend.resolve_user_name(top.user_id);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Top score: {top_name} ({} pts)",
                                            top.score
                                        ))
                                        .size(11.5)
                                        .color(palette.window_sub_text_fg),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let link = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new("Leaderboard & Share")
                                                    .size(11.5)
                                                    .color(palette.window_active_text_fg),
                                            )
                                            .sense(Sense::click()),
                                        );
                                        if link.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        }
                                        if link.clicked() {
                                            *open_share_game = true;
                                        }
                                    },
                                );
                            });
                        }
                        BotMessageAttachment::InlineSent {
                            kind_label,
                            title,
                            subtitle,
                        } => {
                            ui.add_space(4.0);
                            egui::Frame::new()
                                .fill(palette.window_bg_over)
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(egui::Margin::same(8))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(format!("[{kind_label}] {title}"))
                                            .size(12.5)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    );
                                    if !subtitle.is_empty() {
                                        ui.label(
                                            egui::RichText::new(subtitle)
                                                .size(11.5)
                                                .color(palette.window_sub_text_fg),
                                        );
                                    }
                                });
                        }
                    }

                    if !msg.text.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(&msg.text)
                                .size(13.0)
                                .color(palette.window_fg),
                        );
                    }
                });

                // Attached inline keyboard painted directly beneath the bubble
                if let Some(ref markup) = msg.inline_markup
                    && markup.is_inline()
                {
                    ui.add_space(INLINE_KEYBOARD_TOP_SKIP);
                    let kb_w = resp.response.rect.width().max(max_w);
                    let params = InlineKeyboardParams::for_message(
                        msg.id,
                        Some(self.chat.bot_id),
                        self.chat.bot_verified,
                        kb_w,
                        time_secs,
                    );
                    if let Some(act) =
                        show_inline_keyboard(ui, markup, params, &mut self.inline_kb_state, palette)
                    {
                        activation = Some(act);
                    }
                }
            });
        });

        activation
    }

    fn handle_inline_activation(&mut self, act: InlineKeyboardActivation, now_ms: u64) {
        match act.action {
            BotButtonAction::SendCallback(req) => {
                // Special demo shortcut: "Share Score" button on the game message opens ShareGameScoreBox
                if req.data.as_deref() == Some(b"share_game_score")
                    && act.msg_id.msg == DEMO_GAME_MSG_ID
                {
                    self.open_demo_share_game_box();
                    return;
                }
                if req.requires_password {
                    self.active_modal = Some(BotModalBox::CallbackPassword {
                        request: req,
                        button_text: act.button.text,
                        password_input: String::new(),
                        error: None,
                    });
                    return;
                }
                // Check `CallbackAnswerCache` before starting a new loading request.
                let cache_key = CallbackCacheKey {
                    item_id: act.msg_id,
                    is_game: req.is_game,
                    data: req.data.clone().unwrap_or_default(),
                };
                if let Some(cached) = self.callback_cache.get(&cache_key, now_ms / 1_000).cloned() {
                    self.apply_callback_answer(&req, &cached, now_ms);
                    return;
                }
                // Start loading spinner (visible for 450 ms before mock answer completes).
                self.inline_kb_state.set_loading(
                    act.msg_id,
                    act.row,
                    act.col,
                    now_ms as f64 / 1_000.0,
                );
                self.pending_callbacks.push(PendingCallback {
                    msg_id: act.msg_id,
                    row: act.row,
                    col: act.col,
                    request: req,
                    deliver_at_ms: now_ms.saturating_add(450),
                });
            }
            BotButtonAction::OpenUrl {
                url,
                require_confirmation,
            } => {
                if require_confirmation {
                    self.active_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: self.chat.bot_name.clone(),
                    });
                } else {
                    self.last_opened_url = Some(url.clone());
                    self.show_toast(format!("Opening verified link: {url}"), now_ms);
                }
            }
            BotButtonAction::RequestUrlAuth(source) => {
                self.begin_url_auth(source, now_ms);
            }
            BotButtonAction::SwitchInline(decision) => match decision {
                SwitchInlineDecision::SwitchSamePeer { query, .. } => {
                    self.compose_text = format!("@{} {query}", self.chat.bot_username);
                    let _ = self.inline_results.fetch_immediately(
                        self.chat.bot_id,
                        self.chat.bot_username.clone(),
                        self.chat.peer_id,
                        query,
                        now_ms,
                        self.backend.as_mut(),
                    );
                }
                SwitchInlineDecision::ReturnToSavedPeer { query, .. }
                | SwitchInlineDecision::ChooseRecipient { query, .. } => {
                    self.compose_text = format!("@{} {query}", self.chat.bot_username);
                    let _ = self.inline_results.fetch_immediately(
                        self.chat.bot_id,
                        self.chat.bot_username.clone(),
                        self.chat.peer_id,
                        query,
                        now_ms,
                        self.backend.as_mut(),
                    );
                    self.show_toast("Switched to inline search mode", now_ms);
                }
            },
            BotButtonAction::OpenWebView { text, url, .. } => {
                self.last_opened_url = Some(url.clone());
                self.show_toast(format!("Opening Mini App \"{text}\" ({url})"), now_ms);
            }
            BotButtonAction::CopyText { text } => {
                self.last_copied_text = Some(text.clone());
                self.show_toast(format!("Copied to clipboard: {text}"), now_ms);
            }
            BotButtonAction::OpenUserProfile { user_id } => {
                let name = self.backend.resolve_user_name(user_id);
                self.show_toast(
                    format!("Opening user profile: {name} (#{})", user_id.0),
                    now_ms,
                );
            }
            BotButtonAction::OpenCheckout { .. } => {
                self.show_toast(
                    "Opening payment checkout (see Ctrl+Shift+P Payments)",
                    now_ms,
                );
            }
            _ => {}
        }
    }

    fn handle_reply_activation(&mut self, act: ReplyKeyboardActivation, now_ms: u64) {
        match act.action {
            BotButtonAction::SendCommand {
                peer_id,
                command,
                reply_to,
                ..
            } => {
                if let Ok(msg) = self.backend.send_bot_command(peer_id, &command, reply_to) {
                    self.chat.messages.push(msg);
                }
                self.show_toast(format!("Sent reply button: {command}"), now_ms);
            }
            BotButtonAction::ConfirmSharePhone { item_id, .. } => {
                self.active_modal = Some(BotModalBox::RequestPhone {
                    item_id,
                    bot_name: self.chat.bot_name.clone(),
                    phone_number: self.chat.current_user_phone.clone(),
                });
            }
            BotButtonAction::ShowLocationUnavailable { .. } => {
                self.active_modal = Some(BotModalBox::request_location(self.chat.bot_name.clone()));
            }
            BotButtonAction::CreatePoll { restriction, .. } => {
                self.active_modal = Some(BotModalBox::request_poll(restriction));
            }
            BotButtonAction::ChoosePeer {
                item_id,
                button_id,
                query,
                ..
            } => {
                self.active_modal = Some(BotModalBox::RequestPeer {
                    item_id,
                    button_id,
                    query,
                    selected: Vec::new(),
                    search: String::new(),
                });
            }
            BotButtonAction::OpenWebView { text, url, .. } => {
                self.last_opened_url = Some(url.clone());
                self.show_toast(format!("Opening Simple WebView \"{text}\" ({url})"), now_ms);
            }
            _ => {}
        }
    }

    fn begin_url_auth(&mut self, source: UrlAuthSource, now_ms: u64) {
        match self.backend.request_url_auth(&source) {
            Ok(result) => match handle_url_auth_request_result(source, result) {
                UrlAuthDecision::OpenAcceptedUrl { url, .. } => {
                    self.last_opened_url = Some(url.clone());
                    self.show_toast(format!("Authenticated redirect: {url}"), now_ms);
                }
                UrlAuthDecision::OpenDefaultUrl { url, .. } => {
                    self.active_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: self.chat.bot_name.clone(),
                    });
                }
                UrlAuthDecision::ShowConfirmation(box_data) => {
                    self.active_modal = Some(BotModalBox::url_auth(
                        *box_data,
                        self.chat.current_user_name.clone(),
                    ));
                }
                UrlAuthDecision::NoOp => {}
            },
            Err(err_type) => match handle_url_auth_request_error(&source, &err_type) {
                tg_bots::callback::UrlAuthErrorAction::ShowExpiredToast => {
                    self.show_toast("Login link has expired.", now_ms);
                }
                tg_bots::callback::UrlAuthErrorAction::OpenDefaultUrl { url, .. } => {
                    self.active_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: self.chat.bot_name.clone(),
                    });
                }
            },
        }
    }

    fn execute_callback_now(&mut self, request: &CallbackQueryRequest, now_ms: u64) {
        match self.backend.send_callback_query(request) {
            Ok(answer) => {
                let cache_key = CallbackCacheKey {
                    item_id: FullMsgId::new(request.peer_id, request.msg_id),
                    is_game: request.is_game,
                    data: request.data.clone().unwrap_or_default(),
                };
                self.callback_cache
                    .insert(cache_key, answer.clone(), now_ms / 1_000);
                self.apply_callback_answer(request, &answer, now_ms);
            }
            Err(err) => {
                let _ = tg_app_core::user_error::bot_callback(&err);
            }
        }
    }

    fn apply_callback_answer(
        &mut self,
        request: &CallbackQueryRequest,
        answer: &tg_bots::callback::CallbackAnswerInput,
        now_ms: u64,
    ) {
        match handle_callback_answer(
            request.is_game,
            request.requires_password,
            self.chat.bot_verified,
            self.chat.bot_trusted_for_game,
            answer,
        ) {
            CallbackAnswerAction::ShowAlert { message } => {
                self.active_modal = Some(BotModalBox::CallbackAlert {
                    bot_name: self.chat.bot_name.clone(),
                    message,
                });
            }
            CallbackAnswerAction::ShowToast { message, .. } => {
                self.show_toast(message, now_ms);
            }
            CallbackAnswerAction::OpenUrl { url } => {
                self.last_opened_url = Some(url.clone());
                self.show_toast(format!("Callback URL: {url}"), now_ms);
            }
            CallbackAnswerAction::OpenGameUrl {
                url,
                require_confirmation,
                ..
            } => {
                if require_confirmation {
                    self.active_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: self.chat.bot_name.clone(),
                    });
                } else {
                    self.last_opened_url = Some(url.clone());
                    self.show_toast(format!("Launching game: {url}"), now_ms);
                }
            }
            CallbackAnswerAction::HidePasswordLayer | CallbackAnswerAction::None => {}
        }
    }

    fn handle_modal_outcome(
        &mut self,
        outcome: BotModalOutcome,
        repaint: Option<Context>,
        now_ms: u64,
    ) {
        match outcome {
            BotModalOutcome::Closed => {}
            BotModalOutcome::OpenUrlConfirmed(url) => {
                self.last_opened_url = Some(url.clone());
                self.show_toast(format!("Opened external link: {url}"), now_ms);
            }
            BotModalOutcome::UrlAuthAnswered { data, choice } => {
                match build_accept_url_auth(data.source.clone(), choice) {
                    AcceptUrlAuthDecision::Decline {
                        url,
                        open_fallback_url,
                    } => {
                        let _ = self.backend.decline_url_auth(&url);
                        if open_fallback_url {
                            self.last_opened_url = Some(url.clone());
                            self.show_toast(format!("Declined login; opening {url}"), now_ms);
                        }
                    }
                    AcceptUrlAuthDecision::Accept(params) => {
                        let source = params.source.clone();
                        let write_note = if params.write_allowed {
                            " (bot write access granted)"
                        } else {
                            ""
                        };
                        match self.backend.accept_url_auth(&params) {
                            Ok(res) => match handle_url_auth_accept_result(&source, res) {
                                UrlAuthAcceptOutcome::OpenUrl { url, .. } => {
                                    self.last_opened_url = Some(url.clone());
                                    self.show_toast(
                                        format!("Logged in to {}{write_note}", data.display_name),
                                        now_ms,
                                    );
                                }
                                UrlAuthAcceptOutcome::ShowSuccessToast => {
                                    self.show_toast(
                                        format!("Authorization succeeded{write_note}"),
                                        now_ms,
                                    );
                                }
                                UrlAuthAcceptOutcome::DoneWithoutNavigation => {
                                    self.show_toast("Login completed.", now_ms);
                                }
                            },
                            Err(err) => {
                                let _ = tg_app_core::user_error::accept_url_auth_button(&err);
                            }
                        }
                    }
                }
            }
            BotModalOutcome::CallbackPasswordSubmitted { request, .. } => {
                self.execute_callback_now(&request, now_ms);
            }
            BotModalOutcome::SharePhoneConfirmed(item_id) => {
                if let Ok(msg) = self.backend.share_phone(item_id.peer, Some(item_id.msg)) {
                    self.chat.messages.push(msg);
                    self.show_toast("Phone number contact shared with bot", now_ms);
                }
            }
            BotModalOutcome::ShareLocationRequested => {
                self.start_location_share(repaint, now_ms);
            }
            BotModalOutcome::ShareLocationConfirmed { lat_e7, lon_e7 } => {
                let lat = (lat_e7 as f64) / 1e7;
                let lon = (lon_e7 as f64) / 1e7;
                let summary = format!("Location: {lat:.5}, {lon:.5}");
                if let Ok(msg) = self
                    .backend
                    .send_bot_command(self.chat.peer_id, &summary, None)
                {
                    self.chat.messages.push(msg);
                }
                self.show_toast(format!("Shared location ({summary})"), now_ms);
            }
            BotModalOutcome::PollSubmitted(poll) => {
                if let Ok(msg) = self.backend.send_poll(self.chat.peer_id, &poll) {
                    self.chat.messages.push(msg);
                    self.show_toast("Poll sent to chat", now_ms);
                }
            }
            BotModalOutcome::PeersSelected {
                item_id,
                button_id,
                peers,
            } => {
                if let Ok(msg) = self
                    .backend
                    .send_requested_peers(item_id, button_id, &peers)
                {
                    self.chat.messages.push(msg);
                    self.show_toast(format!("Shared {} peer(s) with bot", peers.len()), now_ms);
                }
            }
            BotModalOutcome::GameShared {
                message_id,
                target_peer,
            } => match self.backend.share_game_score(message_id, target_peer) {
                Ok(summary) => self.show_toast(summary, now_ms),
                Err(err) => self.show_toast(err, now_ms),
            },
            BotModalOutcome::CopyGameLink(link) => {
                self.last_copied_text = Some(link.clone());
                self.show_toast(format!("Copied game link: {link}"), now_ms);
            }
            BotModalOutcome::AttachDisclaimerAccepted { bot, allow_write } => {
                self.backend.accept_attach_disclaimer(bot.bot_id);
                self.attach_menu.mark_disclaimer_accepted(bot.bot_id);
                let state = if allow_write {
                    AttachMenuToggleState::AllowedToWrite
                } else {
                    AttachMenuToggleState::Added
                };
                if let Ok(store) = self.backend.toggle_attach_menu_bot(bot.bot_id, state) {
                    self.attach_menu = store;
                }
                self.show_toast(format!("Launched Mini App: {}", bot.short_name), now_ms);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AppBotsHost (`Ctrl+Shift+K` floating overlay host for TgforkApp)
// ---------------------------------------------------------------------------

struct BotsOverlayState {
    open: bool,
    screen: BotsDemoScreen,
}

/// Application-wide floating overlay host for [`BotsDemoScreen`].
///
/// Press `Ctrl+Shift+K` in the running desktop app to open or close the Bot Features Demo
/// overlay (matching `AppPaymentsHost` / `AppBusinessHost` / `AppStatisticsHost`).
pub struct AppBotsHost;

impl AppBotsHost {
    fn state_id() -> Id {
        Id::unique("tg_bots_overlay_state")
    }

    fn get_or_init_state(ctx: &Context) -> Arc<Mutex<BotsOverlayState>> {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Self::state_id(), || {
                Arc::new(Mutex::new(BotsOverlayState {
                    open: false,
                    screen: BotsDemoScreen::with_mock(),
                }))
            })
            .clone()
        })
    }

    /// Returns `true` if the `Ctrl+Shift+K` bot demo overlay is currently open.
    #[must_use]
    pub fn is_open(ctx: &Context) -> bool {
        let arc = Self::get_or_init_state(ctx);
        arc.lock().is_ok_and(|g| g.open)
    }

    /// Explicitly opens or closes the bot demo overlay.
    pub fn set_open(ctx: &Context, open: bool) {
        let arc = Self::get_or_init_state(ctx);
        if let Ok(mut guard) = arc.lock() {
            guard.open = open;
        }
    }

    /// Drives the `Ctrl+Shift+K` keyboard shortcut and renders the floating overlay when open.
    pub fn update_and_render(ctx: &Context, palette: &Palette, _mode: ThemeMode) {
        let toggle_pressed =
            ctx.input_mut(|i| i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::K));

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
        let panel_w = (screen_rect.width() * 0.68).clamp(520.0, 680.0);
        let panel_h = (screen_rect.height() - 48.0).max(520.0);
        let panel_pos = Pos2::new(
            screen_rect.center().x - panel_w / 2.0,
            screen_rect.top() + 24.0,
        );

        let mut close_requested = ctx.input(|i| i.key_pressed(Key::Escape));

        egui::Area::new(Id::unique("tg_bots_overlay_backdrop"))
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

        egui::Area::new(Id::unique("tg_bots_overlay_panel"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter().rect_filled(
                    panel_rect.expand(1.5),
                    CornerRadius::same(11),
                    palette.divider,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    if guard.screen.show(ui, palette, true) {
                        close_requested = true;
                    }
                });
            });

        if close_requested {
            guard.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui::epaint::{Primitive, TextureId};
    use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect, Vec2};
    use tg_bots::callback::{
        BotButtonAction, CallbackQueryRequest, UrlAuthSource, UrlAuthUserChoice,
    };
    use tg_bots::commands::{BotCommand, BotMenuButton, BotStatus, wrap_command_in_chat};
    use tg_bots::inline::{INLINE_BOT_REQUEST_DELAY_MS, InlineResultKind};
    use tg_bots::markup::{
        ButtonKind, MarkupButton, PollRequestRestriction, ReplyMarkupData, ReplyMarkupInput,
    };
    use tg_bots::mini_app::{AttachMenuToggleState, can_share_game_to_peer};
    use tg_bots::stand_in::{FullMsgId, MsgId, PeerKind, UserId};

    use super::*;
    use crate::bots::menus::filter_bot_commands;
    use crate::bots::mock::{
        DEMO_BOT_PEER_ID, DEMO_BOT_USER_ID, DEMO_BOT_USERNAME, DEMO_INLINE_KB_MSG_ID,
        DEMO_REPLY_KB_MSG_ID, MockBotsBackend, build_demo_reply_keyboard,
    };

    #[test]
    fn inline_keyboard_loading_state_and_callback_cache_and_alert_toast_flow() {
        let mut screen = BotsDemoScreen::with_mock();
        let msg_id = FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_INLINE_KB_MSG_ID);

        // 1. Trigger toast callback -> enters loading state with spinner, then delivers after 450ms
        let cb_btn = MarkupButton::new(
            "Refresh Status",
            ButtonKind::Callback {
                data: b"cb_toast".to_vec(),
            },
        );
        screen.handle_inline_activation(
            InlineKeyboardActivation {
                msg_id,
                row: 0,
                col: 0,
                button: cb_btn.clone(),
                action: BotButtonAction::SendCallback(CallbackQueryRequest {
                    peer_id: DEMO_BOT_PEER_ID,
                    msg_id: DEMO_INLINE_KB_MSG_ID,
                    is_game: false,
                    data: Some(b"cb_toast".to_vec()),
                    requires_password: false,
                    ephemeral_id: None,
                }),
            },
            1_000,
        );
        assert!(screen.inline_kb_state.is_loading(msg_id, 0, 0));

        // Before 450 ms -> still loading
        screen.tick(1_200);
        assert!(screen.inline_kb_state.is_loading(msg_id, 0, 0));

        // At 1_500 ms -> spinner clears & toast appears (and cached for 15s)
        screen.tick(1_500);
        assert!(!screen.inline_kb_state.is_loading(msg_id, 0, 0));
        let toast_msg = screen
            .toast
            .as_ref()
            .map(|(m, _)| m.as_str())
            .unwrap_or_default();
        assert!(toast_msg.contains("Catalog synced"));

        // Second click within cache_time resolves immediately from cache without spinner!
        screen.handle_inline_activation(
            InlineKeyboardActivation {
                msg_id,
                row: 0,
                col: 0,
                button: cb_btn,
                action: BotButtonAction::SendCallback(CallbackQueryRequest {
                    peer_id: DEMO_BOT_PEER_ID,
                    msg_id: DEMO_INLINE_KB_MSG_ID,
                    is_game: false,
                    data: Some(b"cb_toast".to_vec()),
                    requires_password: false,
                    ephemeral_id: None,
                }),
            },
            5_000,
        );
        assert!(!screen.inline_kb_state.is_loading(msg_id, 0, 0));

        // 2. Alert callback opens modal CallbackAlert box
        screen.handle_inline_activation(
            InlineKeyboardActivation {
                msg_id,
                row: 0,
                col: 1,
                button: MarkupButton::new(
                    "Show Alert",
                    ButtonKind::Callback {
                        data: b"cb_alert".to_vec(),
                    },
                ),
                action: BotButtonAction::SendCallback(CallbackQueryRequest {
                    peer_id: DEMO_BOT_PEER_ID,
                    msg_id: DEMO_INLINE_KB_MSG_ID,
                    is_game: false,
                    data: Some(b"cb_alert".to_vec()),
                    requires_password: false,
                    ephemeral_id: None,
                }),
            },
            6_000,
        );
        screen.tick(6_500);
        assert!(matches!(
            screen.active_modal,
            Some(BotModalBox::CallbackAlert { .. })
        ));
    }

    #[test]
    fn url_confirm_and_url_auth_with_write_access_checkbox_flow() {
        let mut screen = BotsDemoScreen::with_mock();
        let msg_id = FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_INLINE_KB_MSG_ID);

        // Unverified bot URL opens confirmation dialog
        screen.handle_inline_activation(
            InlineKeyboardActivation {
                msg_id,
                row: 1,
                col: 0,
                button: MarkupButton::new(
                    "Open Docs",
                    ButtonKind::Url {
                        url: "https://example.org/docs".to_owned(),
                    },
                ),
                action: BotButtonAction::OpenUrl {
                    url: "https://example.org/docs".to_owned(),
                    require_confirmation: true,
                },
            },
            1_000,
        );
        assert!(matches!(
            screen.active_modal,
            Some(BotModalBox::UrlConfirm { .. })
        ));
        screen.handle_modal_outcome(
            BotModalOutcome::OpenUrlConfirmed("https://example.org/docs".to_owned()),
            None,
            1_100,
        );
        assert_eq!(
            screen.last_opened_url.as_deref(),
            Some("https://example.org/docs")
        );

        // Login URL button requests URL Auth and builds confirmation box with write_access checkbox
        let source = UrlAuthSource::Button {
            peer_id: DEMO_BOT_PEER_ID,
            msg_id: DEMO_INLINE_KB_MSG_ID,
            button_id: 901,
            url: "https://portal.example.org/oauth/callback".to_owned(),
        };
        screen.handle_inline_activation(
            InlineKeyboardActivation {
                msg_id,
                row: 1,
                col: 1,
                button: MarkupButton::new(
                    "Login via Telegram",
                    ButtonKind::Auth {
                        url: "https://portal.example.org/oauth/callback".to_owned(),
                        forward_text: "Open Portal".to_owned(),
                        button_id: 901,
                    },
                ),
                action: BotButtonAction::RequestUrlAuth(source),
            },
            2_000,
        );

        let auth_data = match &screen.active_modal {
            Some(BotModalBox::UrlAuth {
                data, allow_write, ..
            }) => {
                assert_eq!(data.display_name, "portal.example.org");
                assert_eq!(
                    data.write_access_bot_name.as_deref(),
                    Some("Catalog Assistant")
                );
                assert!(*allow_write, "allow_write defaults to true when requested");
                data.clone()
            }
            other => panic!("expected UrlAuth modal, got {other:?}"),
        };

        // Accept with allow_write = true -> URL contains write_allowed=1
        screen.handle_modal_outcome(
            BotModalOutcome::UrlAuthAnswered {
                data: auth_data,
                choice: UrlAuthUserChoice {
                    auth: true,
                    allow_write: true,
                    share_phone: false,
                    match_code: Some("A7K".to_owned()),
                },
            },
            None,
            2_100,
        );
        let opened = screen.last_opened_url.as_deref().unwrap_or_default();
        assert!(opened.contains("tgAuthResult=demo_token_ok"));
        assert!(opened.contains("write_allowed=1"));
    }

    #[test]
    fn reply_keyboard_flags_single_use_placeholder_and_remove_keyboard() {
        let mut kb = ReplyKeyboardWidget::new();
        let msg_id = FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_REPLY_KB_MSG_ID);

        // Single-use + resized + selective keyboard with placeholder
        let markup = build_demo_reply_keyboard(true, true, false, true);
        assert!(kb.update_markup(Some(msg_id), Some(&markup), true));
        assert!(kb.has_markup());
        assert!(kb.is_shown());
        assert!(!kb.state.maximize_size);
        assert!(kb.state.single_use);
        assert!(kb.state.selective);
        assert_eq!(
            kb.placeholder(),
            Some("Choose a bot command or type a message...")
        );

        // Toggle hides and shows
        kb.toggle_visible();
        assert!(!kb.is_shown());
        kb.toggle_visible();
        assert!(kb.is_shown());

        // Single-use hide clears state
        assert!(kb.state.hide_if_single_use(msg_id));
        assert!(!kb.state.has_visible_rows);

        // RemoveKeyboard clears active keyboard
        let kb2 = build_demo_reply_keyboard(false, false, true, false);
        assert!(kb.update_markup(Some(msg_id), Some(&kb2), true));
        assert!(kb.state.maximize_size);
        assert!(kb.state.persistent);
        let remove = ReplyMarkupData::from_input(Some(ReplyMarkupInput::RemoveKeyboard {
            selective: false,
        }));
        assert!(kb.update_markup(
            Some(FullMsgId::new(DEMO_BOT_PEER_ID, MsgId(999))),
            Some(&remove),
            false
        ));
        assert!(!kb.has_markup());
    }

    #[test]
    fn inline_results_debounce_pagination_list_and_gallery_modes() {
        let mut backend = MockBotsBackend::new();
        let mut panel = InlineResultsPanel::new();

        // Typing `@catalog_demo_bot photos` schedules 400 ms debounce
        assert!(panel.sync_from_compose_text(
            "@catalog_demo_bot photos",
            DEMO_BOT_USER_ID,
            DEMO_BOT_USERNAME,
            DEMO_BOT_PEER_ID,
            1_000
        ));
        assert!(panel.visible);
        assert!(!panel.poll_debounce(1_399, &mut backend));
        assert!(panel.poll_debounce(1_000 + INLINE_BOT_REQUEST_DELAY_MS, &mut backend));

        let entry = panel
            .controller
            .current_entry()
            .expect("gallery page loaded");
        assert!(entry.gallery);
        assert_eq!(entry.next_offset, "page_2");
        assert!(entry.switch_pm.is_some());
        let first_count = entry.results.len();
        assert!(first_count >= 5);

        // Paginate via `next_offset` -> appends page 2 and exhausts `next_offset`
        assert!(panel.load_more(&mut backend));
        let entry2 = panel.controller.current_entry().expect("page 2 loaded");
        assert!(entry2.results.len() > first_count);
        assert!(entry2.next_offset.is_empty());

        // List query covers article, video, audio, voice, document, contact, venue, game
        assert!(panel.fetch_immediately(
            DEMO_BOT_USER_ID,
            DEMO_BOT_USERNAME,
            DEMO_BOT_PEER_ID,
            "catalog",
            2_000,
            &mut backend
        ));
        assert!(panel.load_more(&mut backend));
        let list_entry = panel.controller.current_entry().expect("list loaded");
        assert!(!list_entry.gallery);
        let kinds: Vec<InlineResultKind> = list_entry.results.iter().map(|r| r.kind).collect();
        for required in [
            InlineResultKind::Article,
            InlineResultKind::Video,
            InlineResultKind::Audio,
            InlineResultKind::Voice,
            InlineResultKind::Document,
            InlineResultKind::Contact,
            InlineResultKind::Location,
            InlineResultKind::Venue,
            InlineResultKind::Game,
        ] {
            assert!(
                kinds.contains(&required),
                "missing inline result kind {required:?}"
            );
        }
    }

    #[test]
    fn bot_commands_filtering_wrapping_attach_menu_and_game_share_rules() {
        let mut backend = MockBotsBackend::new();
        let cmds = backend.commands_for_chat();
        assert_eq!(filter_bot_commands(&cmds, "/gam").len(), 1);
        assert_eq!(filter_bot_commands(&cmds, "/gam")[0].command, "game");
        assert_eq!(
            wrap_command_in_chat(BotStatus::HasBots, "/game", DEMO_BOT_USERNAME),
            "/game@catalog_demo_bot"
        );
        assert_eq!(
            wrap_command_in_chat(BotStatus::NoBots, "/game", DEMO_BOT_USERNAME),
            "/game"
        );

        // Attach menu peer filtering & disclaimer NEW badge
        let store = backend.load_attach_menu();
        let in_bot_chat = store.visible_in_attach_menu(DEMO_BOT_PEER_ID, PeerKind::Bot);
        assert_eq!(in_bot_chat.len(), 3);
        let wallet = store.find(UserId(777_002)).expect("wallet bot");
        assert!(store.show_main_menu_new_badge(wallet));
        backend.accept_attach_disclaimer(UserId(777_002));
        let updated = backend.load_attach_menu();
        assert!(!updated.show_main_menu_new_badge(updated.find(UserId(777_002)).unwrap()));

        // Toggle inactive bot on
        let toggled = backend
            .toggle_attach_menu_bot(UserId(777_004), AttachMenuToggleState::Added)
            .unwrap();
        assert!(!toggled.find(UserId(777_004)).unwrap().inactive);

        // Game share peer eligibility & Poll restriction defaults
        let candidates = backend.candidate_peers();
        let shareable: Vec<_> = candidates
            .iter()
            .filter(|c| can_share_game_to_peer(c.is_self, c.can_send_games, c.is_forum))
            .collect();
        assert!(shareable.len() >= 3);
        assert!(!can_share_game_to_peer(true, true, false));

        if let BotModalBox::RequestPoll { is_quiz, .. } =
            BotModalBox::request_poll(PollRequestRestriction::QuizOnly)
        {
            assert!(is_quiz);
        } else {
            panic!("expected RequestPoll modal");
        }

        // Menu button switch
        let mut screen = BotsDemoScreen::with_mock();
        assert!(!screen.chat().bot_info.menu_button.is_web_app());
        screen.toggle_menu_button_kind(100);
        assert!(matches!(
            screen.chat().bot_info.menu_button,
            BotMenuButton::WebApp { .. }
        ));
        let _ = BotCommand::new("test", "desc");
    }

    #[test]
    fn app_bots_host_toggles_on_ctrl_shift_k() {
        let ctx = egui::Context::default();
        let palette = Palette::day();
        assert!(!AppBotsHost::is_open(&ctx));

        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 760.0))),
            events: vec![Event::Key {
                key: Key::K,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(input.clone(), |ui| {
            AppBotsHost::update_and_render(ui.ctx(), &palette, ThemeMode::Day);
        });
        out.textures_delta.clear();
        assert!(AppBotsHost::is_open(&ctx));

        // Second Ctrl+Shift+K closes it
        input.time = Some(0.2);
        let mut out2 = ctx.run_ui(input, |ui| {
            AppBotsHost::update_and_render(ui.ctx(), &palette, ThemeMode::Day);
        });
        out2.textures_delta.clear();
        assert!(!AppBotsHost::is_open(&ctx));
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
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
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
    fn renders_bot_ui_screenshots_via_egui() {
        let write_files = std::env::var("RUSTGRAM_UPDATE_SCREENSHOTS").as_deref() == Ok("1")
            || std::env::var("TGFORK_UPDATE_SCREENSHOTS").as_deref() == Ok("1");
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        if write_files {
            let _ = std::fs::create_dir_all(&docs_dir);
        }
        let palette = Palette::day();

        // 1. Main Bot Chat: Inline Keyboard (with live spinner) + Reply Keyboard + Toast
        let mut screen = BotsDemoScreen::with_mock();
        screen.inline_kb_state.set_loading(
            FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_INLINE_KB_MSG_ID),
            0,
            0,
            0.35,
        );
        screen.show_toast("Callback toast: Catalog synced (cached 15s)", 1_000);
        let chat_png = render_egui_to_png(640, 860, 1.5, |ui| {
            screen.show_at(ui, &palette, false, 1_350);
        });
        assert!(chat_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_chat_keyboards.png"), &chat_png)
                .expect("write bots_chat_keyboards.png");
        }

        // 2. Inline Bot Results — Gallery Mosaic Mode + switch_pm
        let mut screen2 = BotsDemoScreen::with_mock();
        screen2.select_tab(BotsDemoTab::InlineGallery, 1_000);
        let gallery_png = render_egui_to_png(640, 720, 1.5, |ui| {
            screen2.show_at(ui, &palette, false, 1_500);
        });
        assert!(gallery_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_inline_gallery.png"), &gallery_png)
                .expect("write bots_inline_gallery.png");
        }

        // 3. Inline Bot Results — List Mode (Article, Video, Audio, Voice, Doc, Contact, Venue, Game)
        let mut screen3 = BotsDemoScreen::with_mock();
        screen3.select_tab(BotsDemoTab::InlineList, 1_000);
        let list_png = render_egui_to_png(640, 740, 1.5, |ui| {
            screen3.show_at(ui, &palette, false, 1_500);
        });
        assert!(list_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_inline_list.png"), &list_png)
                .expect("write bots_inline_list.png");
        }

        // 4. Empty Bot Chat Info Header + Bot Commands Popup
        let mut screen4 = BotsDemoScreen::with_mock();
        screen4.select_tab(BotsDemoTab::EmptyBotIntro, 1_000);
        screen4.commands_popup_open = true;
        let empty_png = render_egui_to_png(640, 780, 1.5, |ui| {
            screen4.show_at(ui, &palette, false, 1_500);
        });
        assert!(empty_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_empty_commands.png"), &empty_png)
                .expect("write bots_empty_commands.png");
        }

        // 5. URL Auth Confirmation Box ("Allow Catalog Assistant to send you messages")
        let mut screen5 = BotsDemoScreen::with_mock();
        screen5.open_demo_url_auth_box(1_000);
        let auth_png = render_egui_to_png(640, 720, 1.5, |ui| {
            screen5.show_at(ui, &palette, false, 1_500);
        });
        assert!(auth_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_url_auth_box.png"), &auth_png)
                .expect("write bots_url_auth_box.png");
        }

        // 6. Share Game Score Box
        let mut screen6 = BotsDemoScreen::with_mock();
        screen6.open_demo_share_game_box();
        let game_png = render_egui_to_png(640, 720, 1.5, |ui| {
            screen6.show_at(ui, &palette, false, 1_500);
        });
        assert!(game_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_share_game_box.png"), &game_png)
                .expect("write bots_share_game_box.png");
        }

        // 7. Attach Menu Bots Popup
        let mut screen7 = BotsDemoScreen::with_mock();
        screen7.reply_keyboard.visible = false;
        screen7.attach_popup_open = true;
        let attach_png = render_egui_to_png(640, 720, 1.5, |ui| {
            screen7.show_at(ui, &palette, false, 1_500);
        });
        assert!(attach_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("bots_attach_menu.png"), &attach_png)
                .expect("write bots_attach_menu.png");
        }
    }
}
