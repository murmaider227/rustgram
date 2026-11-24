//! Built-in TON Wallet UI screens and overlay host (`AppWalletHost`) for `tg-app`.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_panel.cpp`, `wallet_content.cpp`,
//! `wallet_ton_connect_box.cpp`, `wallet_ton_connect_request_box.cpp`, and
//! `Telegram/SourceFiles/window/window_main_menu.cpp` (wallet entry gated on `walletAvailable`).

pub use tg_app_core::wallet::backend;
pub mod backup;
pub mod card_editor;
pub mod collectibles;
pub mod mock;
pub mod onboarding;
pub mod onramp;
pub mod overview;
pub mod receive;
pub mod send;
pub mod ton_connect_ui;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use egui::{
    Align, Context, CornerRadius, CursorIcon, Frame, Id, Key, Layout, Margin, Modifiers, Order,
    Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2,
};
use tg_ui::stand_in::Colors;
use tg_wallet::WalletAppConfig;

use tg_app_core::core_types::window_types::WalletDeepLinkAction;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

use backend::WalletBackend;
use backup::{BackupOutcome, BackupUiState, render_backup};
use card_editor::{CardEditorOutcome, CardEditorState, render_card_editor};
use collectibles::{CollectiblesOutcome, CollectiblesUiState, render_collectibles};
use mock::MockWalletBackend;
use onboarding::{OnboardingOutcome, OnboardingState, OnboardingStep, render_onboarding};
use onramp::{OnrampOutcome, OnrampUiState, render_onramp};
use overview::{OverviewAction, render_overview};
use receive::{ReceiveOutcome, ReceiveState, render_receive};
use send::{SendOutcome, SendState, render_send};
use ton_connect_ui::{TonConnectOutcome, TonConnectUiState, render_ton_connect};

/// Active screen inside [`WalletPanel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WalletScreen {
    /// Balance card & transaction history.
    #[default]
    Overview,
    /// Create / Import / Passcode unlock screens.
    Onboarding,
    /// Send TON form + confirmation + receipt.
    Send,
    /// Receive TON with QR code.
    Receive,
    /// TON Connect dApp sessions & approval boxes.
    TonConnect,
    /// NFT Collectibles gallery & Jetton token balances + transfer UI.
    Collectibles,
    /// Read-only fiat onramp & swap external link handoff screen.
    Onramp,
    /// Shamir `(k, n)` recovery-phrase backup and reconstruction screen.
    Backup,
    /// Wallet Card Editor (`wallet_card_editor.cpp` / `wallet.setCardStickers`).
    CardEditor,
}

/// Stateful TON Wallet panel coordinator backed by any [`WalletBackend`].
pub struct WalletPanel {
    backend: Box<dyn WalletBackend>,
    screen: WalletScreen,
    pending_screen_after_unlock: Option<WalletScreen>,
    onboarding: OnboardingState,
    send: SendState,
    receive: ReceiveState,
    ton_connect: TonConnectUiState,
    collectibles: CollectiblesUiState,
    onramp: OnrampUiState,
    backup: BackupUiState,
    card_editor: CardEditorState,
    selected_tx_id: Option<String>,
    toast: Option<String>,
}

impl WalletPanel {
    /// Creates a new [`WalletPanel`] backed by `backend`.
    #[must_use]
    pub fn new(backend: Box<dyn WalletBackend>) -> Self {
        let ov = backend.overview();
        let (screen, step) = if !ov.has_wallet {
            (WalletScreen::Onboarding, OnboardingStep::Welcome)
        } else if ov.locked {
            (WalletScreen::Onboarding, OnboardingStep::UnlockVault)
        } else {
            (WalletScreen::Overview, OnboardingStep::Welcome)
        };
        Self {
            backend,
            screen,
            pending_screen_after_unlock: None,
            onboarding: OnboardingState {
                step,
                ..OnboardingState::default()
            },
            send: SendState::default(),
            receive: ReceiveState::default(),
            ton_connect: TonConnectUiState::default(),
            collectibles: CollectiblesUiState::default(),
            onramp: OnrampUiState::default(),
            backup: BackupUiState::default(),
            card_editor: CardEditorState::default(),
            selected_tx_id: None,
            toast: None,
        }
    }

