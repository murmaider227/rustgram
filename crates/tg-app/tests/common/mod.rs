//! Shared headless `egui_kittest` harness helpers for end-to-end UI tests.
//!
//! Safety & determinism guarantees:
//! - Always sets `RUSTGRAM_HEADLESS_AUDIO=1` so no real hardware audio output is opened.
//! - Uses `tempfile::TempDir` for `ShellSettings` so `%APPDATA%` is never touched.
//! - Never constructs `RealTrayPlatform`, WinRT `ToastManager`, or Windows Registry / Start Menu shortcuts.
//! - Drives both `egui`'s simulated frame clock (`TEST_STEP_DT = 1/30 s`) and `tg_app::clock`'s
//!   virtual monotonic clock in lockstep on every frame so animations, debounces, and transitions
//!   are 100% independent of wall-clock time and CPU load.

#![allow(dead_code)]

use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2, ViewportId};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;
use tg_app::notifications::backend::NotifyBackend;
use tg_app::notifications::manager::{NotificationSystem, WindowFocusState};
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_app::stand_in::mock_session::{MockSession, SharedSessionHandle};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::webview::AppWebViewHost;
use tg_app::webview::backend::{ClipboardAccess, ExternalOpener};
use tg_app::window::controller::WindowController;
use tg_ui::fonts::register_tdesktop_fonts;

/// Fixed simulated per-frame time delta (`1/30 s` ≈ `33.33 ms`) used by all e2e harnesses.
pub const TEST_STEP_DT: f32 = 1.0 / 30.0;

/// Default maximum frame budget for condition-based waits (`120` frames = `4.0 s` simulated time).
pub const DEFAULT_MAX_FRAMES: usize = 120;

struct RecordingExternalOpener(Arc<Mutex<Vec<String>>>);

impl ExternalOpener for RecordingExternalOpener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        if let Ok(mut guard) = self.0.lock() {
            guard.push(url.to_owned());
        }
        Ok(())
    }
}

#[derive(Default)]
struct NoopClipboard;

impl ClipboardAccess for NoopClipboard {
    fn read_text(&self) -> Option<String> {
        None
    }

    fn write_text(&self, _text: &str) -> bool {
        true
    }
}

static INIT_ENV: Once = Once::new();

/// Ensures headless environment variables are set before constructing any app state.
pub fn init_headless_env() {
    INIT_ENV.call_once(|| {
        // SAFETY: Called once before spawning any test audio streams; prevents `cpal` hardware init.
        unsafe {
            std::env::set_var("RUSTGRAM_HEADLESS_AUDIO", "1");
        }
    });
}

/// Applies deterministic `egui` style knobs (zero built-in animation time, no cursor blink,
/// no scroll smoothing) so interactive and snapshot tests never vary with timing.
///
/// Must be re-applied after `ThemeManager::apply_to_egui`, which replaces `Visuals` with
/// `Visuals::light()` / `Visuals::dark()` (whose default sets `text_cursor.blink = true`).
pub fn apply_deterministic_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.visuals.text_cursor.blink = false;
        style.scroll_animation = egui::style::ScrollAnimation::none();
        style.animation_time = 0.0;
    });
}

/// Shared mutable state driven inside `egui_kittest::Harness`.
pub struct AppTestState {
    /// Live top-level window controller under test.
    pub controller: WindowController,
    /// Last synced `ThemeManager` generation (mirrors `TgforkApp::sync_theme`).
    last_theme_gen: u64,
    /// Last observed `egui` simulated time (seconds) used to advance `tg_app::clock` in lockstep.
    last_egui_time: f64,
    /// Keep the temporary directory alive for the duration of the test.
    pub _temp_dir: TempDir,
    /// Optional headless notification system wired to the same `MockSession`.
    pub notifications: Option<HeadlessNotifyState>,
    /// Optional headless `AppWebViewHost` wired to the same `MockSession`.
    pub webview_host: Option<AppWebViewHost>,
    /// Recorded external URLs opened via `ExternalOpener` when `webview_host` is attached.
    pub opened_external_urls: Arc<Mutex<Vec<String>>>,
}

