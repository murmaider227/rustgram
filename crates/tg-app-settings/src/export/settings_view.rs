//! Export settings widget, compact format selector button, and date/time range pickers.
//!
//! Port of `Telegram/SourceFiles/export/view/export_view_settings.h`,
//! `Telegram/SourceFiles/export/view/export_view_settings.cpp`,
//! `Telegram/SourceFiles/export/view/export_view_format_button.h`, and
//! `Telegram/SourceFiles/export/view/export_view_format_button.cpp`.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Margin, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use tg_export::stand_in::{TimeId, unix_to_civil};
use tg_export::{ExportTypes, MediaTypes, OutputFormat, Settings};
use tg_ui::style::{
    CheckStyle, CheckboxStyle, LinkButtonStyle, MediaSliderStyle, RadioStyle, RoundButtonStyle,
};
use tg_ui::theme::Colors;
use tg_ui::widgets::buttons::{LinkButton, RoundButton};
use tg_ui::widgets::checkbox::{Checkbox, Radio};
use tg_ui::widgets::sliders::MediaSlider;

use tg_app_core::stand_in::theme::Palette;

/// Number of discrete steps on the media size limit slider (`kSizeValueCount = 100`).
pub const SIZE_VALUE_COUNT: usize = 100;

/// One MiB in bytes (`kMegabyte = 1024 * 1024`).
pub const MEGABYTE: i64 = 1024 * 1024;

/// Minimum clamping gap between `from` and `till` date limits (`kOffset = 600` seconds = 10 minutes).
pub const DATE_LIMIT_OFFSET_SECS: TimeId = 600;

/// Earliest selectable Telegram date (`2013-08-01`, launch of Telegram; `export_view_settings.cpp:597`).
pub const MIN_EXPORT_DATE: (i32, u32, u32) = (2013, 8, 1);

/// Maps a slider step `index` in `0..SIZE_VALUE_COUNT` to a byte size limit (1 MiB .. 4000 MiB).
///
/// Exact port of `Export::View::SizeLimitByIndex` in `export_view_settings.cpp:73-97`.
#[must_use]
pub fn size_limit_by_index(index: usize) -> i64 {
    let clamped = index.min(SIZE_VALUE_COUNT - 1) + 1;
    let megabytes = if clamped <= 10 {
        clamped
    } else if clamped <= 30 {
        10 + (clamped - 10) * 2
    } else if clamped <= 40 {
        50 + (clamped - 30) * 5
    } else if clamped <= 60 {
        100 + (clamped - 40) * 10
    } else if clamped <= 70 {
        300 + (clamped - 60) * 20
    } else if clamped <= 80 {
        500 + (clamped - 70) * 50
    } else if clamped <= 90 {
        1000 + (clamped - 80) * 100
    } else {
        2000 + (clamped - 90) * 200
    };
    (megabytes as i64) * MEGABYTE
}

/// Finds the closest slider step `0..SIZE_VALUE_COUNT` matching `size_limit` bytes.
#[must_use]
pub fn index_by_size_limit(size_limit: i64) -> usize {
    let mut best_idx = 0usize;
    let mut best_diff = (size_limit - size_limit_by_index(0)).abs();
    for idx in 1..SIZE_VALUE_COUNT {
        let diff = (size_limit - size_limit_by_index(idx)).abs();
        if diff < best_diff {
            best_diff = diff;
            best_idx = idx;
        }
    }
    best_idx
}

/// Formats the media size limit label (`lng_export_option_size_limit`, e.g. `"Size limit: 8 MB"`).
#[must_use]
pub fn format_size_limit_label(size_limit: i64) -> String {
    let mb = (size_limit / MEGABYTE).max(1);
    format!("Size limit: {mb} MB")
}

/// Returns `true` if the Export button should be enabled/shown (`export_view_settings.cpp:632`).
#[must_use]
pub fn can_start_export(settings: &Settings) -> bool {
    !settings.types.is_empty() || settings.only_single_peer()
}

/// Returns `true` if the Media Export Settings section should be visible (`export_view_settings.cpp:220-226`).
#[must_use]
pub fn should_show_media_section(settings: &Settings) -> bool {
    if settings.only_single_peer() {
        return true;
    }
    let trigger_mask = ExportTypes::PERSONAL_CHATS
        | ExportTypes::BOT_CHATS
        | ExportTypes::PRIVATE_GROUPS
        | ExportTypes::PRIVATE_CHANNELS
        | ExportTypes::PUBLIC_GROUPS
        | ExportTypes::PUBLIC_CHANNELS
        | ExportTypes::PROFILE_MUSIC;
    settings.types.intersects(trigger_mask)
}