    /// Creates a [`WalletPanel`] pre-wired with [`MockWalletBackend`] `MockSession` demo data.
    #[must_use]
    pub fn with_mock_demo() -> Self {
        Self::new(Box::new(MockWalletBackend::with_demo_wallet()))
    }

    /// Returns whether the wallet is enabled according to tdesktop's `Wallet::Available` gating.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.backend.app_config().is_available()
    }

    /// Updates app-config feature flags (`wallet_available`, `support_mode`, endpoints).
    pub fn set_app_config(&mut self, config: WalletAppConfig) {
        self.backend.set_app_config(config);
    }

    /// Returns the currently displayed [`WalletScreen`].
    #[must_use]
    pub const fn active_screen(&self) -> WalletScreen {
        self.screen
    }

    /// Switches directly to `screen`.
    pub fn select_screen(&mut self, screen: WalletScreen) {
        self.screen = screen;
    }

    /// Mutable access to the underlying backend (for tests and live wiring).
    pub fn backend_mut(&mut self) -> &mut dyn WalletBackend {
        self.backend.as_mut()
    }

    /// Mutable access to the Shamir Backup UI state (for tests and screenshots).
    pub fn backup_state_mut(&mut self) -> &mut BackupUiState {
        &mut self.backup
    }

    /// Mutable access to the Collectibles & Move-to-Gifts UI state (for tests and screenshots).
    pub fn collectibles_state_mut(&mut self) -> &mut CollectiblesUiState {
        &mut self.collectibles
    }

    /// Mutable access to the Card Editor UI state (for tests and screenshots).
    pub fn card_editor_state_mut(&mut self) -> &mut CardEditorState {
        &mut self.card_editor
    }

    /// Read-only access to the current Send TON form state (for tests and deep-link verification).
    #[must_use]
    pub const fn send_state(&self) -> &SendState {
        &self.send
    }

    /// Pre-fills the Send TON screen and switches to it (remembering it across vault unlock).
    pub fn open_send_prefilled(
        &mut self,
        to: Option<&str>,
        amount: Option<&str>,
        comment: Option<&str>,
        bounceable: Option<bool>,
    ) {
        self.send.reset();
        if let Some(dest) = to {
            self.send.destination_input = dest.to_owned();
            if bounceable.is_none()
                && let Ok(parsed) = tg_wallet::address::TonAddress::parse(dest)
            {
                self.send.bounceable = parsed.bounceable;
            }
        }
        if let Some(amt) = amount {
            self.send.amount_input = amt.to_owned();
        }
        if let Some(text) = comment {
            self.send.comment_input = text.to_owned();
        }
        if let Some(b) = bounceable {
            self.send.bounceable = b;
        }
        let ov = self.backend.overview();
        if !ov.has_wallet || ov.locked {
            self.pending_screen_after_unlock = Some(WalletScreen::Send);
            self.screen = WalletScreen::Onboarding;
            self.onboarding.step = if ov.has_wallet {
                OnboardingStep::UnlockVault
            } else {
                OnboardingStep::Welcome
            };
        } else {
            self.pending_screen_after_unlock = None;
            self.screen = WalletScreen::Send;
        }
    }

    /// Queues a TON Connect link into the backend and switches to [`WalletScreen::TonConnect`].
    pub fn open_ton_connect_link(&mut self, raw_link: &str) -> Result<(), String> {
        self.backend.handle_ton_connect_link(raw_link)?;
        self.ton_connect.error = None;
        let ov = self.backend.overview();
        if !ov.has_wallet || ov.locked {
            self.pending_screen_after_unlock = Some(WalletScreen::TonConnect);
            self.screen = WalletScreen::Onboarding;
            self.onboarding.step = if ov.has_wallet {
                OnboardingStep::UnlockVault
            } else {
                OnboardingStep::Welcome
            };
        } else {
            self.pending_screen_after_unlock = None;
            self.screen = WalletScreen::TonConnect;
        }
        Ok(())
    }

    /// Applies a structured [`WalletDeepLinkAction`] routed from `window/deep_links.rs`.
    pub fn handle_deep_link_action(&mut self, action: &WalletDeepLinkAction) -> Result<(), String> {
        if !self.is_available() {
            return Err("TON Wallet is disabled by application configuration.".to_owned());
        }
        match action {
            WalletDeepLinkAction::OpenOverview => {
                let ov = self.backend.overview();
                self.pending_screen_after_unlock = None;
                if !ov.has_wallet || ov.locked {
                    self.screen = WalletScreen::Onboarding;
                    self.onboarding.step = if ov.has_wallet {
                        OnboardingStep::UnlockVault
                    } else {
                        OnboardingStep::Welcome
                    };
                } else {
                    self.screen = WalletScreen::Overview;
                }
                Ok(())
            }
            WalletDeepLinkAction::Send {
                to,
                amount,
                comment,
                bounceable,
            } => {
                self.open_send_prefilled(
                    to.as_deref(),
                    amount.as_deref(),
                    comment.as_deref(),
                    *bounceable,
                );
                Ok(())
            }
            WalletDeepLinkAction::TonConnect { raw_link } => self.open_ton_connect_link(raw_link),
        }
    }

    /// Renders the wallet panel inside `ui`. Returns `true` if the user clicked Close.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette, show_close_button: bool) -> bool {
        let mut close_clicked = false;
        let overview = self.backend.overview();
        let testnet = self.backend.app_config().testnet;

        // Enforce lock/onboarding gate if wallet is missing or vault is locked.
        if !overview.has_wallet && self.screen != WalletScreen::Onboarding {
            self.screen = WalletScreen::Onboarding;
            self.onboarding.step = OnboardingStep::Welcome;
        } else if overview.has_wallet && overview.locked && self.screen != WalletScreen::Onboarding
        {
            self.screen = WalletScreen::Onboarding;
            self.onboarding.step = OnboardingStep::UnlockVault;
        }

        let pending_tc_count = usize::from(self.backend.pending_connect().is_some())
            + usize::from(self.backend.pending_tx_approval().is_some())
            + usize::from(self.backend.pending_sign_data().is_some());

        apply_wallet_visuals(ui, palette);

        Frame::NONE
            .fill(palette.window_bg)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());

                // Header bar.
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("TON Wallet")
                            .size(16.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    if testnet {
                        ui.label(
                            RichText::new("TESTNET")
                                .size(11.0)
                                .strong()
                                .color(palette.attention_fg),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if show_close_button && ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    });
                });
                ui.separator();
                ui.add_space(6.0);

                if !self.is_available() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(36.0);
                        ui.label(
                            RichText::new("TON Wallet is disabled by application configuration.")
                                .color(palette.window_sub_text_fg),
                        );
                    });
                    return;
                }

                match self.screen {
                    WalletScreen::Onboarding => {
                        match render_onboarding(
                            ui,
                            &mut self.onboarding,
                            self.backend.as_mut(),
                            palette,
                        ) {
                            OnboardingOutcome::None => {}
                            OnboardingOutcome::Ready(msg) => {
                                self.toast = Some(msg);
                                self.screen = self
                                    .pending_screen_after_unlock
                                    .take()
                                    .unwrap_or(WalletScreen::Overview);
                            }
                        }
                    }
                    WalletScreen::Overview => {
                        let card_state = self.backend.wallet_card_state();
                        let cloud_backup = self.backend.cloud_backup_status();
                        let gasless = self.backend.gasless_terms();
                        match render_overview(
                            ui,
                            &overview,
                            &card_state,
                            cloud_backup,
                            gasless.as_ref(),
                            &mut self.selected_tx_id,
                            testnet,
                            pending_tc_count,
                            palette,
                        ) {
                            OverviewAction::None => {}
                            OverviewAction::OpenSend => {
                                self.send.reset();
                                self.screen = WalletScreen::Send;
                            }
                            OverviewAction::OpenReceive => {
                                self.screen = WalletScreen::Receive;
                            }
                            OverviewAction::OpenTonConnect => {
                                self.screen = WalletScreen::TonConnect;
                            }
                            OverviewAction::OpenCollectibles => {
                                self.screen = WalletScreen::Collectibles;
                            }
                            OverviewAction::OpenOnramp => {
                                self.screen = WalletScreen::Onramp;
                            }
                            OverviewAction::OpenBackup => {
                                self.screen = WalletScreen::Backup;
                            }
                            OverviewAction::OpenCardEditor => {
                                self.card_editor.load_from_backend(self.backend.as_ref());
                                self.screen = WalletScreen::CardEditor;
                            }
                            OverviewAction::Refresh => {
                                if let Err(err) = self.backend.refresh() {
                                    self.toast = Some(err);
                                } else {
                                    self.toast = Some("Wallet refreshed".to_owned());
                                }
                            }
                            OverviewAction::LockVault => {
                                self.backend.lock_vault();
                                self.onboarding.step = OnboardingStep::UnlockVault;
                                self.screen = WalletScreen::Onboarding;
                                self.toast = Some("Wallet locked".to_owned());
                            }
                            OverviewAction::OpenOnboarding => {
                                self.onboarding.step = OnboardingStep::Welcome;
                                self.screen = WalletScreen::Onboarding;
                            }
                            OverviewAction::Toast(msg) => {
                                self.toast = Some(msg);
                            }
                        }
                    }
                    WalletScreen::Send => {
                        match render_send(
                            ui,
                            &mut self.send,
                            self.backend.as_mut(),
                            overview.balance_nano,
                            overview.usd_per_ton,
                            testnet,
                            palette,
                        ) {
                            SendOutcome::None => {}
                            SendOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            SendOutcome::Sent(msg) => {
                                self.toast = Some(msg);
                            }
                        }
                    }
                    WalletScreen::Receive => {
                        match render_receive(
                            ui,
                            &mut self.receive,
                            overview.address,
                            testnet,
                            palette,
                        ) {
                            ReceiveOutcome::None => {}
                            ReceiveOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            ReceiveOutcome::Toast(msg) => {
                                self.toast = Some(msg);
                            }
                        }
                    }
                    WalletScreen::TonConnect => {
                        match render_ton_connect(
                            ui,
                            &mut self.ton_connect,
                            self.backend.as_mut(),
                            palette,
                        ) {
                            TonConnectOutcome::None => {}
                            TonConnectOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            TonConnectOutcome::Toast(msg) => {
                                self.toast = Some(msg);
                            }
                        }
                    }
                    WalletScreen::Collectibles => {
                        match render_collectibles(
                            ui,
                            &mut self.collectibles,
                            self.backend.as_mut(),
                            testnet,
                            palette,
                        ) {
                            CollectiblesOutcome::None => {}
                            CollectiblesOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            CollectiblesOutcome::Toast(msg) => {
                                self.toast = Some(msg);
                            }
                        }
                    }
                    WalletScreen::Onramp => {
                        match render_onramp(
                            ui,
                            &mut self.onramp,
                            self.backend.as_mut(),
                            overview.address,
                            testnet,
                            palette,
                        ) {
                            OnrampOutcome::None => {}
                            OnrampOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            OnrampOutcome::Toast(msg) => {
                                self.toast = Some(msg);
                            }
                            OnrampOutcome::OpenExternalUrl(url) => {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                                self.toast = Some(format!("Opened read-only handoff: {url}"));
                            }
                        }
                    }
                    WalletScreen::Backup => {
                        match render_backup(ui, &mut self.backup, self.backend.as_mut(), palette) {
                            BackupOutcome::None => {}
                            BackupOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            BackupOutcome::Imported(msg) => {
                                self.toast = Some(msg);
                                self.screen = WalletScreen::Overview;
                            }
                            BackupOutcome::Toast(msg) => {
                                self.toast = Some(msg);
                            }
                        }
                    }
                    WalletScreen::CardEditor => {
                        match render_card_editor(
                            ui,
                            &mut self.card_editor,
                            self.backend.as_mut(),
                            &overview,
                            testnet,
                            palette,
                        ) {
                            CardEditorOutcome::None => {}
                            CardEditorOutcome::Back => {
                                self.screen = WalletScreen::Overview;
                            }
                            CardEditorOutcome::Saved(msg) => {
                                self.toast = Some(msg);
                                self.screen = WalletScreen::Overview;
                            }
                        }
                    }
                }

                if let Some(msg) = self.toast.clone() {
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(10, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&msg)
                                        .size(12.0)
                                        .color(palette.window_active_text_fg),
                                );
                                if ui.small_button("×").clicked() {
                                    self.toast = None;
                                }
                            });
                        });
                }
            });

        close_clicked
    }
}