/// Headless in-app notification state wired to `MockSession`'s `SharedSessionHandle`.
pub struct HeadlessNotifyState {
    pub system: NotificationSystem,
    pub backend: SharedSessionHandle,
    pub focus: WindowFocusState,
    pub now_ms: i64,
    pub now_unix: i64,
}

impl AppTestState {
    fn from_signed_in(signed_in: bool) -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();
        let temp_dir = TempDir::new().expect("create temp settings dir");
        let settings_path = temp_dir.path().join("shell_settings.json");
        let controller = WindowController::with_session(
            MockSession::new(signed_in),
            ShellSettings::default(),
            settings_path,
        );
        Self {
            controller,
            last_theme_gen: u64::MAX,
            last_egui_time: 0.0,
            _temp_dir: temp_dir,
            notifications: None,
            webview_host: None,
            opened_external_urls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Creates a signed-out state starting on `RootScreen::Intro`.
    pub fn intro() -> Self {
        Self::from_signed_in(false)
    }

    /// Creates a signed-in state starting on `RootScreen::Main`.
    pub fn main_workspace() -> Self {
        Self::from_signed_in(true)
    }

    /// Creates a signed-in state with a headless [`AppWebViewHost`] attached to the same `MockSession`.
    pub fn main_with_webview() -> Self {
        let mut state = Self::main_workspace();
        let handle = state.controller.session_handle();
        let opener = RecordingExternalOpener(Arc::clone(&state.opened_external_urls));
        state.webview_host = Some(AppWebViewHost::with_dependencies(
            Box::new(handle),
            Box::new(opener),
            Box::new(NoopClipboard),
        ));
        state
    }

    /// Creates a signed-in state with headless in-app custom notifications attached.
    pub fn main_with_notifications() -> Self {
        let mut state = Self::main_workspace();
        let handle = state.controller.session_handle();
        let userpic_dir = state._temp_dir.path().join("notify_userpics");
        let prefs = NotificationPreferences {
            force_custom_notifications: true,
            native_notifications: false,
            ..NotificationPreferences::default()
        };
        let store = NotifySettingsStore::default();
        let system = NotificationSystem::headless(prefs, store, userpic_dir);
        let mut focus = WindowFocusState::with_defaults(1);
        focus.window_focused = true;
        focus.active_peer_id = state.controller.active_focused_peer();
        state.notifications = Some(HeadlessNotifyState {
            system,
            backend: handle,
            focus,
            now_ms: 1_000,
            now_unix: 1_795_341_600,
        });
        state
    }

    /// Renders one full frame (virtual clock sync + theme sync + `WindowController::show` + popups).
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let egui_time = ctx.input(|i| i.time);
        let delta_secs = (egui_time - self.last_egui_time).max(0.0);
        self.last_egui_time = egui_time;
        if delta_secs > 0.0 {
            tg_app::clock::advance_test_now(Duration::from_secs_f64(delta_secs));
        }

        let current_gen = self.controller.theme_generation();
        if current_gen != self.last_theme_gen {
            self.controller.theme_manager().apply_to_egui(&ctx);
            apply_deterministic_style(&ctx);
            self.last_theme_gen = current_gen;
        }

        // Process any notification clicks queued on the previous frame (mirrors `TgforkApp::update`).
        self.controller.poll_notification_opens(Some(&ctx));

        let palette = self.controller.palette();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette.window_bg))
            .show(ui, |ui| {
                self.controller.show(ui);
            });

        if let Some(ref mut host) = self.webview_host {
            self.controller.dispatch_webview_requests_to(host);
        }

        if let Some(ref mut notify) = self.notifications {
            notify.focus.active_peer_id = self.controller.active_focused_peer();
            let pal = self.controller.palette();
            notify.system.tick(
                &mut notify.backend as &mut dyn NotifyBackend,
                &notify.focus,
                notify.now_ms,
                notify.now_unix,
            );
            let _ = notify.system.show_custom_popups_egui(
                &ctx,
                &pal,
                &mut notify.backend as &mut dyn NotifyBackend,
                &notify.focus,
                notify.now_ms,
            );
        }
    }
}