/// Normalizes a path for case-insensitive comparison on Windows (`IsDefaultPath` in `export_view_panel_controller.cpp`).
#[must_use]
pub fn is_default_export_path(path: &str, default_download_path: &str) -> bool {
    let norm = |v: &str| {
        let trimmed = v.replace('\\', "/");
        let trimmed = trimmed.trim_end_matches('/');
        if cfg!(windows) {
            trimmed.to_ascii_lowercase()
        } else {
            trimmed.to_owned()
        }
    };
    path.is_empty() || norm(path) == norm(default_download_path)
}

/// Formats the clickable export location string shown in Settings (`export_view_settings.cpp:288-295`).
#[must_use]
pub fn display_export_path(path: &str, default_download_path: &str) -> String {
    if is_default_export_path(path, default_download_path) {
        if cfg!(windows) {
            "Downloads\\Rustgram".to_owned()
        } else {
            "Downloads/Rustgram".to_owned()
        }
    } else if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_owned()
    }
}

/// Short label for the compact format dropdown button (`export_view_format_button.cpp:63-67`).
#[must_use]
pub const fn format_short_label(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Html => "HTML",
        OutputFormat::Json => "JSON",
        OutputFormat::HtmlAndJson => "Both",
    }
}

/// Which boundary of the single-peer date range is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateLimitKind {
    /// Start timestamp (`single_peer_from`).
    From,
    /// End timestamp (`single_peer_till`).
    Till,
}

/// Actions emitted by [`SettingsView::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsViewAction {
    /// Settings changed (caller should schedule debounced persistence).
    Changed,
    /// User clicked the `"Export"` button.
    Start,
    /// User clicked the `"Cancel"` button.
    Cancel,
    /// User clicked the output path link to open the native folder chooser.
    ChooseFolder,
    /// User clicked the `"From"` date link (single-peer mode).
    EditFromDate,
    /// User clicked the `"From"` time link (single-peer mode).
    EditFromTime,
    /// User clicked the `"To"` date link (single-peer mode).
    EditTillDate,
    /// User clicked the `"To"` time link (single-peer mode).
    EditTillTime,
}

/// Export settings box UI state (port of `Export::View::SettingsWidget`).
#[derive(Debug, Clone, Default)]
pub struct SettingsView {
    /// Whether the compact single-peer format dropdown menu is currently open.
    pub format_menu_open: bool,
}

