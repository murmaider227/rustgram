//! Connection state indicator state machine and pill renderer.
//!
//! Port origin: `Telegram/SourceFiles/window/window_connecting_widget.h`
//! and `Telegram/SourceFiles/window/window_connecting_widget.cpp`.

use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui};

use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::ConnectionStatus;

/// Ignore initial connecting banner during the first 3 seconds (`kIgnoreStartConnectingFor`).
pub const IGNORE_START_CONNECTING_FOR: Duration = Duration::from_millis(3000);
/// Delay before showing "Connecting" after dropping from Connected (`kConnectingStateDelay`).
pub const CONNECTING_STATE_DELAY: Duration = Duration::from_millis(1000);
/// Minimum waiting duration before switching from Connecting to Waiting (`kMinimalWaitingStateDuration`).
pub const MINIMAL_WAITING_STATE_DURATION_MS: u32 = 4000;

/// Simplified internal connection phase (`Window::ConnectionState::State::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionPhase {
    /// Connected cleanly.
    #[default]
    Connected,
    /// Actively connecting / handshaking.
    Connecting,
    /// Waiting `wait_till_retry` seconds before reconnecting.
    Waiting {
        /// Countdown seconds displayed to the user (`>= 1`).
        wait_till_retry: u32,
    },
}

/// Computed layout for the bottom-left connection pill (`Window::ConnectionState::Layout`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectionPillLayout {
    /// Whether the pill should be shown at all.
    pub visible: bool,
    /// Whether to display the clickable "Try now" link.
    pub has_retry: bool,
    /// Whether the proxy indicator is active.
    pub proxy_enabled: bool,
    /// Whether the spinning radial progress indicator should be shown.
    pub progress_shown: bool,
    /// Display text ("Connecting..." on hover, or "Reconnecting in Xs...").
    pub text: String,
}

/// Actions emitted when the user interacts with the connection indicator pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionWidgetAction {
    /// User clicked the pill body to open proxy/connection settings.
    OpenProxySettings,
    /// User clicked the "Try now" retry button.
    RetryNow,
}

/// Stateful connection indicator controller mirroring `Window::ConnectionState`.
#[derive(Debug)]
pub struct ConnectionIndicator {
    app_started_at: Instant,
    connecting_started_at: Option<Instant>,
    phase: ConnectionPhase,
    proxy_enabled: bool,
    under_cursor: bool,
    force_hidden: bool,
}

impl Default for ConnectionIndicator {
    fn default() -> Self {
        Self::new(tg_app_core::clock::now())
    }
}

impl ConnectionIndicator {
    /// Creates a new `ConnectionIndicator` anchored to `app_started_at`.
    #[must_use]
    pub const fn new(app_started_at: Instant) -> Self {
        Self {
            app_started_at,
            connecting_started_at: None,
            phase: ConnectionPhase::Connected,
            proxy_enabled: false,
            under_cursor: false,
            force_hidden: false,
        }
    }

    /// Forces the indicator hidden (e.g. during full-screen slide animations or lock screen).
    pub fn set_force_hidden(&mut self, hidden: bool) {
        self.force_hidden = hidden;
    }

    /// Updates hover state (in tdesktop, hovering a Connecting pill reveals "Connecting...").
    pub fn set_under_cursor(&mut self, under: bool) {
        self.under_cursor = under;
    }

    /// Returns the currently applied phase.
    #[must_use]
    pub const fn phase(&self) -> ConnectionPhase {
        self.phase
    }

    /// Feeds raw backend connection status and proxy flag into the tdesktop delay state machine.
    ///
    /// Mirrors `ConnectionState::refreshState()` in `window_connecting_widget.cpp:316-362`.
    pub fn update(
        &mut self,
        now: Instant,
        status: ConnectionStatus,
        proxy_enabled: bool,
        strings: &dyn Strings,
    ) -> ConnectionPillLayout {
        self.proxy_enabled = proxy_enabled;

        let target_phase = match status {
            ConnectionStatus::Connected => ConnectionPhase::Connected,
            ConnectionStatus::Connecting => ConnectionPhase::Connecting,
            ConnectionStatus::Waiting { remaining_ms } => {
                if remaining_ms < 600
                    || (remaining_ms <= MINIMAL_WAITING_STATE_DURATION_MS
                        && !matches!(self.phase, ConnectionPhase::Waiting { .. }))
                {
                    ConnectionPhase::Connecting
                } else {
                    ConnectionPhase::Waiting {
                        wait_till_retry: (remaining_ms / 1000) + 1,
                    }
                }
            }
        };

        if matches!(target_phase, ConnectionPhase::Connecting)
            && matches!(self.phase, ConnectionPhase::Connected)
        {
            let started = *self.connecting_started_at.get_or_insert(now);
            let apply_at = (started + CONNECTING_STATE_DELAY)
                .max(self.app_started_at + IGNORE_START_CONNECTING_FOR);
            if now >= apply_at {
                self.phase = ConnectionPhase::Connecting;
            }
        } else {
            if matches!(target_phase, ConnectionPhase::Connected) {
                self.connecting_started_at = None;
            }
            self.phase = target_phase;
        }

        self.compute_layout(strings)
    }

