//! Telegram account and chat data export UI ("Export Telegram data" & "Export chat history").
//!
//! Port origin:
//! - `Telegram/SourceFiles/export/export_manager.{h,cpp}` ([`manager`])
//! - `Telegram/SourceFiles/export/view/export_view_panel_controller.{h,cpp}` ([`manager`])
//! - `Telegram/SourceFiles/export/view/export_view_content.{h,cpp}` ([`content`])
//! - `Telegram/SourceFiles/export/view/export_view_settings.{h,cpp}` &
//!   `export_view_format_button.{h,cpp}` ([`settings_view`])
//! - `Telegram/SourceFiles/export/view/export_view_progress.{h,cpp}` ([`progress_view`])
//! - `Telegram/SourceFiles/export/view/export_view_top_bar.{h,cpp}` ([`top_bar`])
//! - Demo [`tg_export::InMemoryExportSource`] & [`tg_export::TlExportSource`] adapter ([`demo_source`])

pub mod content;
pub mod demo_source;
pub mod manager;
pub mod progress_view;
pub mod settings_view;
pub mod top_bar;

use std::sync::{Arc, Mutex};

use egui::{
    Align2, Area, Context, CornerRadius, FontId, Frame, Id, Key, Margin, Modifiers, Order, Pos2,
    Rect, Sense, Stroke, Ui, Vec2,
};
use tg_export::stand_in::InputPeer;
use tg_tl::types::InputPeerUser;
use tg_ui::style::{IconButtonStyle, RoundButtonStyle};
use tg_ui::theme::{Colors, ThemeManager};
use tg_ui::widgets::buttons::{IconButton, IconKind, RoundButton};

use tg_app_core::stand_in::theme::{Palette, ThemeMode};

pub use content::{
    Content, ContentRow, DONE_ID, content_from_finished, content_from_processing,
    content_from_state, format_download_text, format_size_text,
};
pub use demo_source::{
    PacedExportSource, SkipFileSignal, build_demo_export_source, peer_id_from_input_peer,
    sample_download_progress,
};
pub use manager::{
    DemoSourceBuilder, EXPORT_PANEL_HEIGHT, EXPORT_PANEL_WIDTH, ExportManager, ExportModal,
    ExportScreen, ExportTarget, FolderChooser, FolderOpener, LiveExportStarter,
    PlatformFolderOpener, QtFileSettingsStore, RfdFolderChooser, SAVE_SETTINGS_TIMEOUT,
    SettingsStore, TakeoutDelayInfo, current_unix_time, default_export_download_path,
    parse_takeout_init_delay, resolve_export_settings,
};
pub use progress_view::{
    PROGRESS_ANIM_DURATION_SECS, ProgressView, ProgressViewAction, SHOW_SKIP_FILE_TIMEOUT_SECS,
};
pub use settings_view::{
    DATE_LIMIT_OFFSET_SECS, DateLimitKind, DatePickerState, MEGABYTE, MIN_EXPORT_DATE,
    SIZE_VALUE_COUNT, SettingsView, SettingsViewAction, TimePickerState, apply_from_date,
    apply_from_time, apply_till_date, apply_till_time, can_start_export, civil_to_unix,
    days_in_month, display_export_path, format_date_long, format_date_time_full,
    format_short_label, format_size_limit_label, format_time_short, index_by_size_limit,
    is_default_export_path, is_leap_year, local_civil_to_unix, month_name, remove_time_of_day,
    should_show_media_section, size_limit_by_index, today_civil_date, weekday_mon0,
};
pub use top_bar::{ExportTopBar, TOP_BAR_HEIGHT};

/// Renders the complete Export panel frame (`SeparatePanel` header + active screen + child modal).
pub fn render_export_panel(
    ui: &mut Ui,
    manager: &mut ExportManager,
    palette: &Palette,
    colors: &dyn Colors,
    now_secs: f64,
) {
    let rect = ui.max_rect();
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), palette.window_bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, palette.divider),
        egui::StrokeKind::Inside,
    );

    // Header bar (`boxTitleHeight = 48px` with title & close button).
    let header_h = 48.0;
    let header_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), header_h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(header_rect), |ui| {
        ui.painter().text(
            Pos2::new(header_rect.min.x + 22.0, header_rect.center().y),
            Align2::LEFT_CENTER,
            manager.panel_title(),
            FontId::proportional(16.0),
            palette.window_bold_fg,
        );
        let close_st = IconButtonStyle::box_title_close(colors);
        let close_rect = Rect::from_min_size(
            Pos2::new(header_rect.max.x - close_st.width, header_rect.min.y),
            Vec2::new(close_st.width, header_h.min(close_st.height)),
        );
        let mut close_ui = ui.new_child(egui::UiBuilder::new().max_rect(close_rect));
        if IconButton::new(IconKind::Close, close_st)
            .show(&mut close_ui)
            .clicked()
        {
            manager.close_or_hide_panel();
        }
    });
    ui.painter().line_segment(
        [
            Pos2::new(header_rect.min.x, header_rect.max.y),
            Pos2::new(header_rect.max.x, header_rect.max.y),
        ],
        Stroke::new(1.0, palette.divider),
    );

    // Body area below header.
    let body_rect = Rect::from_min_max(Pos2::new(rect.min.x, header_rect.max.y), rect.max);
    let default_path = manager.default_download_path().to_owned();

    match manager.screen().clone() {
        ExportScreen::Settings => {
            let mut settings_copy = manager.settings().clone();
            let mut actions = Vec::new();
            ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                actions = manager.settings_view_mut().show(
                    ui,
                    &mut settings_copy,
                    &default_path,
                    palette,
                    colors,
                );
            });
            *manager.settings_mut() = settings_copy;
            if !actions.is_empty() {
                manager.handle_settings_actions(actions);
            }
        }
        ExportScreen::Progress => {
            let mut progress_action = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                progress_action = manager
                    .progress_view_mut()
                    .show(ui, palette, colors, now_secs);
            });
            if let Some(act) = progress_action {
                manager.handle_progress_action(act);
            }
        }
        ExportScreen::CriticalError(text) => {
            ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                Frame::NONE
                    .inner_margin(Margin {
                        left: 24,
                        right: 24,
                        top: 90,
                        bottom: 24,
                    })
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(text)
                                        .size(14.0)
                                        .color(palette.attention_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(24.0);
                            let btn_st = RoundButtonStyle::default_box(colors);
                            ui.horizontal(|ui| {
                                if RoundButton::new("Retry", btn_st).show(ui).clicked() {
                                    manager.retry_from_error();
                                }
                                ui.add_space(8.0);
                                if RoundButton::new("Close", btn_st).show(ui).clicked() {
                                    manager.stop();
                                }
                            });
                        });
                    });
            });
        }
    }
}