/// Syncs egui's scoped widget visuals (`TextEdit`, `Button`, selection) to the active tdesktop
/// [`Palette`] so Day and Night themes render with consistent contrast and surfaces.
fn apply_wallet_visuals(ui: &mut Ui, palette: &Palette) {
    let luma = u16::from(palette.window_bg.r())
        + u16::from(palette.window_bg.g())
        + u16::from(palette.window_bg.b());
    let mut visuals = if luma < 384 {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.override_text_color = Some(palette.window_fg);
    visuals.panel_fill = palette.window_bg;
    visuals.window_fill = palette.window_bg;
    visuals.extreme_bg_color = palette.window_bg;
    visuals.faint_bg_color = palette.window_bg_over;
    visuals.widgets.noninteractive.bg_fill = palette.window_bg;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.divider);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.window_fg);
    visuals.widgets.inactive.bg_fill = palette.window_bg_over;
    visuals.widgets.inactive.weak_bg_fill = palette.window_bg_over;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.divider);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.window_fg);
    visuals.widgets.hovered.bg_fill = palette.dialogs_bg_active;
    visuals.widgets.hovered.weak_bg_fill = palette.dialogs_bg_active;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.window_bg_active);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.window_bold_fg);
    visuals.widgets.active.bg_fill = palette.window_bg_active;
    visuals.widgets.active.weak_bg_fill = palette.window_bg_active;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.window_bg_active);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.window_fg_active);
    visuals.selection.bg_fill = palette.window_bg_active;
    visuals.selection.stroke = Stroke::new(1.0, palette.window_fg_active);
    ui.style_mut().visuals = visuals;
}