    /// Computes pill layout from current state (`ConnectionState::computeLayout`).
    #[must_use]
    pub fn compute_layout(&self, strings: &dyn Strings) -> ConnectionPillLayout {
        if self.force_hidden {
            return ConnectionPillLayout::default();
        }
        let progress_shown = !matches!(self.phase, ConnectionPhase::Connected);
        let visible = self.proxy_enabled || progress_shown;
        let (text, has_retry) = match self.phase {
            ConnectionPhase::Connected => (String::new(), false),
            ConnectionPhase::Connecting => {
                let text = if self.under_cursor {
                    strings.connecting().to_owned()
                } else {
                    String::new()
                };
                (text, false)
            }
            ConnectionPhase::Waiting { wait_till_retry } => {
                (strings.reconnecting_in(wait_till_retry), true)
            }
        };

        ConnectionPillLayout {
            visible,
            has_retry,
            proxy_enabled: self.proxy_enabled,
            progress_shown,
            text,
        }
    }

    /// Renders the floating bottom-left connection pill in `ui`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        strings: &dyn Strings,
        layout: &ConnectionPillLayout,
    ) -> Option<ConnectionWidgetAction> {
        if !layout.visible {
            self.under_cursor = false;
            return None;
        }

        let mut action = None;
        let frame_resp = Frame::NONE
            .fill(palette.window_bg)
            .stroke(Stroke::new(1.0, palette.divider))
            .corner_radius(CornerRadius::same(18))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if layout.progress_shown {
                        ui.add(
                            egui::Spinner::new()
                                .size(15.0)
                                .color(palette.window_active_text_fg),
                        );
                    }
                    let proxy_color = if layout.proxy_enabled {
                        palette.window_bg_active
                    } else {
                        palette.window_sub_text_fg
                    };
                    let shield_label = if layout.proxy_enabled { "PROXY" } else { "NET" };
                    let pill_btn = ui.add(
                        egui::Button::new(
                            RichText::new(shield_label)
                                .size(11.0)
                                .strong()
                                .color(proxy_color),
                        )
                        .frame(false),
                    );
                    if pill_btn.clicked() {
                        action = Some(ConnectionWidgetAction::OpenProxySettings);
                    }

                    if !layout.text.is_empty() {
                        ui.label(
                            RichText::new(&layout.text)
                                .size(12.5)
                                .color(palette.window_sub_text_fg),
                        );
                    }

                    if layout.has_retry {
                        let retry = ui.add(
                            egui::Button::new(
                                RichText::new(strings.try_now())
                                    .size(12.5)
                                    .strong()
                                    .color(palette.window_active_text_fg),
                            )
                            .frame(false),
                        );
                        if retry.clicked() {
                            action = Some(ConnectionWidgetAction::RetryNow);
                        }
                    }
                });
            });

        self.under_cursor = frame_resp.response.hovered();
        if action.is_none() && frame_resp.response.interact(egui::Sense::click()).clicked() {
            action = Some(ConnectionWidgetAction::OpenProxySettings);
        }
        action
    }
}

/// Convenience helper for `Color32` status dot in settings / menus.
#[must_use]
pub fn status_dot_color(status: ConnectionStatus, palette: &Palette) -> Color32 {
    match status {
        ConnectionStatus::Connected => Color32::from_rgb(0x4C, 0xAF, 0x50),
        ConnectionStatus::Connecting => palette.window_bg_active,
        ConnectionStatus::Waiting { .. } => palette.attention_fg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_app_core::stand_in::strings::EnglishStrings;

    #[test]
    fn connecting_indicator_respects_startup_and_transition_delays() {
        let t0 = Instant::now();
        let strings = EnglishStrings;
        let mut indicator = ConnectionIndicator::new(t0);

        // Connected without proxy -> hidden.
        let l0 = indicator.update(t0, ConnectionStatus::Connected, false, &strings);
        assert!(!l0.visible);

        // Dropping to Connecting right at startup is suppressed until 3000ms.
        let l1 = indicator.update(
            t0 + Duration::from_millis(500),
            ConnectionStatus::Connecting,
            false,
            &strings,
        );
        assert!(!l1.visible);

        // Still suppressed at 2999ms.
        let l2 = indicator.update(
            t0 + Duration::from_millis(2999),
            ConnectionStatus::Connecting,
            false,
            &strings,
        );
        assert!(!l2.visible);

        // Visible at 3000ms.
        let l3 = indicator.update(
            t0 + Duration::from_millis(3000),
            ConnectionStatus::Connecting,
            false,
            &strings,
        );
        assert!(l3.visible);
        assert!(l3.progress_shown);
        assert!(l3.text.is_empty());

        // Hover reveals "Connecting...".
        indicator.set_under_cursor(true);
        let l4 = indicator.update(
            t0 + Duration::from_millis(3100),
            ConnectionStatus::Connecting,
            false,
            &strings,
        );
        assert_eq!(l4.text, "Connecting...");
    }

    #[test]
    fn waiting_state_shows_countdown_and_retry_link() {
        let t0 = Instant::now();
        let strings = EnglishStrings;
        let mut indicator = ConnectionIndicator::new(t0);

        let l = indicator.update(
            t0 + Duration::from_secs(5),
            ConnectionStatus::Waiting { remaining_ms: 5200 },
            false,
            &strings,
        );
        assert!(l.visible);
        assert!(l.has_retry);
        assert_eq!(l.text, "Reconnecting in 6s...");
    }

    #[test]
    fn proxy_enabled_keeps_pill_visible_even_when_connected() {
        let t0 = Instant::now();
        let strings = EnglishStrings;
        let mut indicator = ConnectionIndicator::new(t0);

        let l = indicator.update(t0, ConnectionStatus::Connected, true, &strings);
        assert!(l.visible);
        assert!(l.proxy_enabled);
        assert!(!l.progress_shown);
    }
}