/// Builds a headless `egui_kittest::Harness` configured with tdesktop bundled fonts and
/// deterministic simulated frame timing (`TEST_STEP_DT`).
pub fn build_harness(state: AppTestState, size: Vec2) -> Harness<'static, AppTestState> {
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut AppTestState| {
                st.show(ui);
            },
            state,
        )
}

/// Builds a standalone widget `egui_kittest::Harness` with tdesktop fonts and deterministic
/// simulated frame timing (`TEST_STEP_DT`) synced to `tg_app::clock`.
pub fn build_widget_harness<S>(
    state: S,
    size: Vec2,
    mut render: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) -> Harness<'static, S> {
    init_headless_env();
    tg_app::clock::init_test_clock();
    let mut last_egui_time = 0.0_f64;
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            move |ui, st: &mut S| {
                let egui_time = ui.ctx().input(|i| i.time);
                let delta_secs = (egui_time - last_egui_time).max(0.0);
                last_egui_time = egui_time;
                if delta_secs > 0.0 {
                    tg_app::clock::advance_test_now(Duration::from_secs_f64(delta_secs));
                }
                render(ui, st);
            },
            state,
        )
}

/// Builds a headless `egui_kittest::Harness` with `wgpu` rendering enabled for pixel snapshots.
pub fn build_wgpu_harness(state: AppTestState, size: Vec2) -> Harness<'static, AppTestState> {
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut AppTestState| {
                st.show(ui);
            },
            state,
        )
}

/// Steps `count` frames cleanly (avoids `Harness::run()` panicking on continuous repaint timers).
pub fn step_frames<S>(harness: &mut Harness<'_, S>, count: usize) {
    for _ in 0..count {
        harness.step();
    }
}

/// Returns `true` when the root viewport has no immediate (`Duration::ZERO`) repaint pending.
pub fn is_repaint_settled<S>(harness: &Harness<'_, S>) -> bool {
    harness
        .output()
        .viewport_output
        .get(&ViewportId::ROOT)
        .is_none_or(|vp| vp.repaint_delay != Duration::ZERO)
}

/// Steps the harness one frame at a time until `predicate(harness)` returns `true` or
/// `max_frames` frames have elapsed.
///
/// Always steps at least once so queued events (such as `node.click()`) are processed before
/// evaluating `predicate`.
pub fn run_until<S>(
    harness: &mut Harness<'_, S>,
    max_frames: usize,
    mut predicate: impl FnMut(&mut Harness<'_, S>) -> bool,
) -> bool {
    for _ in 0..max_frames.max(1) {
        harness.step();
        if predicate(harness) {
            return true;
        }
    }
    false
}

/// Steps until `predicate(harness)` returns `true` (up to `max_frames`), panicking with `context`
/// if the condition is not satisfied within the budget.
#[track_caller]
pub fn wait_until<S>(
    harness: &mut Harness<'_, S>,
    max_frames: usize,
    context: &str,
    predicate: impl FnMut(&mut Harness<'_, S>) -> bool,
) {
    assert!(
        run_until(harness, max_frames, predicate),
        "condition not met within {max_frames} frames: {context}"
    );
}

/// Steps until all `WindowController` transition animations (`IntroWidget`, `SessionController`
/// slides, `BoxHost` modal enters/fades, `TabbedPanel` slides) have finished.
#[track_caller]
pub fn settle_animations(harness: &mut Harness<'_, AppTestState>) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "WindowController animations did not finish",
        |h| !h.state().controller.is_animating(),
    );
    // One extra pass ensures AccessKit node rectangles reflect the final settled geometry.
    harness.step();
}

/// Steps until all `WindowController` animations have finished and no immediate repaint is
/// pending (e.g. initial chat relayout, photo blur-up, or popup reveal).
#[track_caller]
pub fn settle_layout(harness: &mut Harness<'_, AppTestState>) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "layout and animations did not settle",
        |h| !h.state().controller.is_animating() && is_repaint_settled(h),
    );
}