/// Renders any active child [`ExportModal`] centered over `bounds`.
pub fn render_export_modal(
    ctx: &Context,
    bounds: Rect,
    manager: &mut ExportManager,
    palette: &Palette,
    colors: &dyn Colors,
) {
    let Some(mut modal) = manager.modal().cloned() else {
        return;
    };

    // Dimming backdrop inside `bounds`.
    Area::new(Id::unique("tg_export_modal_backdrop"))
        .order(Order::Tooltip)
        .fixed_pos(bounds.min)
        .show(ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(bounds.size(), Sense::click());
            ui.painter()
                .rect_filled(r, CornerRadius::ZERO, palette.layer_backdrop);
        });

    let modal_w = match &modal {
        ExportModal::DatePicker { .. } => 316.0_f32,
        _ => 320.0_f32,
    }
    .min((bounds.width() - 24.0).max(240.0));

    let modal_pos = Pos2::new(
        bounds.center().x - modal_w * 0.5,
        (bounds.center().y - 140.0).max(bounds.min.y + 18.0),
    );

    let mut close_confirm_cancel = false;
    let mut confirm_stop = false;
    let mut date_result: Option<(DateLimitKind, Option<tg_export::stand_in::TimeId>)> = None;
    let mut time_result: Option<(DateLimitKind, Option<tg_export::stand_in::TimeId>)> = None;
    let mut dismiss_inform = false;
    let mut suggest_accept = false;
    let mut suggest_dismiss = false;

    Area::new(Id::unique("tg_export_modal_box"))
        .order(Order::Tooltip)
        .fixed_pos(modal_pos)
        .show(ctx, |ui| {
            Frame::NONE
                .fill(palette.window_bg)
                .stroke(Stroke::new(1.0, palette.divider))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.set_min_width(modal_w - 40.0);
                    ui.set_max_width(modal_w - 40.0);

                    match &mut modal {
                        ExportModal::ConfirmStop => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        "Are you sure you want to stop exporting your data?\n\n\
                                         If you do, you'll need to start over.",
                                    )
                                    .size(14.0)
                                    .color(palette.window_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(18.0);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let stop_st = RoundButtonStyle::attention_box(colors);
                                    if RoundButton::new("Stop", stop_st).show(ui).clicked() {
                                        confirm_stop = true;
                                    }
                                    ui.add_space(8.0);
                                    let cancel_st = RoundButtonStyle::default_box(colors);
                                    if RoundButton::new("Cancel", cancel_st).show(ui).clicked() {
                                        close_confirm_cancel = true;
                                    }
                                },
                            );
                        }
                        ExportModal::DatePicker { kind, state } => {
                            let title = match kind {
                                DateLimitKind::From => "Start date",
                                DateLimitKind::Till => "End date",
                            };
                            ui.label(
                                egui::RichText::new(title)
                                    .size(15.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(8.0);
                            if let Some(chosen) = state.show_body(ui, palette, colors) {
                                date_result = Some((*kind, chosen));
                            }
                        }
                        ExportModal::TimePicker { kind, state } => {
                            ui.label(
                                egui::RichText::new("Select time")
                                    .size(15.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(10.0);
                            if let Some(chosen) = state.show_body(ui, palette, colors) {
                                time_result = Some((*kind, chosen));
                            }
                        }
                        ExportModal::Inform { title, message, .. } => {
                            ui.label(
                                egui::RichText::new(title.as_str())
                                    .size(16.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(10.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(message.as_str())
                                        .size(13.5)
                                        .color(palette.window_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(16.0);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let ok_st = RoundButtonStyle::default_box(colors);
                                    if RoundButton::new("OK", ok_st).show(ui).clicked() {
                                        dismiss_inform = true;
                                    }
                                },
                            );
                        }
                        ExportModal::SuggestStart { .. } => {
                            ui.label(
                                egui::RichText::new("Data export ready")
                                    .size(16.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(10.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        "You can now download the data you requested. Start exporting data?",
                                    )
                                    .size(13.5)
                                    .color(palette.window_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(16.0);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let ok_st = RoundButtonStyle::default_box(colors);
                                    if RoundButton::new("OK", ok_st).show(ui).clicked() {
                                        suggest_accept = true;
                                    }
                                    ui.add_space(8.0);
                                    if RoundButton::new("Not now", ok_st).show(ui).clicked() {
                                        suggest_dismiss = true;
                                    }
                                },
                            );
                        }
                    }
                });
        });

    // Write back any interactive picker navigation edits, then apply terminal modal actions.
    if date_result.is_none()
        && time_result.is_none()
        && !close_confirm_cancel
        && !confirm_stop
        && !dismiss_inform
        && !suggest_accept
        && !suggest_dismiss
    {
        if let ExportModal::DatePicker { kind, state } = modal {
            manager.finish_date_picker_update(kind, state);
        } else if let ExportModal::TimePicker { kind, state } = modal {
            manager.finish_time_picker_update(kind, state);
        }
        return;
    }

    if confirm_stop {
        manager.confirm_stop();
    } else if close_confirm_cancel {
        manager.clear_modal();
    } else if let Some((kind, chosen)) = date_result {
        manager.finish_date_picker(kind, chosen);
    } else if let Some((kind, chosen)) = time_result {
        manager.finish_time_picker(kind, chosen);
    } else if dismiss_inform {
        manager.dismiss_inform_modal();
    } else if suggest_accept {
        manager.accept_suggest_start();
    } else if suggest_dismiss {
        manager.dismiss_suggest_start();
    }
}

impl ExportManager {
    pub(crate) fn finish_date_picker_update(
        &mut self,
        kind: DateLimitKind,
        state: DatePickerState,
    ) {
        if matches!(self.modal(), Some(ExportModal::DatePicker { .. })) {
            self.set_modal(Some(ExportModal::DatePicker { kind, state }));
        }
    }

    pub(crate) fn finish_time_picker_update(
        &mut self,
        kind: DateLimitKind,
        state: TimePickerState,
    ) {
        if matches!(self.modal(), Some(ExportModal::TimePicker { .. })) {
            self.set_modal(Some(ExportModal::TimePicker { kind, state }));
        }
    }

    /// Directly replaces the active modal dialog (internal helper for modal widgets & tests).
    pub fn set_modal(&mut self, modal: Option<ExportModal>) {
        if let Some(m) = modal {
            self.replace_modal_inner(Some(m));
        } else {
            self.replace_modal_inner(None);
        }
    }

    /// Clears any open modal dialog.
    pub fn clear_modal(&mut self) {
        self.replace_modal_inner(None);
    }
}

/// Self-contained overlay host for `app.rs` (single-line hook in `TgforkApp::ui`).
///
/// - Press `Ctrl+Shift+E` to open "Export Your Data" (full account export).
/// - Press `Ctrl+Shift+H` to open "Export chat history" (single-chat reduced settings).
/// - Also exposes static `egui::Context` entry points (`open_full_export`,
///   `open_single_chat_export`, `open_single_topic_export`) for Settings & Chat menu actions.
pub struct AppExportHost {
    manager: ExportManager,
    theme_manager: ThemeManager,
    last_theme_mode: ThemeMode,
    bound_session_key: Option<usize>,
}

impl AppExportHost {
    fn get_or_init(ctx: &Context, theme_mode: ThemeMode) -> Arc<Mutex<Self>> {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Id::unique("tg_app_export_host"), || {
                let mut theme_manager = ThemeManager::new();
                theme_manager.set_night_mode(theme_mode.is_night());
                let mut manager = ExportManager::new();
                manager.check_takeout_suggestion(current_unix_time());
                Arc::new(Mutex::new(Self {
                    manager,
                    theme_manager,
                    last_theme_mode: theme_mode,
                    bound_session_key: None,
                }))
            })
            .clone()
        })
    }

    /// Wires a live [`LiveExportStarter`] (backed by `TlExportSource<LiveExportRpc>`) and
    /// Tokio runtime handle into the context-scoped [`ExportManager`], replacing demo mode.
    pub fn set_live_export_starter(
        ctx: &Context,
        theme_mode: ThemeMode,
        starter: LiveExportStarter,
        tokio_handle: Option<tokio::runtime::Handle>,
    ) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            if let Some(handle) = tokio_handle {
                guard.manager.set_tokio_handle(handle);
            }
            guard.manager.set_live_export_starter(starter);
        }
    }

    /// Wires a live [`LiveExportStarter`] and records the owning session token so live-host
    /// account switches can detect and rebind seamlessly.
    pub fn set_live_export_starter_for_session(
        ctx: &Context,
        theme_mode: ThemeMode,
        session_key: usize,
        starter: LiveExportStarter,
        tokio_handle: Option<tokio::runtime::Handle>,
    ) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            if let Some(handle) = tokio_handle {
                guard.manager.set_tokio_handle(handle);
            }
            guard.manager.set_live_export_starter(starter);
            guard.bound_session_key = Some(session_key);
        }
    }

    /// Returns the currently bound live session token, if any.
    #[must_use]
    pub fn bound_session_key(ctx: &Context, theme_mode: ThemeMode) -> Option<usize> {
        let host = Self::get_or_init(ctx, theme_mode);
        host.lock().ok().and_then(|guard| guard.bound_session_key)
    }

    /// Returns `true` when a live [`LiveExportStarter`] is currently installed.
    #[must_use]
    pub fn is_live_mode(ctx: &Context, theme_mode: ThemeMode) -> bool {
        let host = Self::get_or_init(ctx, theme_mode);
        host.lock()
            .map(|guard| guard.manager.is_live_mode())
            .unwrap_or(false)
    }

    /// Returns `true` when the floating export settings/progress panel is currently visible.
    #[must_use]
    pub fn is_panel_visible(ctx: &Context, theme_mode: ThemeMode) -> bool {
        let host = Self::get_or_init(ctx, theme_mode);
        host.lock()
            .map(|guard| guard.manager.is_panel_visible())
            .unwrap_or(false)
    }

    /// Returns a snapshot of the active [`ExportTarget`] (`FullAccount`, `SingleChat`, `SingleTopic`).
    #[must_use]
    pub fn current_target(ctx: &Context, theme_mode: ThemeMode) -> Option<ExportTarget> {
        let host = Self::get_or_init(ctx, theme_mode);
        host.lock().ok().map(|guard| guard.manager.target().clone())
    }

    /// Clears the live export starter, reverting to demo mode (e.g. on logout or account switch).
    pub fn clear_live_export_starter(ctx: &Context, theme_mode: ThemeMode) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            guard.manager.clear_live_export_starter();
            guard.bound_session_key = None;
        }
    }

    /// Returns `true` if a background export is currently running.
    #[must_use]
    pub fn is_export_running(ctx: &Context, theme_mode: ThemeMode) -> bool {
        let host = Self::get_or_init(ctx, theme_mode);
        host.lock()
            .map(|guard| guard.manager.is_export_running())
            .unwrap_or(false)
    }

    /// Opens the stop-export confirmation modal (used before logout/quit while exporting).
    pub fn stop_with_confirmation(ctx: &Context, theme_mode: ThemeMode) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            guard.manager.stop_with_confirmation();
        }
    }

    /// Checks whether a previously persisted 24-hour takeout delay has elapsed and opens the
    /// `"Data export ready"` prompt if so.
    pub fn check_takeout_suggestion(
        ctx: &Context,
        theme_mode: ThemeMode,
        now_unix: tg_export::stand_in::TimeId,
    ) -> bool {
        let host = Self::get_or_init(ctx, theme_mode);
        host.lock()
            .map(|mut guard| guard.manager.check_takeout_suggestion(now_unix))
            .unwrap_or(false)
    }

    /// Opens the full account export panel ("Export Telegram data") from anywhere in the UI.
    pub fn open_full_export(ctx: &Context, theme_mode: ThemeMode) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            guard.manager.start_full_export();
        }
    }

    /// Opens the reduced single-chat export panel ("Export chat history") from a chat's menu or Info pane.
    pub fn open_single_chat_export(
        ctx: &Context,
        theme_mode: ThemeMode,
        peer: InputPeer,
        title: impl Into<String>,
    ) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            guard.manager.start_single_chat_export(peer, title);
        }
    }

    /// Convenience entry point for the Info pane in live mode: ensures the live starter is wired
    /// and opens the single-chat export settings panel for `peer`.
    pub fn open_single_chat_export_live(
        ctx: &Context,
        theme_mode: ThemeMode,
        starter: LiveExportStarter,
        tokio_handle: Option<tokio::runtime::Handle>,
        peer: InputPeer,
        title: impl Into<String>,
    ) {
        Self::set_live_export_starter(ctx, theme_mode, starter, tokio_handle);
        Self::open_single_chat_export(ctx, theme_mode, peer, title);
    }

    /// Opens the single-topic export panel ("Export topic history").
    pub fn open_single_topic_export(
        ctx: &Context,
        theme_mode: ThemeMode,
        peer: InputPeer,
        topic_root_id: i32,
        peer_id: u64,
        topic_title: impl Into<String>,
    ) {
        let host = Self::get_or_init(ctx, theme_mode);
        if let Ok(mut guard) = host.lock() {
            guard
                .manager
                .start_single_topic_export(peer, topic_root_id, peer_id, topic_title);
        }
    }

    /// Drives shortcuts (`Ctrl+Shift+E` / `Ctrl+Shift+H`), polls active `ExportController` runs,
    /// renders the persistent `ExportTopBar` while running, and renders the floating Export panel
    /// and any child modal boxes.
    pub fn update_and_render(ctx: &Context, palette: &Palette, theme_mode: ThemeMode) {
        let (open_full, open_single, esc_pressed, now_secs) = ctx.input(|i| {
            let chord = i
                .modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT);
            (
                chord && i.key_pressed(Key::E),
                chord && i.key_pressed(Key::H),
                i.key_pressed(Key::Escape),
                i.time,
            )
        });

        let state_arc = Self::get_or_init(ctx, theme_mode);
        let Ok(mut guard) = state_arc.lock() else {
            return;
        };

        if guard.last_theme_mode != theme_mode {
            guard.theme_manager.set_night_mode(theme_mode.is_night());
            guard.last_theme_mode = theme_mode;
        }

        if open_full {
            guard.manager.start_full_export();
        } else if open_single && !guard.manager.is_live_mode() {
            guard.manager.start_single_chat_export(
                InputPeer::User(InputPeerUser {
                    user_id: 100_002,
                    access_hash: 42,
                }),
                "Orbit Tester",
            );
        }

        guard.manager.poll(now_secs);
        if guard.manager.is_export_running() {
            ctx.request_repaint();
        }

        let screen_rect = ctx.content_rect();

        // 1. Top bar while export runs (clicking activates the export panel).
        if guard.manager.should_show_top_bar() {
            let mut clicked = false;
            Area::new(Id::unique("tg_export_top_bar_area"))
                .order(Order::Middle)
                .fixed_pos(screen_rect.min)
                .show(ctx, |ui| {
                    ui.set_min_width(screen_rect.width());
                    ui.set_max_width(screen_rect.width());
                    clicked = guard.manager.top_bar().show(ui, palette);
                });
            if clicked {
                guard.manager.activate_panel();
            }
        }

        // 2. Floating Export panel (`364x480` per `st::exportPanelSize`).
        if guard.manager.is_panel_visible() && esc_pressed && guard.manager.modal().is_none() {
            guard.manager.close_or_hide_panel();
        }

        if guard.manager.is_panel_visible() {
            let panel_w = EXPORT_PANEL_WIDTH.min((screen_rect.width() - 32.0).max(280.0));
            let panel_h = EXPORT_PANEL_HEIGHT.min((screen_rect.height() - 48.0).max(320.0));
            let panel_pos = Pos2::new(
                screen_rect.center().x - panel_w * 0.5,
                screen_rect.center().y - panel_h * 0.5,
            );
            let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));

            Area::new(Id::unique("tg_export_panel_backdrop"))
                .order(Order::Middle)
                .fixed_pos(screen_rect.min)
                .show(ctx, |ui| {
                    let (_, resp) = ui.allocate_exact_size(screen_rect.size(), Sense::click());
                    ui.painter().rect_filled(
                        screen_rect,
                        CornerRadius::ZERO,
                        palette.layer_backdrop,
                    );
                    if resp.clicked() && guard.manager.modal().is_none() {
                        guard.manager.close_or_hide_panel();
                    }
                });

            let colors_palette = guard.theme_manager.palette().clone();
            Area::new(Id::unique("tg_export_panel_window"))
                .order(Order::Foreground)
                .fixed_pos(panel_pos)
                .show(ctx, |ui| {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                        render_export_panel(
                            ui,
                            &mut guard.manager,
                            palette,
                            &colors_palette,
                            now_secs,
                        );
                    });
                });

            if guard.manager.modal().is_some() {
                render_export_modal(
                    ctx,
                    panel_rect,
                    &mut guard.manager,
                    palette,
                    &colors_palette,
                );
            }
        } else if guard.manager.modal().is_some() {
            // Standalone SuggestStart / Inform modal when panel is not open.
            let colors_palette = guard.theme_manager.palette().clone();
            render_export_modal(
                ctx,
                screen_rect,
                &mut guard.manager,
                palette,
                &colors_palette,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use egui::{Pos2, Rect, Vec2};
    use tg_export::stand_in::InputPeer;
    use tg_export::{
        ApiError, EntityType, ExportState, ExportTypes, FinishedState, InMemoryExportSource,
        MediaTypes, OutputFormat, ProcessingState, Settings, Step,
    };
    use tg_tl::types::InputPeerUser;
    use tg_ui::theme::DayPalette;

    use super::*;
    use tg_app_core::stand_in::theme::{Palette, ThemeMode};

    #[derive(Default)]
    struct FakeFolderChooser {
        choice: Mutex<Option<PathBuf>>,
    }

    impl FakeFolderChooser {
        fn returning(path: PathBuf) -> Self {
            Self {
                choice: Mutex::new(Some(path)),
            }
        }
    }

    impl FolderChooser for FakeFolderChooser {
        fn pick_folder(&self, _title: &str, _initial_dir: &str) -> Option<PathBuf> {
            self.choice.lock().ok().and_then(|g| g.clone())
        }
    }

    #[derive(Default, Clone)]
    struct FakeFolderOpener {
        opened: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl FakeFolderOpener {
        fn opened_paths(&self) -> Vec<PathBuf> {
            self.opened.lock().map(|g| g.clone()).unwrap_or_default()
        }
    }

    impl FolderOpener for FakeFolderOpener {
        fn show_in_folder(&self, path: &Path) -> Result<(), String> {
            if let Ok(mut g) = self.opened.lock() {
                g.push(path.to_path_buf());
            }
            Ok(())
        }
    }

    #[derive(Default, Clone)]
    struct MemorySettingsStore {
        bytes: Arc<Mutex<Option<Vec<u8>>>>,
    }

    impl SettingsStore for MemorySettingsStore {
        fn load(&self) -> Settings {
            self.bytes
                .lock()
                .ok()
                .and_then(|g| g.as_ref().and_then(|b| Settings::deserialize_qt(b)))
                .unwrap_or_default()
        }

        fn save(&self, settings: &Settings) -> Result<(), String> {
            if let Ok(mut g) = self.bytes.lock() {
                *g = Some(settings.serialize_qt());
            }
            Ok(())
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rustgram_export_test_{label}_{}_{nanos}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    // ========================================================================
    // 1. Size limit slider curve (`export_view_settings.cpp:SizeLimitByIndex`)
    // ========================================================================

    #[test]
    fn size_limit_slider_matches_tdesktop_piecewise_curve() {
        assert_eq!(size_limit_by_index(0), MEGABYTE);
        assert_eq!(size_limit_by_index(7), 8 * MEGABYTE);
        assert_eq!(size_limit_by_index(9), 10 * MEGABYTE);
        assert_eq!(size_limit_by_index(10), 12 * MEGABYTE);
        assert_eq!(size_limit_by_index(29), 50 * MEGABYTE);
        assert_eq!(size_limit_by_index(30), 55 * MEGABYTE);
        assert_eq!(size_limit_by_index(39), 100 * MEGABYTE);
        assert_eq!(size_limit_by_index(59), 300 * MEGABYTE);
        assert_eq!(size_limit_by_index(69), 500 * MEGABYTE);
        assert_eq!(size_limit_by_index(79), 1000 * MEGABYTE);
        assert_eq!(size_limit_by_index(89), 2000 * MEGABYTE);
        assert_eq!(size_limit_by_index(99), 4000 * MEGABYTE);

        // Strictly monotonic & exact round-trip for every step.
        for idx in 0..SIZE_VALUE_COUNT {
            let val = size_limit_by_index(idx);
            assert_eq!(index_by_size_limit(val), idx);
            if idx > 0 {
                assert!(val > size_limit_by_index(idx - 1));
            }
        }
        assert_eq!(format_size_limit_label(8 * MEGABYTE), "Size limit: 8 MB");
        assert_eq!(
            format_size_limit_label(4000 * MEGABYTE),
            "Size limit: 4000 MB"
        );
    }

    // ========================================================================
    // 2. Content view-model mapping (`export_view_content.cpp`)
    // ========================================================================

    #[test]
    fn content_from_processing_and_finished_matches_tdesktop() {
        let mut settings = Settings::default();
        let state = ProcessingState {
            step: Step::Dialogs,
            substeps_passed: 4,
            substeps_now: 4,
            substeps_total: 10,
            entity_type: EntityType::Chat,
            entity_name: "Design Notes".to_owned(),
            entity_index: 1,
            entity_count: 4,
            item_index: 12,
            item_count: 48,
            bytes_random_id: 77_001,
            bytes_name: "photo_12.jpg".to_owned(),
            bytes_loaded: 1_572_864,
            bytes_count: 3_670_016,
        };

        let content = content_from_processing(&settings, &state);
        assert_eq!(content.rows.len(), 3);
        assert_eq!(content.rows[0].id, "main");
        assert_eq!(content.rows[0].label, "Chats");
        assert_eq!(content.rows[0].info, "2 / 4");
        // done = 4/10 = 0.4, add = 4 * 1 / (10 * 4) = 0.1 => 0.5
        assert!((content.rows[0].progress - 0.5).abs() < 1e-6);

        assert_eq!(content.rows[1].id, "chat1");
        assert_eq!(content.rows[1].label, "Design Notes");
        assert_eq!(content.rows[1].info, "12 / 48");
        assert!((content.rows[1].progress - 0.25).abs() < 1e-6);

        assert_eq!(content.rows[2].id, "file1_12");
        assert_eq!(content.rows[2].label, "photo_12.jpg");
        assert_eq!(content.rows[2].info, "1.5 / 3.5 MB");
        assert_eq!(content.rows[2].random_id, 77_001);
        assert!((content.rows[2].progress - (1_572_864.0 / 3_670_016.0)).abs() < 1e-6);

        // Single-peer export pads to 2 rows and omits the top "Chats" row when entity_count == 1.
        settings.single_peer = InputPeer::Self_;
        let single_state = ProcessingState {
            step: Step::Dialogs,
            entity_type: EntityType::SavedMessages,
            entity_name: "Self".to_owned(),
            entity_index: 0,
            entity_count: 1,
            item_index: 5,
            item_count: 10,
            ..Default::default()
        };
        let single_content = content_from_processing(&settings, &single_state);
        assert_eq!(single_content.rows.len(), 2);
        assert_eq!(single_content.rows[0].label, "Saved Messages");
        assert_eq!(single_content.rows[0].info, "5 / 10");
        assert!(single_content.rows[1].id.is_empty());

        // Finished state -> 3 rows with DONE_ID and formatted totals.
        let done = content_from_finished(&FinishedState {
            path: "C:/Export/export_results.html".to_owned(),
            files_count: 24,
            bytes_count: 2_621_440,
        });
        assert!(done.is_done());
        assert_eq!(done.rows.len(), 3);
        assert_eq!(done.rows[0].label, "Data export completed.");
        assert_eq!(done.rows[1].label, "Total files: 24");
        assert_eq!(done.rows[2].label, "Total size: 2.5 MB");

        // Size & download formatting checks.
        assert_eq!(format_size_text(512), "512 B");
        assert_eq!(format_size_text(2_560), "2.5 KB");
        assert_eq!(format_size_text(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_download_text(2048, 8192), "2 / 8 KB");
        assert_eq!(format_download_text(300, 900), "300 / 900 B");
    }

    // ========================================================================
    // 3. Date & time range pickers + clamping (`export_view_settings.cpp`)
    // ========================================================================

    #[test]
    fn date_and_time_limit_pickers_roundtrip_and_clamp() {
        // The range is read in local time, so the times are built in local time.
        let _zone = super::settings_view::tests_local_time::ZoneGuard::east(2 * 3600);
        let ts = local_civil_to_unix(2025, 3, 15, 14, 30, 0);
        assert_eq!(format_date_long(ts), "March 15, 2025");
        assert_eq!(format_time_short(ts), "14:30");
        assert_eq!(format_date_time_full(ts), "March 15, 2025 at 14:30");

        let mut settings = Settings {
            single_peer: InputPeer::Self_,
            ..Default::default()
        };
        let day1 = local_civil_to_unix(2025, 3, 10, 0, 0, 0);
        let day2 = local_civil_to_unix(2025, 3, 20, 0, 0, 0);

        apply_from_date(&mut settings, day1);
        apply_till_date(&mut settings, day2);
        assert_eq!(settings.single_peer_from, day1);
        assert_eq!(settings.single_peer_till, day2);
        assert!(settings.validate());

        // Setting till <= from clamps till to from + 600s.
        apply_till_date(&mut settings, day1);
        assert_eq!(settings.single_peer_till, day1 + DATE_LIMIT_OFFSET_SECS);
        assert!(settings.validate());

        // Setting from_time >= till clamps from to till - 600s.
        apply_from_time(&mut settings, 12 * 3600);
        assert_eq!(
            settings.single_peer_from,
            settings.single_peer_till - DATE_LIMIT_OFFSET_SECS
        );

        // Calendar date picker enforces min_date >= 2013-08-01 and month navigation.
        let mut picker = DatePickerState::for_limit(day1, 0, day2);
        assert_eq!(picker.min_date, MIN_EXPORT_DATE);
        assert_eq!(picker.max_date, (2025, 3, 20));
        assert!(!picker.is_date_enabled((2013, 7, 31)));
        assert!(picker.is_date_enabled((2013, 8, 1)));
        assert!(!picker.is_date_enabled((2025, 3, 21)));

        picker.prev_month();
        assert_eq!((picker.view_year, picker.view_month), (2025, 2));
        picker.next_month();
        assert_eq!((picker.view_year, picker.view_month), (2025, 3));
    }

    // ========================================================================
    // 4. Real ExportController execution with InMemoryExportSource + Explorer
    // ========================================================================

    #[test]
    fn real_export_controller_produces_files_and_opens_folder_on_done() {
        let out_dir = unique_temp_dir("full_run");
        let opener = FakeFolderOpener::default();
        let store = MemorySettingsStore::default();
        let mut mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::returning(out_dir.clone())),
            Box::new(opener.clone()),
            Box::new(store.clone()),
            out_dir.to_string_lossy().into_owned(),
            Duration::ZERO,
        );

        mgr.start_full_export();
        assert!(mgr.is_active());
        assert!(mgr.is_panel_visible());
        assert_eq!(mgr.panel_title(), "Export Your Data");

        // Single running export invariant: second start call activates existing panel instead of replacing it.
        mgr.settings_mut().format = OutputFormat::HtmlAndJson;
        mgr.start_single_chat_export(InputPeer::Self_, "Should Not Replace");
        assert_eq!(mgr.target(), &ExportTarget::FullAccount);
        assert_eq!(mgr.settings().format, OutputFormat::HtmlAndJson);

        // Choose folder action updates path and marks settings dirty.
        mgr.handle_settings_actions(vec![SettingsViewAction::ChooseFolder]);
        assert_eq!(mgr.settings().path, out_dir.to_string_lossy());

        // Start real ExportController run via InMemoryExportSource.
        mgr.start_demo_export();
        assert_eq!(*mgr.screen(), ExportScreen::Progress);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut t = 0.0_f64;
        while mgr.finished_state().is_none() && std::time::Instant::now() < deadline {
            t += 0.05;
            mgr.poll(t);
            std::thread::sleep(Duration::from_millis(10));
        }

        let finished = mgr
            .finished_state()
            .expect("ExportController should finish and populate FinishedState")
            .clone();
        assert!(finished.files_count > 0);
        assert!(finished.bytes_count > 0);
        assert!(
            Path::new(&finished.path).exists(),
            "exported entry file should exist on disk: {}",
            finished.path
        );
        assert!(mgr.progress_view().is_done());

        // Verify both HTML and JSON outputs were actually written!
        let export_folder = Path::new(&finished.path)
            .parent()
            .expect("export file has parent dir");
        assert!(export_folder.join("export_results.html").exists());
        assert!(export_folder.join("result.json").exists());

        // Clicking "Show my data" invokes FolderOpener (tg-platform::show_in_folder) and closes panel.
        mgr.handle_progress_action(ProgressViewAction::ShowMyData);
        assert_eq!(opener.opened_paths(), vec![PathBuf::from(&finished.path)]);
        assert!(!mgr.is_active());

        // Settings persisted via tg-export Qt binary format round-trip cleanly.
        let reloaded = store.load();
        assert_eq!(reloaded.format, OutputFormat::HtmlAndJson);

        let _ = std::fs::remove_dir_all(&out_dir);
    }

    // ========================================================================
    // 5. 24-hour Takeout Delay (`TAKEOUT_INIT_DELAY_*`), SuggestStart, & Cancel
    // ========================================================================

    #[test]
    fn takeout_24h_delay_and_suggest_start_flow() {
        let info = parse_takeout_init_delay("TAKEOUT_INIT_DELAY_86400", 1_750_000_000)
            .expect("parse 24h delay");
        assert_eq!(info.delay_seconds, 86_400);
        assert_eq!(info.hours, 24);
        assert_eq!(info.hours_text, "24 hours");
        assert_eq!(info.available_at, 1_750_086_400);
        assert!(info.message.contains("24 hours"));

        let short = parse_takeout_init_delay("TAKEOUT_INIT_DELAY_1200", 1_750_000_000)
            .expect("parse short delay");
        assert_eq!(short.hours_text, "less than an hour");

        // Drive ExportManager with a source that returns TAKEOUT_INIT_DELAY_86400.
        let out_dir = unique_temp_dir("takeout_delay");
        let store = MemorySettingsStore::default();
        let mut mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::default()),
            Box::new(FakeFolderOpener::default()),
            Box::new(store.clone()),
            out_dir.to_string_lossy().into_owned(),
            Duration::ZERO,
        );
        mgr.set_demo_source_builder(Arc::new(|_| {
            let mut src = InMemoryExportSource::default();
            src.fail_on_start = Some(ApiError::new(
                420,
                "TAKEOUT_INIT_DELAY_86400",
                "Security wait required",
            ));
            src
        }));

        mgr.start_full_export();
        mgr.start_demo_export();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mgr.takeout_delay().is_none() && std::time::Instant::now() < deadline {
            mgr.poll(0.1);
            std::thread::sleep(Duration::from_millis(10));
        }

        let delay = mgr
            .takeout_delay()
            .expect("TAKEOUT_INIT_DELAY_86400 should populate takeout_delay")
            .clone();
        assert_eq!(delay.hours, 24);
        assert!(matches!(
            mgr.modal(),
            Some(ExportModal::Inform {
                close_panel_on_ok: true,
                ..
            })
        ));

        // available_at was persisted to SettingsStore via serialize_qt!
        let persisted = store.load();
        assert_eq!(persisted.available_at, delay.available_at);

        // Dismissing the inform modal closes the panel cleanly.
        mgr.dismiss_inform_modal();
        assert!(!mgr.is_active());

        // Before available_at -> no suggestion yet.
        assert!(!mgr.check_takeout_suggestion(delay.available_at - 10));
        // At/after available_at -> opens SuggestStart modal and clears persisted available_at!
        assert!(mgr.check_takeout_suggestion(delay.available_at));
        assert!(matches!(
            mgr.modal(),
            Some(ExportModal::SuggestStart { .. })
        ));
        assert_eq!(store.load().available_at, 0);

        // Accepting suggestion opens the export panel.
        mgr.accept_suggest_start();
        assert!(mgr.is_active());

        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn single_chat_reduced_settings_and_progress_skip_timer_and_cancel_confirm() {
        let out_dir = unique_temp_dir("single_chat");
        let store = MemorySettingsStore::default();
        let mut mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::default()),
            Box::new(FakeFolderOpener::default()),
            Box::new(store),
            out_dir.to_string_lossy().into_owned(),
            Duration::from_millis(60),
        );

        let peer = InputPeer::User(InputPeerUser {
            user_id: 100_002,
            access_hash: 42,
        });
        mgr.start_single_chat_export(peer.clone(), "Orbit Tester");
        assert!(mgr.settings().only_single_peer());
        assert_eq!(mgr.panel_title(), "Chat export settings");
        assert!(can_start_export(mgr.settings()));
        assert!(should_show_media_section(mgr.settings()));

        // Progress view skip-file link stays hidden until 5.0s elapsed on same file random_id.
        let mut pview = ProgressView::new(Content::default());
        let proc_content = content_from_processing(
            mgr.settings(),
            &ProcessingState {
                step: Step::Dialogs,
                entity_count: 1,
                entity_name: "Orbit Tester".to_owned(),
                item_index: 2,
                item_count: 10,
                bytes_random_id: 55_555,
                bytes_name: "archive.zip".to_owned(),
                bytes_loaded: 512_000,
                bytes_count: 2_048_000,
                ..Default::default()
            },
        );
        pview.update_content(proc_content, 10.0);
        assert!(!pview.should_show_skip_file(14.9));
        assert!(pview.should_show_skip_file(15.0));

        // Start a paced run, hide panel (top bar stays visible), then request stop with confirmation.
        mgr.start_demo_export();
        assert!(mgr.should_show_top_bar());
        mgr.close_or_hide_panel();
        assert!(!mgr.is_panel_visible());
        assert!(mgr.should_show_top_bar());

        mgr.stop_with_confirmation();
        assert!(mgr.is_panel_visible());
        assert_eq!(mgr.modal(), Some(&ExportModal::ConfirmStop));

        mgr.confirm_stop();
        assert!(!mgr.is_active());
        assert!(!mgr.should_show_top_bar());

        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn qt_file_settings_store_persists_binary_roundtrip() {
        let dir = unique_temp_dir("qt_store");
        let file = dir.join("export_settings.bin");
        let store = QtFileSettingsStore::new(file.clone());

        let s = Settings {
            format: OutputFormat::Json,
            types: ExportTypes::PERSONAL_INFO | ExportTypes::PERSONAL_CHATS | ExportTypes::CONTACTS,
            media: tg_export::MediaSettings {
                types: MediaTypes::PHOTO | MediaTypes::VIDEO | MediaTypes::FILE,
                size_limit: 64 * MEGABYTE,
            },
            path: "D:/Exports/Demo".to_owned(),
            available_at: 1_760_000_000,
            ..Default::default()
        };

        store.save(&s).expect("save qt settings");
        assert!(file.exists());

        let loaded = store.load();
        assert_eq!(loaded.format, OutputFormat::Json);
        assert_eq!(loaded.types, s.types);
        assert_eq!(loaded.media, s.media);
        assert_eq!(loaded.path, s.path);
        assert_eq!(loaded.available_at, s.available_at);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ========================================================================
    // 6. Offscreen egui screenshot generator (`crates/tg-app/docs/export_*.png`)
    // ========================================================================

    fn render_egui_to_png(
        logical_w: u32,
        logical_h: u32,
        ppp: f32,
        draw: impl FnMut(&mut egui::Ui),
    ) -> Vec<u8> {
        let ctx = egui::Context::default();
        ctx.set_visuals(Palette::day().to_egui_visuals(ThemeMode::Day));
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
        tg_ui::soft_raster::render_egui_to_png(&ctx, logical_w, logical_h, ppp, draw)
    }

    #[test]
    fn renders_export_settings_progress_and_done_screenshots() {
        let write_files = std::env::var("RUSTGRAM_UPDATE_SCREENSHOTS").as_deref() == Ok("1")
            || std::env::var("TGFORK_UPDATE_SCREENSHOTS").as_deref() == Ok("1");
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        if write_files {
            let _ = std::fs::create_dir_all(&docs_dir);
        }

        let palette = Palette::day();
        let colors = DayPalette::new();
        let out_dir = unique_temp_dir("screenshots");

        // 1. Export Settings screenshot (side-by-side: Full Account Export + Single-Chat Export).
        let mut full_mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::default()),
            Box::new(FakeFolderOpener::default()),
            Box::new(MemorySettingsStore::default()),
            out_dir.to_string_lossy().into_owned(),
            Duration::ZERO,
        );
        full_mgr.start_full_export();
        full_mgr.settings_mut().types |= ExportTypes::PUBLIC_GROUPS | ExportTypes::SESSIONS;
        full_mgr.settings_mut().media.types |= MediaTypes::VIDEO | MediaTypes::FILE;
        full_mgr.settings_mut().media.size_limit = 32 * MEGABYTE;

        let mut single_mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::default()),
            Box::new(FakeFolderOpener::default()),
            Box::new(MemorySettingsStore::default()),
            out_dir.to_string_lossy().into_owned(),
            Duration::ZERO,
        );
        single_mgr.start_single_chat_export(
            InputPeer::User(InputPeerUser {
                user_id: 100_002,
                access_hash: 42,
            }),
            "Orbit Tester",
        );
        single_mgr.settings_mut().media.types |= MediaTypes::VIDEO | MediaTypes::VOICE_MESSAGE;
        single_mgr.settings_mut().media.size_limit = 16 * MEGABYTE;
        single_mgr.settings_mut().format = OutputFormat::HtmlAndJson;
        single_mgr.settings_mut().single_peer_from = local_civil_to_unix(2025, 1, 1, 0, 0, 0);
        single_mgr.settings_mut().single_peer_till = local_civil_to_unix(2026, 6, 15, 18, 30, 0);

        let settings_png = render_egui_to_png(790, 620, 1.5, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.chat_canvas_bg);
            let left_rect = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(364.0, 580.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                render_export_panel(ui, &mut full_mgr, &palette, &colors, 0.0);
            });
            let right_rect = Rect::from_min_size(Pos2::new(406.0, 20.0), Vec2::new(364.0, 580.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                render_export_panel(ui, &mut single_mgr, &palette, &colors, 0.0);
            });
        });
        assert!(settings_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("export_settings.png"), &settings_png)
                .expect("write export_settings.png");
        }

        // 2. Export Progress screenshot (persistent TopBar at top + Progress panel with live rows).
        let mut prog_mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::default()),
            Box::new(FakeFolderOpener::default()),
            Box::new(MemorySettingsStore::default()),
            out_dir.to_string_lossy().into_owned(),
            Duration::ZERO,
        );
        prog_mgr.start_full_export();
        let sample_proc = ProcessingState {
            step: Step::Dialogs,
            substeps_passed: 5,
            substeps_now: 4,
            substeps_total: 10,
            entity_type: EntityType::Chat,
            entity_name: "Design Notes".to_owned(),
            entity_index: 2,
            entity_count: 4,
            item_index: 18,
            item_count: 32,
            bytes_random_id: 42_001,
            bytes_name: "photo_14@18-01-2026_12-30-00.jpg".to_owned(),
            bytes_loaded: 1_572_864,
            bytes_count: 3_670_016,
        };
        prog_mgr.handle_controller_state(ExportState::Processing(sample_proc), 1.0);

        let progress_png = render_egui_to_png(520, 560, 1.5, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.chat_canvas_bg);
            // Persistent Top Bar across the top of the window.
            let bar_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), TOP_BAR_HEIGHT));
            ui.scope_builder(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
                let _ = prog_mgr.top_bar().show(ui, &palette);
            });
            // Floating Export Progress panel centered below the top bar.
            let panel_rect = Rect::from_min_size(
                Pos2::new(
                    (rect.width() - EXPORT_PANEL_WIDTH) * 0.5,
                    TOP_BAR_HEIGHT + 24.0,
                ),
                Vec2::new(EXPORT_PANEL_WIDTH, EXPORT_PANEL_HEIGHT),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                render_export_panel(ui, &mut prog_mgr, &palette, &colors, 7.0);
            });
        });
        assert!(progress_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("export_progress.png"), &progress_png)
                .expect("write export_progress.png");
        }

        // 3. Export Done screenshot driven by a REAL ExportController run with InMemoryExportSource.
        let mut done_mgr = ExportManager::with_dependencies(
            Box::new(FakeFolderChooser::default()),
            Box::new(FakeFolderOpener::default()),
            Box::new(MemorySettingsStore::default()),
            out_dir.to_string_lossy().into_owned(),
            Duration::ZERO,
        );
        done_mgr.start_full_export();
        done_mgr.settings_mut().format = OutputFormat::HtmlAndJson;
        done_mgr.start_demo_export();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while done_mgr.finished_state().is_none() && std::time::Instant::now() < deadline {
            done_mgr.poll(2.0);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(done_mgr.progress_view().is_done());

        let done_png = render_egui_to_png(440, 520, 1.5, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.chat_canvas_bg);
            let panel_rect = Rect::from_min_size(
                Pos2::new((rect.width() - EXPORT_PANEL_WIDTH) * 0.5, 20.0),
                Vec2::new(EXPORT_PANEL_WIDTH, EXPORT_PANEL_HEIGHT),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                render_export_panel(ui, &mut done_mgr, &palette, &colors, 3.0);
            });
        });
        assert!(done_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("export_done.png"), &done_png)
                .expect("write export_done.png");
        }

        let _ = std::fs::remove_dir_all(&out_dir);
    }
}