impl SettingsView {
    /// Creates a new `SettingsView`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            format_menu_open: false,
        }
    }

    /// Renders the full or single-peer settings UI, mutating `settings` in place and returning
    /// all triggered [`SettingsViewAction`]s.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        settings: &mut Settings,
        default_download_path: &str,
        palette: &Palette,
        colors: &dyn Colors,
    ) -> Vec<SettingsViewAction> {
        let mut actions = Vec::new();
        let single_peer = settings.only_single_peer();
        let total_rect = ui.available_rect_before_wrap();
        let footer_h = 54.0;
        let scroll_h = (total_rect.height() - footer_h).max(120.0);

        let check_st = CheckStyle::default_check(colors);
        let radio_st = RadioStyle::default_radio(colors);
        let cb_st = CheckboxStyle::box_checkbox(colors);
        let link_st = LinkButtonStyle::default_link(colors);

        // Scrollable options body.
        let scroll_rect =
            Rect::from_min_size(total_rect.min, Vec2::new(total_rect.width(), scroll_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_export_settings_scroll", single_peer))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let pad_x = 22.0;
                    let inner_w = (scroll_rect.width() - pad_x * 2.0 - 8.0).max(160.0);
                    egui::Frame::NONE
                        .inner_margin(Margin {
                            left: pad_x as i8,
                            right: pad_x as i8,
                            top: 8,
                            bottom: 12,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(inner_w);

                            if !single_peer {
                                self.show_full_export_options(
                                    ui,
                                    settings,
                                    palette,
                                    cb_st,
                                    check_st,
                                    &mut actions,
                                );
                            }

                            if should_show_media_section(settings) {
                                if !single_peer {
                                    add_section_header(ui, "Media export settings", palette);
                                }
                                self.show_media_options(
                                    ui,
                                    settings,
                                    palette,
                                    colors,
                                    cb_st,
                                    check_st,
                                    &mut actions,
                                );
                            }

                            if !single_peer {
                                self.show_other_options(
                                    ui,
                                    settings,
                                    palette,
                                    cb_st,
                                    check_st,
                                    &mut actions,
                                );
                                self.show_full_path_and_format(
                                    ui,
                                    settings,
                                    default_download_path,
                                    palette,
                                    cb_st,
                                    radio_st,
                                    link_st,
                                    &mut actions,
                                );
                            } else {
                                self.show_single_peer_format_and_limits(
                                    ui,
                                    settings,
                                    default_download_path,
                                    palette,
                                    link_st,
                                    &mut actions,
                                );
                            }
                        });
                });
        });

        // Bottom divider + Cancel / Export buttons (`SettingsWidget::setupButtons`).
        let footer_rect = Rect::from_min_size(
            Pos2::new(total_rect.min.x, scroll_rect.max.y),
            Vec2::new(total_rect.width(), footer_h),
        );
        ui.painter().line_segment(
            [
                Pos2::new(footer_rect.min.x, footer_rect.min.y),
                Pos2::new(footer_rect.max.x, footer_rect.min.y),
            ],
            Stroke::new(1.0, palette.divider),
        );

        let can_start = can_start_export(settings);
        let box_btn_st = RoundButtonStyle::default_box(colors);
        ui.scope_builder(egui::UiBuilder::new().max_rect(footer_rect), |ui| {
            egui::Frame::NONE
                .inner_margin(Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if can_start && RoundButton::new("Export", box_btn_st).show(ui).clicked() {
                            actions.push(SettingsViewAction::Start);
                        }
                        ui.add_space(6.0);
                        if RoundButton::new("Cancel", box_btn_st).show(ui).clicked() {
                            actions.push(SettingsViewAction::Cancel);
                        }
                    });
                });
        });

        actions
    }

    fn show_full_export_options(
        &self,
        ui: &mut Ui,
        settings: &mut Settings,
        palette: &Palette,
        cb_st: CheckboxStyle,
        check_st: CheckStyle,
        actions: &mut Vec<SettingsViewAction>,
    ) {
        add_option_with_about(
            ui,
            settings,
            "Account information",
            ExportTypes::PERSONAL_INFO | ExportTypes::USERPICS,
            "Your chosen display name, username, phone number and profile photos.",
            palette,
            cb_st,
            check_st,
            actions,
        );
        add_option_with_about(
            ui,
            settings,
            "Contacts list",
            ExportTypes::CONTACTS,
            "If you allow access, contacts are continuously synced with Telegram. You can adjust this in Settings > Privacy & Security on mobile devices.",
            palette,
            cb_st,
            check_st,
            actions,
        );
        add_option_with_about(
            ui,
            settings,
            "Story archive",
            ExportTypes::STORIES,
            "All stories you posted from Telegram mobile apps.",
            palette,
            cb_st,
            check_st,
            actions,
        );
        add_option_with_about(
            ui,
            settings,
            "Music on Profiles",
            ExportTypes::PROFILE_MUSIC,
            "All tracks you saved to your playlist.",
            palette,
            cb_st,
            check_st,
            actions,
        );

        add_section_header(ui, "Chat export settings", palette);

        add_type_checkbox(
            ui,
            settings,
            "Personal chats",
            ExportTypes::PERSONAL_CHATS,
            cb_st,
            check_st,
            actions,
        );
        add_type_checkbox(
            ui,
            settings,
            "Bot chats",
            ExportTypes::BOT_CHATS,
            cb_st,
            check_st,
            actions,
        );
        add_chat_option(
            ui,
            settings,
            "Private groups",
            ExportTypes::PRIVATE_GROUPS,
            false,
            cb_st,
            check_st,
            actions,
        );
        add_chat_option(
            ui,
            settings,
            "Private channels",
            ExportTypes::PRIVATE_CHANNELS,
            false,
            cb_st,
            check_st,
            actions,
        );
        add_chat_option(
            ui,
            settings,
            "Public groups",
            ExportTypes::PUBLIC_GROUPS,
            true,
            cb_st,
            check_st,
            actions,
        );
        add_chat_option(
            ui,
            settings,
            "Public channels",
            ExportTypes::PUBLIC_CHANNELS,
            true,
            cb_st,
            check_st,
            actions,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn show_media_options(
        &self,
        ui: &mut Ui,
        settings: &mut Settings,
        palette: &Palette,
        colors: &dyn Colors,
        cb_st: CheckboxStyle,
        check_st: CheckStyle,
        actions: &mut Vec<SettingsViewAction>,
    ) {
        let items = [
            ("Photos", MediaTypes::PHOTO),
            ("Videos", MediaTypes::VIDEO),
            ("Voice messages", MediaTypes::VOICE_MESSAGE),
            ("Video messages", MediaTypes::VIDEO_MESSAGE),
            ("Stickers", MediaTypes::STICKER),
            ("GIFs", MediaTypes::GIF),
        ];
        for (label, flag) in items {
            let mut checked = settings.media.types.contains(flag);
            if Checkbox::new(&mut checked, label, cb_st, check_st)
                .show(ui)
                .changed()
            {
                settings.media.types.set(flag, checked);
                actions.push(SettingsViewAction::Changed);
            }
            ui.add_space(2.0);
        }

        // `"Files"` row paired with `"Size limit: N MB"` on the right (`export_view_settings.cpp:763-855`).
        let size_label = format_size_limit_label(settings.media.size_limit);
        ui.horizontal(|ui| {
            let mut files_checked = settings.media.types.contains(MediaTypes::FILE);
            if Checkbox::new(&mut files_checked, "Files", cb_st, check_st)
                .show(ui)
                .changed()
            {
                settings.media.types.set(MediaTypes::FILE, files_checked);
                actions.push(SettingsViewAction::Changed);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(size_label)
                        .size(13.5)
                        .color(palette.window_sub_text_fg),
                );
            });
        });

        // Continuous pseudo-discrete size slider (`kSizeValueCount = 100`, 1 MB .. 4000 MB).
        ui.add_space(4.0);
        let current_index = index_by_size_limit(settings.media.size_limit);
        let mut slider_val = current_index as f64 / (SIZE_VALUE_COUNT - 1) as f64;
        let mut slider_style = MediaSliderStyle::default_media_slider(colors);
        slider_style.seek_size = tg_ui::style::Size::new(15.0, 15.0);
        let resp = MediaSlider::new(&mut slider_val, slider_style)
            .always_display_marker(true)
            .show(ui);
        if resp.changed() {
            let new_idx = ((slider_val * (SIZE_VALUE_COUNT - 1) as f64).round() as usize)
                .min(SIZE_VALUE_COUNT - 1);
            let new_limit = size_limit_by_index(new_idx);
            if settings.media.size_limit != new_limit {
                settings.media.size_limit = new_limit;
                actions.push(SettingsViewAction::Changed);
            }
        }
        ui.add_space(6.0);
    }

    fn show_other_options(
        &self,
        ui: &mut Ui,
        settings: &mut Settings,
        palette: &Palette,
        cb_st: CheckboxStyle,
        check_st: CheckStyle,
        actions: &mut Vec<SettingsViewAction>,
    ) {
        add_section_header(ui, "Other", palette);
        add_option_with_about(
            ui,
            settings,
            "Active sessions",
            ExportTypes::SESSIONS,
            "We may store this to display your connected devices in Settings > Privacy & Security > Show all sessions.",
            palette,
            cb_st,
            check_st,
            actions,
        );
        add_option_with_about(
            ui,
            settings,
            "Miscellaneous data",
            ExportTypes::OTHER_DATA,
            "Other types of data not mentioned above (beta).",
            palette,
            cb_st,
            check_st,
            actions,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn show_full_path_and_format(
        &self,
        ui: &mut Ui,
        settings: &mut Settings,
        default_download_path: &str,
        palette: &Palette,
        cb_st: CheckboxStyle,
        radio_st: RadioStyle,
        link_st: LinkButtonStyle,
        actions: &mut Vec<SettingsViewAction>,
    ) {
        add_section_header(ui, "Location and format", palette);

        let path_display = display_export_path(&settings.path, default_download_path);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                egui::RichText::new("Download path:")
                    .size(13.5)
                    .color(palette.window_fg),
            );
            if LinkButton::new(&path_display, link_st).show(ui).clicked() {
                actions.push(SettingsViewAction::ChooseFolder);
            }
        });
        ui.add_space(8.0);

        let formats = [
            (OutputFormat::Html, "Human-readable HTML"),
            (OutputFormat::Json, "Machine-readable JSON"),
            (OutputFormat::HtmlAndJson, "Both"),
        ];
        for (fmt, label) in formats {
            if Radio::new(&mut settings.format, fmt, label, cb_st, radio_st)
                .show(ui)
                .changed()
            {
                actions.push(SettingsViewAction::Changed);
            }
            ui.add_space(4.0);
        }
    }

    fn show_single_peer_format_and_limits(
        &mut self,
        ui: &mut Ui,
        settings: &mut Settings,
        default_download_path: &str,
        palette: &Palette,
        link_st: LinkButtonStyle,
        actions: &mut Vec<SettingsViewAction>,
    ) {
        ui.add_space(6.0);
        // Compact `"Format: [HTML v], Path: Downloads\Rustgram"` row
        // (port of `export_view_format_button.cpp:CreateFormatAndLocation`).
        let path_display = display_export_path(&settings.path, default_download_path);
        let fmt_label = format_short_label(settings.format);
        let mut fmt_btn_rect = Rect::NOTHING;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                egui::RichText::new("Format:")
                    .size(13.5)
                    .color(palette.window_fg),
            );

            // Pill dropdown button (`st::exportFormatButton`) with crisp vector chevron.
            let galley = ui.painter().layout_no_wrap(
                fmt_label.to_owned(),
                FontId::proportional(12.5),
                palette.window_bold_fg,
            );
            let arrow_space = 12.0;
            let btn_size = Vec2::new(galley.size().x + 14.0 + arrow_space, 22.0);
            let (rect, resp) = ui.allocate_exact_size(btn_size, Sense::click());
            fmt_btn_rect = rect;
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let bg = if resp.hovered() || self.format_menu_open {
                palette.divider
            } else {
                palette.window_bg_over
            };
            ui.painter().rect_filled(rect, CornerRadius::same(11), bg);
            ui.painter().galley(
                Pos2::new(rect.min.x + 8.0, rect.center().y - galley.size().y * 0.5),
                galley,
                palette.window_bold_fg,
            );
            // Vector dropdown arrow (`exportFormatDropdownArrow`).
            let ax = rect.max.x - 10.0;
            let ay = rect.center().y;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(ax - 3.5, ay - 2.0),
                    Pos2::new(ax + 3.5, ay - 2.0),
                    Pos2::new(ax, ay + 2.5),
                ],
                palette.window_sub_text_fg,
                Stroke::NONE,
            ));
            if resp.clicked() {
                self.format_menu_open = !self.format_menu_open;
            }

            ui.label(
                egui::RichText::new(", Path:")
                    .size(13.5)
                    .color(palette.window_fg),
            );
            if LinkButton::new(&path_display, link_st).show(ui).clicked() {
                actions.push(SettingsViewAction::ChooseFolder);
            }
        });

        // Inline format popup menu when toggled open.
        if self.format_menu_open {
            let popup_pos = Pos2::new(fmt_btn_rect.min.x, fmt_btn_rect.max.y + 4.0);
            egui::Area::new(egui::Id::unique("tg_export_format_popup"))
                .order(egui::Order::Tooltip)
                .fixed_pos(popup_pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(6, 6))
                        .show(ui, |ui| {
                            ui.set_min_width(196.0);
                            let opts = [
                                (OutputFormat::Html, "Human-readable HTML"),
                                (OutputFormat::Json, "Machine-readable JSON"),
                                (OutputFormat::HtmlAndJson, "Both"),
                            ];
                            for (fmt, label) in opts {
                                let selected = settings.format == fmt;
                                let (r, resp) =
                                    ui.allocate_exact_size(Vec2::new(196.0, 28.0), Sense::click());
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    ui.painter().rect_filled(
                                        r,
                                        CornerRadius::same(4),
                                        palette.window_bg_over,
                                    );
                                }
                                let prefix = if selected { "\u{2713} " } else { "   " };
                                ui.painter().text(
                                    Pos2::new(r.min.x + 8.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    format!("{prefix}{label}"),
                                    FontId::proportional(13.0),
                                    if selected {
                                        palette.window_active_text_fg
                                    } else {
                                        palette.window_fg
                                    },
                                );
                                if resp.clicked() {
                                    if settings.format != fmt {
                                        settings.format = fmt;
                                        actions.push(SettingsViewAction::Changed);
                                    }
                                    self.format_menu_open = false;
                                }
                            }
                        });
                });
        }

        // Date range `"From: {from}, to: {till}"` clickable pickers (`export_view_settings.cpp:359-556`).
        ui.add_space(10.0);
        let from_date_label = if settings.single_peer_from != 0 {
            format_date_long(settings.single_peer_from)
        } else {
            "the oldest message".to_owned()
        };
        let from_time_label = if settings.single_peer_from != 0 {
            Some(format_time_short(settings.single_peer_from))
        } else {
            None
        };
        let till_date_label = if settings.single_peer_till != 0 {
            format_date_long(settings.single_peer_till)
        } else {
            "present".to_owned()
        };
        let till_time_label = if settings.single_peer_till != 0 {
            Some(format_time_short(settings.single_peer_till))
        } else {
            None
        };

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                egui::RichText::new("From:")
                    .size(13.5)
                    .color(palette.window_fg),
            );
            if LinkButton::new(&from_date_label, link_st)
                .show(ui)
                .clicked()
            {
                actions.push(SettingsViewAction::EditFromDate);
            }
            if let Some(ref t) = from_time_label {
                ui.label(egui::RichText::new(",").size(13.5).color(palette.window_fg));
                if LinkButton::new(t, link_st).show(ui).clicked() {
                    actions.push(SettingsViewAction::EditFromTime);
                }
            }

            ui.label(
                egui::RichText::new(", to:")
                    .size(13.5)
                    .color(palette.window_fg),
            );
            if LinkButton::new(&till_date_label, link_st)
                .show(ui)
                .clicked()
            {
                actions.push(SettingsViewAction::EditTillDate);
            }
            if let Some(ref t) = till_time_label {
                ui.label(egui::RichText::new(",").size(13.5).color(palette.window_fg));
                if LinkButton::new(t, link_st).show(ui).clicked() {
                    actions.push(SettingsViewAction::EditTillTime);
                }
            }
        });
    }
}