/// Steps until an AccessKit node with exact `label` appears.
#[track_caller]
pub fn wait_for_label<S>(harness: &mut Harness<'_, S>, label: &str) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        &format!("label {label:?} did not appear"),
        |h| h.query_by_label(label).is_some(),
    );
}

/// Steps until an AccessKit node containing `substr` appears.
#[track_caller]
pub fn wait_for_label_contains<S>(harness: &mut Harness<'_, S>, substr: &str) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        &format!("label containing {substr:?} did not appear"),
        |h| h.query_by_label_contains(substr).is_some(),
    );
}

/// Steps until no AccessKit node with exact `label` remains present.
#[track_caller]
pub fn wait_for_label_gone<S>(harness: &mut Harness<'_, S>, label: &str) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        &format!("label {label:?} did not disappear"),
        |h| h.query_by_label(label).is_none(),
    );
}

/// Steps until `WindowController` animations finish AND an AccessKit node with exact `label`
/// is present with a stable rectangle across two consecutive frames.
#[track_caller]
pub fn wait_for_label_settled(harness: &mut Harness<'_, AppTestState>, label: &str) {
    let mut prev_rect: Option<Rect> = None;
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        &format!("settled label {label:?} did not appear"),
        |h| {
            if h.state().controller.is_animating() {
                prev_rect = None;
                return false;
            }
            let Some(node) = h.query_by_label(label) else {
                prev_rect = None;
                return false;
            };
            let rect = node.rect();
            let stable = prev_rect.is_some_and(|p| p == rect) && rect.is_positive();
            prev_rect = Some(rect);
            stable
        },
    );
}

/// Steps until `WindowController` animations finish AND an AccessKit node containing `substr`
/// is present with a stable rectangle across two consecutive frames.
#[track_caller]
pub fn wait_for_label_contains_settled(harness: &mut Harness<'_, AppTestState>, substr: &str) {
    let mut prev_rect: Option<Rect> = None;
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        &format!("settled label containing {substr:?} did not appear"),
        |h| {
            if h.state().controller.is_animating() {
                prev_rect = None;
                return false;
            }
            let Some(node) = h.query_by_label_contains(substr) else {
                prev_rect = None;
                return false;
            };
            let rect = node.rect();
            let stable = prev_rect.is_some_and(|p| p == rect) && rect.is_positive();
            prev_rect = Some(rect);
            stable
        },
    );
}

/// Steps until a popup menu item with `label` is visible and its reveal animation has settled
/// (`is_repaint_settled` is true), guaranteeing the item lies within the menu's clip rect.
#[track_caller]
pub fn wait_for_menu_item<S>(harness: &mut Harness<'_, S>, label: &str) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        &format!("popup menu item {label:?} did not finish revealing"),
        |h| h.query_by_label(label).is_some() && is_repaint_settled(h),
    );
}

/// Sends a real left-click (move + press + release) at logical point `pos`.
pub fn click_at<S>(harness: &mut Harness<'_, S>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
}

/// Sends a real right-click (secondary button press + release) at `pos`.
pub fn right_click_at<S>(harness: &mut Harness<'_, S>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
}

/// Moves the pointer to `pos` and steps a frame.
pub fn hover_at<S>(harness: &mut Harness<'_, S>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
}

/// Presses and releases a key with modifiers, then steps a frame.
pub fn press_key<S>(harness: &mut Harness<'_, S>, modifiers: Modifiers, key: Key) {
    harness.key_press_modifiers(modifiers, key);
    harness.step();
}

/// Types literal text via `Event::Text` and steps a frame.
pub fn type_text<S>(harness: &mut Harness<'_, S>, text: &str) {
    harness
        .input_mut()
        .events
        .push(Event::Text(text.to_owned()));
    harness.step();
}

/// Computes relative luminance `[0.0, 255.0]` of an `egui::Color32` for Day/Night assertions.
pub fn color_luminance(c: egui::Color32) -> f32 {
    0.299 * f32::from(c.r()) + 0.587 * f32::from(c.g()) + 0.114 * f32::from(c.b())
}