/// Self-contained overlay host for `app.rs`, `live/host.rs`, `window/main_menu.rs`, and
/// `settings/main_page.rs` (matching `AppPaymentsHost` / `AppBusinessHost`).
///
/// Press `Ctrl+Shift+W` or click the **Wallet** entry in the Main Menu or Settings to open.
pub struct AppWalletHost {
    open: bool,
    panel: WalletPanel,
    account_panels: BTreeMap<i32, WalletPanel>,
    active_slot: Option<i32>,
}

impl AppWalletHost {
    fn state_arc(ctx: &Context) -> Arc<Mutex<Self>> {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Id::unique("tg_app_wallet_host_v1"), || {
                Arc::new(Mutex::new(Self {
                    open: false,
                    panel: WalletPanel::with_mock_demo(),
                    account_panels: BTreeMap::new(),
                    active_slot: None,
                }))
            })
            .clone()
        })
    }

    fn active_panel(&self) -> &WalletPanel {
        if let Some(slot) = self.active_slot
            && let Some(panel) = self.account_panels.get(&slot)
        {
            return panel;
        }
        &self.panel
    }

    fn active_panel_mut(&mut self) -> &mut WalletPanel {
        if let Some(slot) = self.active_slot
            && let Some(panel) = self.account_panels.get_mut(&slot)
        {
            return panel;
        }
        &mut self.panel
    }

    /// Replaces the single active wallet backend (used when running single-account or in tests).
    pub fn set_backend(ctx: &Context, backend: Box<dyn WalletBackend>) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock() {
            guard.account_panels.clear();
            guard.active_slot = None;
            guard.panel = WalletPanel::new(backend);
            guard.open = false;
        }
    }

    /// Registers (or replaces) a per-account live [`WalletBackend`] for `account_slot`.
    pub fn set_account_backend(
        ctx: &Context,
        account_slot: i32,
        backend: Box<dyn WalletBackend>,
        make_active: bool,
    ) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock() {
            guard
                .account_panels
                .insert(account_slot, WalletPanel::new(backend));
            if make_active || guard.active_slot.is_none() {
                guard.active_slot = Some(account_slot);
            }
        }
    }

    /// Returns `true` if a per-account backend has been installed for `account_slot`.
    #[must_use]
    pub fn has_account_backend(ctx: &Context, account_slot: i32) -> bool {
        let arc = Self::state_arc(ctx);
        arc.lock()
            .map(|g| g.account_panels.contains_key(&account_slot))
            .unwrap_or(false)
    }

    /// Switches the active live wallet panel to `account_slot`.
    pub fn activate_account(ctx: &Context, account_slot: i32) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock()
            && guard.account_panels.contains_key(&account_slot)
            && guard.active_slot != Some(account_slot)
        {
            guard.active_slot = Some(account_slot);
            guard.open = false;
        }
    }

    /// Removes the live wallet panel for `account_slot` (e.g. on account logout).
    pub fn remove_account(ctx: &Context, account_slot: i32) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock() {
            guard.account_panels.remove(&account_slot);
            if guard.active_slot == Some(account_slot) {
                guard.active_slot = guard.account_panels.keys().next().copied();
                guard.open = false;
            }
        }
    }

    /// Retains only the specified live account slots, dropping logged-out accounts.
    pub fn retain_accounts(ctx: &Context, active_slots: &[i32]) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock() {
            guard
                .account_panels
                .retain(|slot, _| active_slots.contains(slot));
            if let Some(current) = guard.active_slot
                && !guard.account_panels.contains_key(&current)
            {
                guard.active_slot = guard.account_panels.keys().next().copied();
                guard.open = false;
            }
        }
    }

    /// Runs `f` against the active [`WalletBackend`] (for tests and live sync).
    pub fn with_active_backend_mut<R>(
        ctx: &Context,
        f: impl FnOnce(&mut dyn WalletBackend) -> R,
    ) -> Option<R> {
        let arc = Self::state_arc(ctx);
        let mut guard = arc.lock().ok()?;
        Some(f(guard.active_panel_mut().backend_mut()))
    }

    /// Runs `f` against the active [`WalletPanel`] (for tests and deep-link verification).
    pub fn with_active_panel<R>(ctx: &Context, f: impl FnOnce(&WalletPanel) -> R) -> Option<R> {
        let arc = Self::state_arc(ctx);
        let guard = arc.lock().ok()?;
        Some(f(guard.active_panel()))
    }

    /// Returns whether the Wallet overlay is currently open.
    #[must_use]
    pub fn is_open(ctx: &Context) -> bool {
        let arc = Self::state_arc(ctx);
        arc.lock().map(|g| g.open).unwrap_or(false)
    }

    /// Opens the Wallet overlay panel if allowed by [`WalletAppConfig::is_available`].
    pub fn open(ctx: &Context) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock() {
            guard.active_panel_mut().backend_mut().poll_background();
            if guard.active_panel().is_available() {
                guard.open = true;
                ctx.request_repaint();
            }
        }
    }

    /// Routes a [`WalletDeepLinkAction`] (`tc://`, `tg://sendgrams`, `ton://transfer`) into the
    /// active [`WalletPanel`] and opens the overlay on success.
    pub fn handle_deep_link_action(
        ctx: &Context,
        action: &WalletDeepLinkAction,
    ) -> Result<(), String> {
        let arc = Self::state_arc(ctx);
        let mut guard = arc
            .lock()
            .map_err(|_| "Wallet host lock poisoned".to_owned())?;
        guard.active_panel_mut().backend_mut().poll_background();
        guard.active_panel_mut().handle_deep_link_action(action)?;
        guard.open = true;
        ctx.request_repaint();
        Ok(())
    }

    /// Updates app-config gating (`wallet_available` / `support_mode`) on the active host panel.
    pub fn set_app_config(ctx: &Context, config: WalletAppConfig) {
        let arc = Self::state_arc(ctx);
        if let Ok(mut guard) = arc.lock() {
            if !config.is_available() {
                guard.open = false;
            }
            guard.active_panel_mut().set_app_config(config);
        }
    }

    /// Returns whether the Wallet entry should be displayed according to `Wallet::Available`.
    #[must_use]
    pub fn is_available(ctx: &Context) -> bool {
        let arc = Self::state_arc(ctx);
        arc.lock()
            .map(|mut g| {
                g.active_panel_mut().backend_mut().poll_background();
                g.active_panel().is_available()
            })
            .unwrap_or(false)
    }

    /// Single-line registration helper for `window/main_menu.rs` (`MainMenu::setupMenu`).
    ///
    /// Renders the `"Wallet"` drawer row when `Wallet::Available` is true, and opens the wallet
    /// overlay when clicked (returning `true` so the drawer can close cleanly).
    pub fn render_main_menu_entry(ui: &mut Ui, drawer_width: f32, palette: &Palette) -> bool {
        if !Self::is_available(ui.ctx()) {
            return false;
        }
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(drawer_width, 40.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let badge_center = Pos2::new(rect.left() + 32.0, rect.center().y);
        ui.painter()
            .circle_filled(badge_center, 11.0, palette.window_bg_active);
        ui.painter().text(
            badge_center,
            egui::Align2::CENTER_CENTER,
            "WL",
            egui::FontId::proportional(9.5),
            palette.window_fg_active,
        );
        ui.painter().text(
            Pos2::new(rect.left() + 58.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Wallet",
            egui::FontId::proportional(14.0),
            palette.window_fg,
        );
        if resp.clicked() {
            Self::open(ui.ctx());
            return true;
        }
        false
    }

    /// Single-line registration helper for `settings/main_page.rs` (`SetupSections`).
    ///
    /// Renders a `"TON Wallet"` row in Settings when `Wallet::Available` is true.
    pub fn render_settings_entry(ui: &mut Ui, colors: &dyn Colors) {
        if !Self::is_available(ui.ctx()) {
            return;
        }
        let avail_w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 42.0), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let badge_center = Pos2::new(rect.left() + 34.0, rect.center().y);
        ui.painter().rect_filled(
            Rect::from_center_size(badge_center, Vec2::splat(26.0)),
            CornerRadius::same(7),
            colors.color("windowBgActive"),
        );
        ui.painter().text(
            badge_center,
            egui::Align2::CENTER_CENTER,
            "TON",
            egui::FontId::proportional(9.0),
            colors.color("windowFgActive"),
        );
        ui.painter().text(
            Pos2::new(rect.left() + 62.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Wallet",
            egui::FontId::proportional(14.0),
            colors.color("windowFg"),
        );
        if resp.clicked() {
            Self::open(ui.ctx());
        }
    }

    /// Drives keyboard shortcut (`Ctrl+Shift+W`) and renders the floating Wallet overlay when open.
    pub fn update_and_render(ctx: &Context, palette: &Palette, _theme_mode: ThemeMode) {
        let toggle_pressed = ctx.input(|i| {
            i.modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT)
                && i.key_pressed(Key::W)
        });

        let state_arc = Self::state_arc(ctx);
        let Ok(mut guard) = state_arc.lock() else {
            return;
        };
        guard.active_panel_mut().backend_mut().poll_background();
        if !guard.active_panel().is_available() {
            guard.open = false;
            return;
        }
        if toggle_pressed {
            guard.open = !guard.open;
        }
        if !guard.open {
            return;
        }

        let screen_rect = ctx.content_rect();
        let panel_w = (screen_rect.width() * 0.56).clamp(420.0, 560.0);
        let panel_h = (screen_rect.height() - 56.0).clamp(420.0, 680.0);
        let panel_pos = Pos2::new(
            screen_rect.center().x - panel_w / 2.0,
            screen_rect.top() + 28.0,
        );

        let mut close_requested = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Area::new(Id::unique("tg_wallet_overlay_backdrop"))
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

        egui::Area::new(Id::unique("tg_wallet_overlay_panel"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter().rect_filled(
                    panel_rect.expand(1.0),
                    CornerRadius::same(11),
                    palette.divider,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    if guard.active_panel_mut().show(ui, palette, true) {
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
    use egui::{RawInput, Rect, Vec2};
    use tg_wallet::address::TonAddress;
    use tg_wallet::contract::WalletVersion;
    use tg_wallet::tonconnect::TonConnectKeypair;

    use super::mock::{DEMO_WALLET_PASSCODE, MockWalletBackend};
    use super::*;

    fn run_frame(ctx: &Context, mut f: impl FnMut(&mut Ui)) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| f(ui));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn create_import_lock_unlock_send_and_ton_connect_backend_flows() {
        let mut backend = MockWalletBackend::empty();
        assert!(!backend.overview().has_wallet);

        // Create wallet with fresh generated 24-word mnemonic & passcode.
        let phrase = backend.generate_phrase().unwrap();
        assert_eq!(phrase.len(), 24);
        let addr = backend
            .create_wallet(&phrase, WalletVersion::V4R2, "pass1234")
            .unwrap();
        assert_eq!(backend.overview().address, Some(addr));
        assert!(!backend.overview().locked);
        assert_eq!(backend.reveal_phrase().unwrap(), phrase);

        // Lock & unlock.
        backend.lock_vault();
        assert!(backend.overview().locked);
        assert!(backend.unlock_vault("wrong").is_err());
        backend.unlock_vault("pass1234").unwrap();
        assert!(!backend.overview().locked);

        // Prepare & confirm send.
        let recipient = TonAddress::new(0, [0x99; 32]).to_friendly(false, false);
        let quote = backend
            .prepare_send(&recipient, 1_000_000_000, "Unit test transfer", Some(false))
            .unwrap();
        let receipt = backend.confirm_send(&quote).unwrap();
        assert!(!receipt.message_hash_hex.is_empty());
        assert!(receipt.signed_boc_base64.starts_with("te6cc"));
        assert_eq!(backend.overview().history.len(), 1);
        assert_eq!(backend.overview().history[0].comment, "Unit test transfer");

        // TON Connect link -> approve -> disconnect.
        let dapp_kp = TonConnectKeypair::generate();
        let link = format!(
            "tc://?v=2&id={}&r=%7B%22manifestUrl%22%3A%22https%3A%2F%2Fdemo.example%2Fmanifest.json%22%2C%22items%22%3A%5B%7B%22name%22%3A%22ton_addr%22%7D%2C%7B%22name%22%3A%22ton_proof%22%2C%22payload%22%3A%22hello%22%7D%5D%7D",
            dapp_kp.client_id()
        );
        backend.handle_ton_connect_link(&link).unwrap();
        assert!(backend.pending_connect().is_some());
        let session_card = backend.approve_ton_connect().unwrap();
        assert_eq!(backend.ton_connect_sessions().len(), 1);
        backend
            .disconnect_ton_connect(session_card.session_id)
            .unwrap();
        assert!(backend.ton_connect_sessions().is_empty());
    }

    #[test]
    fn demo_wallet_panel_renders_all_screens_and_respects_feature_gating() {
        let ctx = Context::default();
        let palette = Palette::day();
        let mut backend = MockWalletBackend::with_demo_wallet();
        backend.queue_demo_connect_approval();
        let mut panel = WalletPanel::new(Box::new(backend));

        for screen in [
            WalletScreen::Overview,
            WalletScreen::Send,
            WalletScreen::Receive,
            WalletScreen::TonConnect,
            WalletScreen::Onboarding,
        ] {
            panel.select_screen(screen);
            run_frame(&ctx, |ui| {
                let _ = panel.show(ui, &palette, true);
            });
        }

        // Approve pending TON Connect tx in demo wallet.
        let receipt = panel.backend_mut().approve_ton_connect_tx().unwrap();
        assert!(!receipt.signed_boc_base64.is_empty());

        // Lock & unlock with DEMO_WALLET_PASSCODE.
        panel.backend_mut().lock_vault();
        assert!(panel.backend_mut().overview().locked);
        panel
            .backend_mut()
            .unlock_vault(DEMO_WALLET_PASSCODE)
            .unwrap();
        assert!(!panel.backend_mut().overview().locked);

        // AppConfig feature-flag gating (`wallet_available = false`).
        let cfg = WalletAppConfig {
            wallet_available: false,
            ..WalletAppConfig::default()
        };
        AppWalletHost::set_app_config(&ctx, cfg);
        assert!(!AppWalletHost::is_available(&ctx));

        let cfg_on = WalletAppConfig {
            wallet_available: true,
            ..WalletAppConfig::default()
        };
        AppWalletHost::set_app_config(&ctx, cfg_on);
        assert!(AppWalletHost::is_available(&ctx));

        // Open overlay and render via AppWalletHost::update_and_render + main menu & settings hooks.
        AppWalletHost::open(&ctx);
        let live_colors = crate::settings::theme_bridge::LiveThemeColors::default();
        run_frame(&ctx, |ui| {
            let _ = AppWalletHost::render_main_menu_entry(ui, 274.0, &palette);
            AppWalletHost::render_settings_entry(ui, &live_colors);
            AppWalletHost::update_and_render(ui.ctx(), &palette, ThemeMode::Day);
        });
    }

    #[test]
    fn send_transfer_link_and_receive_qr_render() {
        let ctx = Context::default();
        let addr = TonAddress::new(0, [0x44; 32]);
        let uri = tg_wallet::address::build_transfer_link(
            &addr,
            false,
            false,
            Some(2_500_000_000),
            Some("Demo QR"),
        );

        let mut send_state = send::SendState::default();
        assert!(send_state.try_apply_transfer_link(&uri));
        assert_eq!(send_state.amount_input, "2.5");
        assert_eq!(send_state.comment_input, "Demo QR");

        let mut qr_ok = false;
        run_frame(&ctx, |ui| {
            qr_ok = receive::paint_qr_code(ui, &uri, 180.0, Palette::day().divider);
        });
        assert!(qr_ok);
    }
}