fn add_section_header(ui: &mut Ui, text: &str, palette: &Palette) {
    ui.add_space(14.0);
    ui.label(
        egui::RichText::new(text)
            .size(15.0)
            .strong()
            .color(palette.window_bold_fg),
    );
    ui.add_space(8.0);
}

fn add_type_checkbox(
    ui: &mut Ui,
    settings: &mut Settings,
    label: &str,
    flags: ExportTypes,
    cb_st: CheckboxStyle,
    check_st: CheckStyle,
    actions: &mut Vec<SettingsViewAction>,
) -> bool {
    let mut checked = settings.types.contains(flags);
    if Checkbox::new(&mut checked, label, cb_st, check_st)
        .show(ui)
        .changed()
    {
        settings.types.set(flags, checked);
        actions.push(SettingsViewAction::Changed);
    }
    ui.add_space(4.0);
    checked
}

#[allow(clippy::too_many_arguments)]
fn add_option_with_about(
    ui: &mut Ui,
    settings: &mut Settings,
    label: &str,
    flags: ExportTypes,
    about: &str,
    palette: &Palette,
    cb_st: CheckboxStyle,
    check_st: CheckStyle,
    actions: &mut Vec<SettingsViewAction>,
) {
    add_type_checkbox(ui, settings, label, flags, cb_st, check_st, actions);
    ui.add(
        egui::Label::new(
            egui::RichText::new(about)
                .size(12.5)
                .color(palette.window_sub_text_fg),
        )
        .wrap(),
    );
    ui.add_space(12.0);
}

#[allow(clippy::too_many_arguments)]
fn add_chat_option(
    ui: &mut Ui,
    settings: &mut Settings,
    label: &str,
    flags: ExportTypes,
    force_only_my: bool,
    cb_st: CheckboxStyle,
    check_st: CheckStyle,
    actions: &mut Vec<SettingsViewAction>,
) {
    let checked = add_type_checkbox(ui, settings, label, flags, cb_st, check_st, actions);
    if checked {
        ui.horizontal(|ui| {
            ui.add_space(34.0);
            let mut only_my = if force_only_my {
                true
            } else {
                !settings.full_chats.contains(flags)
            };
            if Checkbox::new(&mut only_my, "Only my messages", cb_st, check_st)
                .enabled(!force_only_my)
                .show(ui)
                .changed()
                && !force_only_my
            {
                settings.full_chats.set(flags, !only_my);
                actions.push(SettingsViewAction::Changed);
            }
        });
        ui.add_space(6.0);
    }
}

// ============================================================================
// Date & Time Limit Helpers + Calendar / Time Picker Modals
// ============================================================================

/// Returns `true` if `year` is a Gregorian leap year.
#[must_use]
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Returns the number of days in `(year, month)` (`month` in `1..=12`).
#[must_use]
pub const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Converts a Gregorian civil UTC date/time to a Unix [`TimeId`] (inverse of `unix_to_civil`).
#[must_use]
pub fn civil_to_unix(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> TimeId {
    let y = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = u64::from(month);
    let d = u64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + (doe as i64) - 719_468;
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    secs.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as TimeId
}

/// Returns the current UTC `(year, month, day)`.
#[must_use]
pub fn today_civil_date() -> (i32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_735_689_600);
    let (y, m, d, _, _, _) = unix_to_civil(secs, 0);
    (y, m, d)
}

/// Returns the day-of-week index (`0 = Mon .. 6 = Sun`) for `(year, month, day)`.
#[must_use]
pub fn weekday_mon0(year: i32, month: u32, day: u32) -> usize {
    let ts = i64::from(civil_to_unix(year, month, day, 0, 0, 0));
    let days = ts.div_euclid(86_400);
    // 1970-01-01 was Thursday (index 3 when Mon=0).
    (days + 3).rem_euclid(7) as usize
}

/// English full month name for `month` in `1..=12`.
#[must_use]
pub const fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "January",
    }
}

/// Formats a [`TimeId`] date as `"January 15, 2026"` (`langDayOfMonthFull`).
#[must_use]
pub fn format_date_long(ts: TimeId) -> String {
    let (y, m, d, _, _, _) = unix_to_civil(i64::from(ts), 0);
    format!("{} {d}, {y}", month_name(m))
}

/// Formats a [`TimeId`] time as `"HH:MM"`.
#[must_use]
pub fn format_time_short(ts: TimeId) -> String {
    let (_, _, _, h, min, _) = unix_to_civil(i64::from(ts), 0);
    format!("{h:02}:{min:02}")
}

/// Formats a [`TimeId`] as `"January 15, 2026 at 14:30"` (`langDateTimeFull`).
#[must_use]
pub fn format_date_time_full(ts: TimeId) -> String {
    format!("{} at {}", format_date_long(ts), format_time_short(ts))
}

/// Strips the time-of-day component from `ts`, keeping UTC 00:00:00 (`removeTime` in `export_view_settings.cpp:439`).
#[must_use]
pub fn remove_time_of_day(ts: TimeId) -> TimeId {
    let (y, m, d, _, _, _) = unix_to_civil(i64::from(ts), 0);
    civil_to_unix(y, m, d, 0, 0, 0)
}

/// Applies a chosen `"From"` date timestamp (`0` resets to beginning).
pub fn apply_from_date(settings: &mut Settings, limit: TimeId) {
    settings.single_peer_from = limit;
}

/// Applies a chosen `"From"` time-of-day in seconds, clamping to `till - 600` if needed
/// (`export_view_settings.cpp:498-510`).
pub fn apply_from_time(settings: &mut Settings, time_of_day_secs: TimeId) {
    if settings.single_peer_from == 0 {
        return;
    }
    let result = remove_time_of_day(settings.single_peer_from) + time_of_day_secs;
    if settings.single_peer_till != 0 && result >= settings.single_peer_till {
        settings.single_peer_from = settings.single_peer_till - DATE_LIMIT_OFFSET_SECS;
    } else {
        settings.single_peer_from = result;
    }
}

/// Applies a chosen `"Till"` date timestamp (`0` resets to present), clamping to `from + 600` if needed
/// (`export_view_settings.cpp:513-523`).
pub fn apply_till_date(settings: &mut Settings, limit: TimeId) {
    if limit != 0 && settings.single_peer_from != 0 && limit <= settings.single_peer_from {
        settings.single_peer_till = settings.single_peer_from + DATE_LIMIT_OFFSET_SECS;
    } else {
        settings.single_peer_till = limit;
    }
}

/// Applies a chosen `"Till"` time-of-day in seconds, clamping to `from + 600` if needed
/// (`export_view_settings.cpp:538-550`).
pub fn apply_till_time(settings: &mut Settings, time_of_day_secs: TimeId) {
    if settings.single_peer_till == 0 {
        return;
    }
    let result = remove_time_of_day(settings.single_peer_till) + time_of_day_secs;
    if settings.single_peer_from != 0 && result <= settings.single_peer_from {
        settings.single_peer_till = settings.single_peer_from + DATE_LIMIT_OFFSET_SECS;
    } else {
        settings.single_peer_till = result;
    }
}

/// State of the interactive Calendar date picker modal (`Ui::CalendarBox` in `export_view_settings.cpp:558-604`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatePickerState {
    /// Currently viewed year.
    pub view_year: i32,
    /// Currently viewed month (`1..=12`).
    pub view_month: u32,
    /// Highlighted date `(year, month, day)`.
    pub highlighted: (i32, u32, u32),
    /// Minimum selectable date inclusive (`(2013, 8, 1)` or `single_peer_from`).
    pub min_date: (i32, u32, u32),
    /// Maximum selectable date inclusive (`today` or `single_peer_till`).
    pub max_date: (i32, u32, u32),
}

impl DatePickerState {
    /// Builds a `DatePickerState` matching `SettingsWidget::editDateLimit`.
    #[must_use]
    pub fn for_limit(current: TimeId, min_ts: TimeId, max_ts: TimeId) -> Self {
        let today = today_civil_date();
        let to_ymd = |ts: TimeId| {
            let (y, m, d, _, _, _) = unix_to_civil(i64::from(ts), 0);
            (y, m, d)
        };
        let highlighted = if current != 0 {
            to_ymd(current)
        } else if max_ts != 0 {
            to_ymd(max_ts)
        } else if min_ts != 0 {
            to_ymd(min_ts)
        } else {
            today
        };
        let min_date = if min_ts != 0 {
            to_ymd(min_ts)
        } else {
            MIN_EXPORT_DATE
        };
        let max_date = if max_ts != 0 { to_ymd(max_ts) } else { today };
        Self {
            view_year: highlighted.0,
            view_month: highlighted.1,
            highlighted,
            min_date,
            max_date,
        }
    }

    /// Moves calendar view one month backward.
    pub fn prev_month(&mut self) {
        if self.view_month == 1 {
            self.view_year -= 1;
            self.view_month = 12;
        } else {
            self.view_month -= 1;
        }
    }

    /// Moves calendar view one month forward.
    pub fn next_month(&mut self) {
        if self.view_month == 12 {
            self.view_year += 1;
            self.view_month = 1;
        } else {
            self.view_month += 1;
        }
    }

    /// Returns `true` if `(year, month, day)` is within `[min_date, max_date]`.
    #[must_use]
    pub fn is_date_enabled(&self, ymd: (i32, u32, u32)) -> bool {
        ymd >= self.min_date && ymd <= self.max_date
    }

    /// Renders the calendar modal body and returns `Some(timestamp)` when a date or Reset (`0`) is chosen.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        colors: &dyn Colors,
    ) -> Option<Option<TimeId>> {
        let mut outcome: Option<Option<TimeId>> = None;

        // Month header with prev / next buttons.
        ui.horizontal(|ui| {
            if ui.button("<").clicked() {
                self.prev_month();
            }
            ui.with_layout(
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {}",
                            month_name(self.view_month),
                            self.view_year
                        ))
                        .size(14.5)
                        .strong()
                        .color(palette.window_bold_fg),
                    );
                },
            );
            if ui.button(">").clicked() {
                self.next_month();
            }
        });
        ui.add_space(6.0);

        // Day-of-week header row.
        let cell_w = 38.0;
        let cell_h = 32.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for wd in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                let (r, _) = ui.allocate_exact_size(Vec2::new(cell_w, 22.0), Sense::hover());
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    wd,
                    FontId::proportional(11.5),
                    palette.window_sub_text_fg,
                );
            }
        });

        let first_wd = weekday_mon0(self.view_year, self.view_month, 1);
        let total_days = days_in_month(self.view_year, self.view_month);
        let total_cells = first_wd + total_days as usize;
        let rows = total_cells.div_ceil(7);

        for row in 0..rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for col in 0..7 {
                    let slot = row * 7 + col;
                    let (r, resp) =
                        ui.allocate_exact_size(Vec2::new(cell_w, cell_h), Sense::click());
                    if slot < first_wd || slot >= total_cells {
                        continue;
                    }
                    let day = (slot - first_wd + 1) as u32;
                    let ymd = (self.view_year, self.view_month, day);
                    let enabled = self.is_date_enabled(ymd);
                    let selected = ymd == self.highlighted;

                    if enabled && resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if selected {
                        ui.painter()
                            .circle_filled(r.center(), 14.0, palette.window_bg_active);
                    } else if enabled && resp.hovered() {
                        ui.painter()
                            .circle_filled(r.center(), 14.0, palette.window_bg_over);
                    }

                    let fg = if selected {
                        palette.window_fg_active
                    } else if enabled {
                        palette.window_fg
                    } else {
                        Color32::from_rgba_unmultiplied(
                            palette.window_sub_text_fg.r(),
                            palette.window_sub_text_fg.g(),
                            palette.window_sub_text_fg.b(),
                            110,
                        )
                    };
                    ui.painter().text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        format!("{day}"),
                        FontId::proportional(13.0),
                        fg,
                    );

                    if enabled && resp.clicked() {
                        self.highlighted = ymd;
                        outcome = Some(Some(civil_to_unix(ymd.0, ymd.1, ymd.2, 0, 0, 0)));
                    }
                }
            });
        }

        ui.add_space(10.0);
        let btn_st = RoundButtonStyle::default_box(colors);
        ui.horizontal(|ui| {
            if RoundButton::new("Reset", btn_st).show(ui).clicked() {
                outcome = Some(Some(0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if RoundButton::new("Cancel", btn_st).show(ui).clicked() {
                    outcome = Some(None);
                }
            });
        });

        outcome
    }
}

/// State of the time-of-day picker modal (`Ui::ChooseTimeWidget` in `export_view_settings.cpp:446-473`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimePickerState {
    /// Hour `0..=23`.
    pub hour: u32,
    /// Minute `0..=59`.
    pub minute: u32,
}

impl TimePickerState {
    /// Initializes a `TimePickerState` from a UTC [`TimeId`].
    #[must_use]
    pub fn from_timestamp(ts: TimeId) -> Self {
        let (_, _, _, h, m, _) = unix_to_civil(i64::from(ts), 0);
        Self { hour: h, minute: m }
    }

    /// Returns seconds since midnight (`hour * 3600 + minute * 60`).
    #[must_use]
    pub const fn as_seconds(self) -> TimeId {
        (self.hour * 3600 + self.minute * 60) as TimeId
    }

    /// Renders the time picker modal body, returning `Some(Some(seconds))` on Save or
    /// `Some(None)` on Cancel.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        colors: &dyn Colors,
    ) -> Option<Option<TimeId>> {
        let mut outcome = None;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Hour:")
                    .size(13.5)
                    .color(palette.window_fg),
            );
            ui.add(egui::DragValue::new(&mut self.hour).range(0..=23));
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("Minute:")
                    .size(13.5)
                    .color(palette.window_fg),
            );
            ui.add(egui::DragValue::new(&mut self.minute).range(0..=59));
        });
        ui.add_space(8.0);
        ui.painter().rect_stroke(
            ui.available_rect_before_wrap().shrink2(Vec2::new(0.0, 2.0)),
            CornerRadius::same(4),
            Stroke::NONE,
            StrokeKind::Inside,
        );
        ui.label(
            egui::RichText::new(format!(
                "Selected time: {:02}:{:02}",
                self.hour, self.minute
            ))
            .size(13.0)
            .color(palette.window_sub_text_fg),
        );
        ui.add_space(12.0);
        let btn_st = RoundButtonStyle::default_box(colors);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if RoundButton::new("Save", btn_st).show(ui).clicked() {
                outcome = Some(Some(self.as_seconds()));
            }
            ui.add_space(6.0);
            if RoundButton::new("Cancel", btn_st).show(ui).clicked() {
                outcome = Some(None);
            }
        });
        outcome
    }
}
