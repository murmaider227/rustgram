//! Compose modal dialogs: Schedule box (date/time picker + "Send when online"),
//! Polls/Quiz creation box (`boxes/create_poll_box.cpp`), Location & Venues picker,
//! Share Contact picker, Attach Files box (`tg-chat::media_prepare` + `tg-editor`),
//! Delete-for-everyone confirmation box, and Report Message box.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/history_view_schedule_box.cpp`
//! - `Telegram/SourceFiles/boxes/create_poll_box.cpp`
//! - `Telegram/SourceFiles/boxes/send_files_box.cpp`
//! - `Telegram/SourceFiles/editor/photo_editor.cpp`
//! - `Telegram/SourceFiles/boxes/delete_messages_box.cpp`
//! - `Telegram/SourceFiles/boxes/report_messages_box.cpp`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, ScrollArea,
    Sense, Stroke, TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_chat::media_prepare::{
    AlbumType, CandidateFile, PreparedGroup, PreparedList, ProbedMedia, SendFilesWay,
    divide_by_groups, photo_side_limit, prepare_media_list,
};
use tg_chat::send_options::default_schedule_time;
use tg_editor::video::VideoModifications;
use tg_editor::{
    BrushStroke, BrushTool, CropPreset, DrawToReplyRequest, LocationData, LocationItem, PaintScene,
    PhotoExportSettings, PhotoModifications, PointF, ShapeItem, ShapeType, Size as EditorSize,
    StoryLinkItem, StoryReactionItem, StrokePoint, TextAlignment, TextItem, TextStyle,
    TextTypeface, WeatherData, WeatherItem, apply as editor_apply, collect_story_areas,
    export_jpeg,
};
use tg_platform::geolocation::{GeoLocation, GeoLocationProvider, GeolocationStatus};

use crate::history::video_cover::{VideoCoverPickerOutcome, VideoCoverPickerState};
use crate::history::video_editor::VideoEditorModalState;
use tg_app_info::boxes::self_destruct::MediaTtlPreset;

use crate::history::sections_model::{
    AddPollOptionRequest, ContactAttachment, CreatePollRequest, LocationAttachment,
    MessageReportReason, POLL_MAX_OPTIONS, POLL_MIN_OPTIONS, POLL_OPTION_LIMIT,
    POLL_QUESTION_LIMIT, POLL_SOLUTION_LIMIT, POLL_WARN_OPTION_LIMIT, POLL_WARN_QUESTION_LIMIT,
    POLL_WARN_SOLUTION_LIMIT, PollMessageExtras, PollOptionAttachment, ScheduleDateTime,
    ScheduleTarget, VenueItem, days_in_month, month_name, validate_create_poll,
    validate_poll_link_url, validate_schedule_target, weekday_monday_zero,
};
use crate::history::theme::HistoryColors;
use tg_app_core::core_types::platform_call::{
    PLATFORM_CALL_REPAINT_INTERVAL, PlatformCall, PlatformCallState,
};
use tg_app_core::stand_in::types::MsgId;

// ---------------------------------------------------------------------------
// 1. Schedule Message Box (`history_view_schedule_box.cpp`)
// ---------------------------------------------------------------------------

/// Operating mode of [`ScheduleBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleBoxMode {
    /// Schedule a new outgoing message.
    ScheduleMessage,
    /// Set a reminder in Saved Messages.
    SetReminder,
    /// Reschedule an existing scheduled message.
    Reschedule(MsgId),
}

/// Action emitted by [`ScheduleBoxState::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleBoxAction {
    /// Confirmed with chosen [`ScheduleTarget`].
    Confirmed {
        /// Mode (schedule new, reminder, or reschedule).
        mode: ScheduleBoxMode,
        /// Selected timestamp or "Send when online".
        target: ScheduleTarget,
    },
    /// User cancelled.
    Cancelled,
}

/// Stateful Schedule Date/Time Picker modal (`HistoryView::PrepareScheduleBox`).
#[derive(Debug, Clone)]
pub struct ScheduleBoxState {
    /// Current modal mode.
    pub mode: ScheduleBoxMode,
    /// Reference current Unix UTC time for validation.
    pub now_unix: i32,
    /// Currently displayed calendar year.
    pub view_year: i32,
    /// Currently displayed calendar month (`1..=12`).
    pub view_month: u32,
    /// Selected date/time.
    pub selected: ScheduleDateTime,
    /// Whether "Send when online" is offered (`CanScheduleUntilOnline`).
    pub can_send_when_online: bool,
    /// Optional recipient name for "Send when <name> is online" label.
    pub recipient_name: String,
    /// Optional validation error text.
    pub error: Option<String>,
}

impl ScheduleBoxState {
    /// Creates a Schedule Box pre-populated with `default_schedule_time(now_unix)` (`now + 10 min`).
    #[must_use]
    pub fn new(
        mode: ScheduleBoxMode,
        now_unix: i32,
        can_send_when_online: bool,
        recipient_name: impl Into<String>,
    ) -> Self {
        let initial_ts = default_schedule_time(now_unix);
        let dt = ScheduleDateTime::from_unix_utc(i64::from(initial_ts));
        Self {
            mode,
            now_unix,
            view_year: dt.year,
            view_month: dt.month,
            selected: dt,
            can_send_when_online,
            recipient_name: recipient_name.into(),
            error: None,
        }
    }

    /// Creates a Schedule Box pre-filled for rescheduling an existing timestamp.
    #[must_use]
    pub fn for_reschedule(
        msg_id: MsgId,
        current_ts: i32,
        now_unix: i32,
        can_send_when_online: bool,
        recipient_name: impl Into<String>,
    ) -> Self {
        let ts = if current_ts <= now_unix
            || current_ts == tg_chat::send_options::SCHEDULED_UNTIL_ONLINE_TIMESTAMP
        {
            default_schedule_time(now_unix)
        } else {
            current_ts
        };
        let dt = ScheduleDateTime::from_unix_utc(i64::from(ts));
        Self {
            mode: ScheduleBoxMode::Reschedule(msg_id),
            now_unix,
            view_year: dt.year,
            view_month: dt.month,
            selected: dt,
            can_send_when_online,
            recipient_name: recipient_name.into(),
            error: None,
        }
    }

    /// Moves calendar view to the previous month.
    pub fn prev_month(&mut self) {
        if self.view_month <= 1 {
            self.view_month = 12;
            self.view_year -= 1;
        } else {
            self.view_month -= 1;
        }
    }

    /// Moves calendar view to the next month.
    pub fn next_month(&mut self) {
        if self.view_month >= 12 {
            self.view_month = 1;
            self.view_year += 1;
        } else {
            self.view_month += 1;
        }
    }

    /// Validates and builds the current [`ScheduleTarget::Timestamp`].
    pub fn confirm_timestamp(&mut self) -> Result<ScheduleTarget, String> {
        let ts = self.selected.to_unix_utc() as i32;
        let target = ScheduleTarget::Timestamp(ts);
        match validate_schedule_target(target, self.now_unix) {
            Ok(()) => {
                self.error = None;
                Ok(target)
            }
            Err(err) => {
                self.error = Some(err.clone()); // local error: checked before any request
                Err(err)
            }
        }
    }

    /// Paints the Schedule Box modal centered over `viewport_rect` (or directly in `ui` when `embedded`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
        embedded: bool,
    ) -> Option<ScheduleBoxAction> {
        let mut action = None;
        let draw_body = |ui: &mut Ui, state: &mut Self, action: &mut Option<ScheduleBoxAction>| {
            let title = match state.mode {
                ScheduleBoxMode::ScheduleMessage => "Schedule Message",
                ScheduleBoxMode::SetReminder => "Set a Reminder",
                ScheduleBoxMode::Reschedule(_) => "Reschedule Message",
            };
            ui.label(
                egui::RichText::new(title)
                    .strong()
                    .size(16.0)
                    .color(colors.bold_fg),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("Selected: {}", state.selected.format_display()))
                    .size(12.5)
                    .color(colors.active_text_fg),
            );
            ui.add_space(8.0);

            // Quick presets row
            ui.horizontal(|ui| {
                for (label, offset_secs) in [
                    ("In 30 min", 1800i32),
                    ("In 1 hour", 3600),
                    ("Tomorrow 09:00", -1),
                ] {
                    if ui.small_button(label).clicked() {
                        let ts = if offset_secs > 0 {
                            state.now_unix + offset_secs
                        } else {
                            let mut d =
                                ScheduleDateTime::from_unix_utc(i64::from(state.now_unix) + 86_400);
                            d.hour = 9;
                            d.minute = 0;
                            d.to_unix_utc() as i32
                        };
                        let dt = ScheduleDateTime::from_unix_utc(i64::from(ts));
                        state.selected = dt;
                        state.view_year = dt.year;
                        state.view_month = dt.month;
                        state.error = None;
                    }
                }
            });

            ui.add_space(8.0);

            // Month navigation header
            ui.horizontal(|ui| {
                if tg_app_core::ui_icons::small_icon_button(
                    ui,
                    "Previous month",
                    true,
                    |p, c, s, col| {
                        tg_app_core::ui_icons::paint_triangle(
                            p,
                            c,
                            s,
                            tg_app_core::ui_icons::ArrowDir::Left,
                            col,
                        );
                    },
                )
                .clicked()
                {
                    state.prev_month();
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} {}",
                        month_name(state.view_month),
                        state.view_year
                    ))
                    .strong()
                    .size(13.5)
                    .color(colors.bold_fg),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if tg_app_core::ui_icons::small_icon_button(
                        ui,
                        "Next month",
                        true,
                        |p, c, s, col| {
                            tg_app_core::ui_icons::paint_triangle(
                                p,
                                c,
                                s,
                                tg_app_core::ui_icons::ArrowDir::Right,
                                col,
                            );
                        },
                    )
                    .clicked()
                    {
                        state.next_month();
                    }
                });
            });

            ui.add_space(6.0);

            // Calendar weekday header + days grid
            let cell_w = 40.0;
            let cell_h = 30.0;
            let today = ScheduleDateTime::from_unix_utc(i64::from(state.now_unix));
            egui::Grid::new("tg_schedule_calendar_grid")
                .spacing(Vec2::new(4.0, 4.0))
                .show(ui, |ui| {
                    for wd in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                        let (r, _) =
                            ui.allocate_exact_size(Vec2::new(cell_w, 20.0), Sense::hover());
                        ui.painter().text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            wd,
                            FontId::proportional(11.5),
                            colors.sub_text_fg,
                        );
                    }
                    ui.end_row();

                    let first_wd = weekday_monday_zero(state.view_year, state.view_month, 1);
                    let total_days = days_in_month(state.view_year, state.view_month);
                    let mut col = 0u32;
                    for _ in 0..first_wd {
                        ui.allocate_exact_size(Vec2::new(cell_w, cell_h), Sense::hover());
                        col += 1;
                    }
                    for day in 1..=total_days {
                        let is_selected = state.selected.year == state.view_year
                            && state.selected.month == state.view_month
                            && state.selected.day == day;
                        let is_today = today.year == state.view_year
                            && today.month == state.view_month
                            && today.day == day;
                        let (r, resp) =
                            ui.allocate_exact_size(Vec2::new(cell_w, cell_h), Sense::click());
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if is_selected {
                            ui.painter().circle_filled(r.center(), 14.0, colors.accent);
                        } else if resp.hovered() {
                            ui.painter()
                                .circle_filled(r.center(), 14.0, colors.bar_bg_over);
                        } else if is_today {
                            ui.painter().circle_stroke(
                                r.center(),
                                14.0,
                                Stroke::new(1.2, colors.accent),
                            );
                        }
                        ui.painter().text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            day.to_string(),
                            FontId::proportional(12.5),
                            if is_selected {
                                Color32::WHITE
                            } else {
                                colors.text_fg
                            },
                        );
                        if resp.clicked() {
                            state.selected.year = state.view_year;
                            state.selected.month = state.view_month;
                            state.selected.day = day;
                            state.error = None;
                        }
                        col += 1;
                        if col.is_multiple_of(7) {
                            ui.end_row();
                        }
                    }
                });

            ui.add_space(10.0);

            // Time picker row (Hour : Minute with steppers)
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Time (UTC):")
                        .size(13.0)
                        .color(colors.text_fg),
                );
                ui.add_space(8.0);
                if ui.small_button("−").clicked() {
                    state.selected.hour = (state.selected.hour + 23) % 24;
                    state.error = None;
                }
                ui.label(
                    egui::RichText::new(format!("{:02}", state.selected.hour))
                        .monospace()
                        .strong()
                        .size(15.0)
                        .color(colors.bold_fg),
                );
                if ui.small_button("+").clicked() {
                    state.selected.hour = (state.selected.hour + 1) % 24;
                    state.error = None;
                }

                ui.label(egui::RichText::new(":").strong().size(15.0));

                if ui.small_button("−").clicked() {
                    state.selected.minute = (state.selected.minute + 55) % 60;
                    state.error = None;
                }
                ui.label(
                    egui::RichText::new(format!("{:02}", state.selected.minute))
                        .monospace()
                        .strong()
                        .size(15.0)
                        .color(colors.bold_fg),
                );
                if ui.small_button("+").clicked() {
                    state.selected.minute = (state.selected.minute + 5) % 60;
                    state.error = None;
                }
            });

            if let Some(ref err) = state.error {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(colors.attention_fg),
                );
            }

            if state.can_send_when_online {
                ui.add_space(10.0);
                let online_label = if state.recipient_name.is_empty() {
                    "Send when online".to_owned()
                } else {
                    format!("Send when {} is online", state.recipient_name)
                };
                let (r, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    r,
                    CornerRadius::same(8),
                    if resp.hovered() {
                        colors.bar_bg_over
                    } else {
                        Color32::from_rgba_unmultiplied(
                            colors.accent.r(),
                            colors.accent.g(),
                            colors.accent.b(),
                            22,
                        )
                    },
                );
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    online_label,
                    FontId::proportional(13.0),
                    colors.active_text_fg,
                );
                if resp.clicked() {
                    *action = Some(ScheduleBoxAction::Confirmed {
                        mode: state.mode,
                        target: ScheduleTarget::WhenOnline,
                    });
                }
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirm_label = match state.mode {
                        ScheduleBoxMode::SetReminder => "Set Reminder",
                        ScheduleBoxMode::Reschedule(_) => "Reschedule",
                        ScheduleBoxMode::ScheduleMessage => "Schedule",
                    };
                    if paint_primary_button(ui, confirm_label, colors)
                        && let Ok(target) = state.confirm_timestamp()
                    {
                        *action = Some(ScheduleBoxAction::Confirmed {
                            mode: state.mode,
                            target,
                        });
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        *action = Some(ScheduleBoxAction::Cancelled);
                    }
                });
            });
        };

        if embedded {
            egui::Frame::NONE
                .fill(colors.bar_bg)
                .stroke(Stroke::new(1.0, colors.divider))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(340.0);
                    draw_body(ui, self, &mut action);
                });
        } else {
            show_modal_shell(ui, "tg_schedule_box_modal", 356.0, colors, |ui| {
                draw_body(ui, self, &mut action);
            });
            if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                action = Some(ScheduleBoxAction::Cancelled);
            }
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 2. Poll / Quiz Creation Box (`boxes/create_poll_box.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`CreatePollBoxState::show`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreatePollBoxAction {
    /// Poll validated and submitted (legacy plain options).
    Submit(CreatePollRequest),
    /// Poll validated and submitted with Layer-233 extensions (open answers, option media/links).
    SubmitWithExtras(CreatePollRequest, PollMessageExtras),
    /// User cancelled.
    Cancelled,
}

/// Modal/inline URL picker for attaching a link to a poll option (`poll/poll_link_box.cpp`).
#[derive(Debug, Clone, Default)]
pub struct PollLinkBoxState {
    /// Option index being edited (`None` when used standalone inside [`AddPollOptionBoxState`]).
    pub target_option: Option<usize>,
    /// Editable URL input text.
    pub url_input: String,
    /// Optional title override.
    pub title_input: String,
    /// Validation error, if any.
    pub error: Option<String>,
}

impl PollLinkBoxState {
    /// Opens a link picker pre-filled with `initial_url`.
    #[must_use]
    pub fn new(target_option: Option<usize>, initial_url: impl Into<String>) -> Self {
        Self {
            target_option,
            url_input: initial_url.into(),
            title_input: String::new(),
            error: None,
        }
    }

    /// Validates and builds a [`PollOptionAttachment::Link`].
    pub fn validate(&mut self) -> Result<PollOptionAttachment, String> {
        match validate_poll_link_url(&self.url_input) {
            Ok(url) => {
                self.error = None;
                let title = (!self.title_input.trim().is_empty())
                    .then(|| self.title_input.trim().to_owned());
                Ok(PollOptionAttachment::Link { url, title })
            }
            Err(err) => {
                self.error = Some(err.clone()); // local error: checked before any request
                Err(err)
            }
        }
    }
}

/// Action emitted by [`AddPollOptionBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddPollOptionAction {
    /// User submitted a new poll option (`messages.addPollAnswer`).
    Submit {
        /// Target poll message ID.
        msg_id: MsgId,
        /// New option payload (text + optional media/link attachment).
        request: AddPollOptionRequest,
    },
    /// User cancelled.
    Cancelled,
}

/// Stateful modal for adding a new answer option to an open-answers poll
/// (`history/view/history_view_add_poll_option.cpp`).
#[derive(Debug, Clone)]
pub struct AddPollOptionBoxState {
    /// Target poll message ID.
    pub msg_id: MsgId,
    /// Poll question heading for context.
    pub poll_question: String,
    /// New option text (max 100 chars).
    pub text: String,
    /// Optional Layer-233 link or media attachment on this option.
    pub attachment: Option<PollOptionAttachment>,
    /// Inline link editor when attaching a URL.
    pub link_editor: Option<PollLinkBoxState>,
    /// Validation error text.
    pub error: Option<String>,
}

impl AddPollOptionBoxState {
    /// Creates a new add-poll-option modal for `msg_id`.
    #[must_use]
    pub fn new(msg_id: MsgId, poll_question: impl Into<String>) -> Self {
        Self {
            msg_id,
            poll_question: poll_question.into(),
            text: String::new(),
            attachment: None,
            link_editor: None,
            error: None,
        }
    }

    /// Validates and builds an [`AddPollOptionRequest`].
    pub fn build_request(&mut self) -> Result<AddPollOptionRequest, String> {
        let trimmed = self.text.trim();
        if trimmed.is_empty() && self.attachment.is_none() {
            let msg = "Enter an option label or attach media/link.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        let len = trimmed.chars().count();
        if len > POLL_OPTION_LIMIT {
            let msg = format!("Option is too long ({len}/{POLL_OPTION_LIMIT} characters).");
            self.error = Some(msg.clone());
            return Err(msg);
        }
        self.error = None;
        Ok(AddPollOptionRequest {
            text: trimmed.to_owned(),
            attachment: self.attachment.clone(),
        })
    }

    /// Renders the Add Poll Option modal overlay.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<AddPollOptionAction> {
        let mut action = None;
        show_modal_shell(ui, "tg_add_poll_option_modal", 380.0, colors, |ui| {
            ui.label(
                egui::RichText::new("Add Poll Option")
                    .strong()
                    .size(16.0)
                    .color(colors.bold_fg),
            );
            ui.label(
                egui::RichText::new(&self.poll_question)
                    .size(12.0)
                    .color(colors.sub_text_fg),
            );
            ui.add_space(8.0);

            ui.add(
                TextEdit::singleline(&mut self.text)
                    .desired_width(f32::INFINITY)
                    .hint_text("Option text..."),
            );
            ui.add_space(6.0);

            // Media / link attachment controls (`poll/*`)
            ui.horizontal_wrapped(|ui| {
                if tg_app_core::ui_icons::icon_label_button(
                    ui,
                    "Attach Link",
                    true,
                    |p, c, s, col| {
                        tg_app_core::ui_icons::paint_style_tinted(
                            p,
                            Rect::from_center_size(c, Vec2::splat(s)),
                            tg_ui::tg_style::st::menu_icon_link,
                            col,
                        );
                    },
                )
                .clicked()
                {
                    let initial = match &self.attachment {
                        Some(PollOptionAttachment::Link { url, .. }) => url.clone(),
                        _ => String::new(),
                    };
                    self.link_editor = Some(PollLinkBoxState::new(None, initial));
                }
                if tg_app_core::ui_icons::icon_label_button(
                    ui,
                    "Attach File...",
                    true,
                    |p, c, s, col| {
                        tg_app_core::ui_icons::paint_style_tinted(
                            p,
                            Rect::from_center_size(c, Vec2::splat(s)),
                            tg_ui::tg_style::st::history_attach.icon,
                            col,
                        );
                    },
                )
                .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Attach Media to Poll Option")
                        .pick_file()
                {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let file_name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("attachment.bin")
                                .to_owned();
                            let mime_type = mime_for_poll_file(&file_name).to_owned();
                            self.attachment = Some(PollOptionAttachment::Media {
                                file_name,
                                mime_type,
                                bytes,
                            });
                            self.error = None;
                        }
                        Err(e) => self.error = Some(format!("Failed to read file: {e}")),
                    }
                }
                if let Some(ref att) = self.attachment.clone() {
                    ui.label(
                        egui::RichText::new(att.display_badge())
                            .size(11.5)
                            .color(colors.active_text_fg),
                    );
                    if ui.small_button("×").clicked() {
                        self.attachment = None;
                    }
                }
            });

            let mut apply_attachment = None;
            let mut close_link_editor = false;
            if let Some(ref mut link_box) = self.link_editor {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut link_box.url_input)
                            .desired_width(240.0)
                            .hint_text("https://example.org"),
                    );
                    if ui.small_button("Set").clicked()
                        && let Ok(att) = link_box.validate()
                    {
                        apply_attachment = Some(att);
                        close_link_editor = true;
                    }
                    if ui.small_button("×").clicked() {
                        close_link_editor = true;
                    }
                });
                if let Some(ref err) = link_box.error {
                    ui.label(
                        egui::RichText::new(err)
                            .size(11.0)
                            .color(colors.attention_fg),
                    );
                }
            }
            if let Some(att) = apply_attachment {
                self.attachment = Some(att);
            }
            if close_link_editor {
                self.link_editor = None;
            }

            if let Some(ref err) = self.error {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(colors.attention_fg),
                );
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_primary_button(ui, "Add Option", colors)
                        && let Ok(request) = self.build_request()
                    {
                        action = Some(AddPollOptionAction::Submit {
                            msg_id: self.msg_id,
                            request,
                        });
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        action = Some(AddPollOptionAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(AddPollOptionAction::Cancelled);
        }
        action
    }
}

fn mime_for_poll_file(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}

/// Stateful Poll / Quiz Creation Box (`CreatePollBox`).
#[derive(Debug, Clone)]
pub struct CreatePollBoxState {
    /// Poll question text (max 255 chars).
    pub question: String,
    /// Dynamic list of answer choices (auto-grows up to `max_options`).
    pub options: Vec<String>,
    /// Optional per-option Layer-233 link or media attachments aligned with `options`.
    pub option_attachments: Vec<Option<PollOptionAttachment>>,
    /// Active inline link editor for one option (`poll/poll_link_box.cpp`).
    pub active_link_box: Option<PollLinkBoxState>,
    /// Anonymous voting flag.
    pub is_anonymous: bool,
    /// Allow multiple answers (disabled when `is_quiz == true`).
    pub multiple_choice: bool,
    /// Quiz mode flag (single correct answer + optional explanation).
    pub is_quiz: bool,
    /// Layer-233: allow chat members to append options (`messages.addPollAnswer`).
    pub open_answers: bool,
    /// Layer-233: shuffle option order.
    pub shuffle_answers: bool,
    /// Layer-233: prevent retracting votes.
    pub revoting_disabled: bool,
    /// Layer-233: hide vote percentages until poll closes.
    pub hide_results_until_close: bool,
    /// Selected correct answer index for Quiz Mode.
    pub correct_option: Option<usize>,
    /// Quiz explanation text (max 200 chars).
    pub explanation: String,
    /// Maximum allowed options count (default 10).
    pub max_options: usize,
    /// Optional validation error banner.
    pub error: Option<String>,
}

impl Default for CreatePollBoxState {
    fn default() -> Self {
        Self::new()
    }
}

impl CreatePollBoxState {
    /// Creates an empty poll creation box with 2 initial option fields.
    #[must_use]
    pub fn new() -> Self {
        Self {
            question: String::new(),
            options: vec![String::new(), String::new()],
            option_attachments: vec![None, None],
            active_link_box: None,
            is_anonymous: true,
            multiple_choice: false,
            is_quiz: false,
            open_answers: false,
            shuffle_answers: false,
            revoting_disabled: false,
            hide_results_until_close: false,
            correct_option: None,
            explanation: String::new(),
            max_options: POLL_MAX_OPTIONS,
            error: None,
        }
    }

    /// Toggles Quiz Mode, automatically disabling Multiple Choice when turning Quiz Mode on
    /// (matching `CreatePollBox` mutual exclusion in `create_poll_box.cpp`).
    pub fn set_quiz_mode(&mut self, enabled: bool) {
        self.is_quiz = enabled;
        if enabled {
            self.multiple_choice = false;
            self.open_answers = false;
        } else {
            self.correct_option = None;
        }
        self.error = None;
    }

    /// Toggles Multiple Choice, automatically disabling Quiz Mode when enabled.
    pub fn set_multiple_choice(&mut self, enabled: bool) {
        self.multiple_choice = enabled;
        if enabled {
            self.is_quiz = false;
            self.correct_option = None;
        }
        self.error = None;
    }

    /// Appends a new blank option slot if under `max_options`.
    pub fn add_option(&mut self) {
        if self.options.len() < self.max_options {
            self.options.push(String::new());
            self.option_attachments.push(None);
        }
    }

    /// Sets or clears the Layer-233 link/media attachment on option `idx`.
    pub fn set_option_attachment(&mut self, idx: usize, attachment: Option<PollOptionAttachment>) {
        if self.option_attachments.len() < self.options.len() {
            self.option_attachments.resize(self.options.len(), None);
        }
        if idx < self.option_attachments.len() {
            self.option_attachments[idx] = attachment;
        }
    }

    /// Removes option at `idx`, keeping at least `POLL_MIN_OPTIONS` slots and updating `correct_option`.
    pub fn remove_option(&mut self, idx: usize) {
        if self.options.len() <= POLL_MIN_OPTIONS || idx >= self.options.len() {
            return;
        }
        self.options.remove(idx);
        if idx < self.option_attachments.len() {
            self.option_attachments.remove(idx);
        }
        if let Some(corr) = self.correct_option {
            if corr == idx {
                self.correct_option = None;
            } else if corr > idx {
                self.correct_option = Some(corr - 1);
            }
        }
    }

    /// Validates and builds a [`CreatePollRequest`] plus Layer-233 [`PollMessageExtras`].
    pub fn build_request_with_extras(
        &mut self,
    ) -> Result<(CreatePollRequest, PollMessageExtras), String> {
        let mut non_empty = Vec::new();
        let mut mapped_attachments = Vec::new();
        let mut mapped_correct = None;
        for (i, opt) in self.options.iter().enumerate() {
            let trimmed = opt.trim();
            let att = self.option_attachments.get(i).cloned().flatten();
            if !trimmed.is_empty() || att.is_some() {
                if self.correct_option == Some(i) {
                    mapped_correct = Some(non_empty.len());
                }
                let label = if trimmed.is_empty() {
                    att.as_ref()
                        .map_or_else(String::new, PollOptionAttachment::display_badge)
                } else {
                    trimmed.to_owned()
                };
                non_empty.push(label);
                mapped_attachments.push(att);
            }
        }
        let req = CreatePollRequest {
            question: self.question.trim().to_owned(),
            options: non_empty,
            is_anonymous: self.is_anonymous,
            multiple_choice: self.multiple_choice,
            is_quiz: self.is_quiz,
            correct_option: mapped_correct,
            explanation: if self.is_quiz && !self.explanation.trim().is_empty() {
                Some(self.explanation.trim().to_owned())
            } else {
                None
            },
            silent: false,
            schedule_date: None,
        };
        match validate_create_poll(&req, self.max_options) {
            Ok(()) => {
                self.error = None;
                let extras = PollMessageExtras {
                    open_answers: self.open_answers && !self.is_quiz,
                    shuffle_answers: self.shuffle_answers,
                    revoting_disabled: self.revoting_disabled,
                    hide_results_until_close: self.hide_results_until_close,
                    option_attachments: mapped_attachments,
                    option_bytes: (0..req.options.len()).map(|i| vec![i as u8]).collect(),
                    option_added_by: vec![None; req.options.len()],
                    attached_media: None,
                    solution_media: None,
                };
                Ok((req, extras))
            }
            Err(err) => {
                let msg = err.message();
                self.error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Validates and builds a [`CreatePollRequest`].
    pub fn build_request(&mut self) -> Result<CreatePollRequest, String> {
        self.build_request_with_extras().map(|(req, _)| req)
    }

    /// Renders the Poll / Quiz creation box (modal overlay or embedded card for screenshots).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
        embedded: bool,
    ) -> Option<CreatePollBoxAction> {
        let mut action = None;

        let draw_body = |ui: &mut Ui,
                         state: &mut Self,
                         action: &mut Option<CreatePollBoxAction>| {
            ui.label(
                egui::RichText::new(if state.is_quiz {
                    "New Quiz"
                } else {
                    "New Poll"
                })
                .strong()
                .size(16.0)
                .color(colors.bold_fg),
            );
            ui.add_space(8.0);

            // Question section + warning counter (`kQuestionLimit = 255`, warn `<= 80`)
            let q_len = state.question.chars().count();
            let q_rem = POLL_QUESTION_LIMIT as isize - q_len as isize;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Question")
                        .strong()
                        .size(12.5)
                        .color(colors.active_text_fg),
                );
                if q_rem <= POLL_WARN_QUESTION_LIMIT as isize {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("{q_rem}")).size(11.5).color(
                            if q_rem < 0 {
                                colors.attention_fg
                            } else {
                                colors.sub_text_fg
                            },
                        ));
                    });
                }
            });
            ui.add(
                TextEdit::multiline(&mut state.question)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .hint_text("Ask a question..."),
            );

            ui.add_space(10.0);

            // Answer options section
            let remaining_slots = state.max_options.saturating_sub(state.options.len());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Poll options")
                        .strong()
                        .size(12.5)
                        .color(colors.active_text_fg),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}/{} options",
                            state.options.len(),
                            state.max_options
                        ))
                        .size(11.0)
                        .color(colors.sub_text_fg),
                    );
                });
            });
            ui.add_space(4.0);

            let mut remove_idx = None;
            let opt_count = state.options.len();
            ScrollArea::vertical()
                .id_salt("tg_poll_options_scroll")
                .max_height(210.0)
                .show(ui, |ui| {
                    for i in 0..opt_count {
                        ui.horizontal(|ui| {
                            if state.is_quiz {
                                let is_corr = state.correct_option == Some(i);
                                if ui
                                    .radio(is_corr, "")
                                    .on_hover_text("Mark as correct quiz answer")
                                    .clicked()
                                {
                                    state.correct_option = Some(i);
                                    state.error = None;
                                }
                            }
                            let len = state.options[i].chars().count();
                            let rem = POLL_OPTION_LIMIT as isize - len as isize;
                            ui.add(
                                TextEdit::singleline(&mut state.options[i])
                                    .desired_width(if state.is_quiz { 208.0 } else { 236.0 })
                                    .hint_text(format!("Option {}", i + 1)),
                            );
                            if tg_app_core::ui_icons::small_icon_button(
                                ui,
                                "Attach link to poll option",
                                true,
                                |p, c, s, col| {
                                    tg_app_core::ui_icons::paint_style_tinted(
                                        p,
                                        Rect::from_center_size(c, Vec2::splat(s)),
                                        tg_ui::tg_style::st::menu_icon_link,
                                        col,
                                    );
                                },
                            )
                            .on_hover_text("Attach link to poll option")
                            .clicked()
                            {
                                let init = match state
                                    .option_attachments
                                    .get(i)
                                    .and_then(|o| o.as_ref())
                                {
                                    Some(PollOptionAttachment::Link { url, .. }) => url.clone(),
                                    _ => String::new(),
                                };
                                state.active_link_box = Some(PollLinkBoxState::new(Some(i), init));
                            }
                            if tg_app_core::ui_icons::small_icon_button(
                                ui,
                                "Attach media to poll option",
                                true,
                                |p, c, s, col| {
                                    tg_app_core::ui_icons::paint_style_tinted(
                                        p,
                                        Rect::from_center_size(c, Vec2::splat(s)),
                                        tg_ui::tg_style::st::history_attach.icon,
                                        col,
                                    );
                                },
                            )
                            .on_hover_text("Attach media to poll option")
                            .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title("Attach Media to Poll Option")
                                    .pick_file()
                                && let Ok(bytes) = std::fs::read(&path)
                            {
                                let file_name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("attachment.bin")
                                    .to_owned();
                                let mime_type = mime_for_poll_file(&file_name).to_owned();
                                state.set_option_attachment(
                                    i,
                                    Some(PollOptionAttachment::Media {
                                        file_name,
                                        mime_type,
                                        bytes,
                                    }),
                                );
                            }
                            if rem <= POLL_WARN_OPTION_LIMIT as isize {
                                ui.label(egui::RichText::new(format!("{rem}")).size(10.5).color(
                                    if rem < 0 {
                                        colors.attention_fg
                                    } else {
                                        colors.sub_text_fg
                                    },
                                ));
                            }
                            if opt_count > POLL_MIN_OPTIONS && ui.small_button("×").clicked() {
                                remove_idx = Some(i);
                            }
                        });
                        if let Some(Some(att)) = state.option_attachments.get(i).cloned() {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(
                                    egui::RichText::new(att.display_badge())
                                        .size(11.0)
                                        .color(colors.active_text_fg),
                                );
                                if ui.small_button("×").clicked() {
                                    state.set_option_attachment(i, None);
                                }
                            });
                        }
                        ui.add_space(3.0);
                    }
                });

            if let Some(ref mut link_box) = state.active_link_box.clone() {
                let mut lb = link_box.clone();
                let mut close_lb = false;
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut lb.url_input)
                            .desired_width(250.0)
                            .hint_text("https://example.org"),
                    );
                    if ui.small_button("Attach Link").clicked()
                        && let Ok(att) = lb.validate()
                    {
                        if let Some(opt_idx) = lb.target_option {
                            state.set_option_attachment(opt_idx, Some(att));
                        }
                        close_lb = true;
                    }
                    if ui.small_button("×").clicked() {
                        close_lb = true;
                    }
                });
                if let Some(ref err) = lb.error {
                    ui.label(
                        egui::RichText::new(err)
                            .size(11.0)
                            .color(colors.attention_fg),
                    );
                }
                state.active_link_box = (!close_lb).then_some(lb);
            }

            if let Some(idx) = remove_idx {
                state.remove_option(idx);
            }

            if remaining_slots > 0 {
                ui.add_space(2.0);
                if ui
                    .button(format!("+ Add an option ({remaining_slots} remaining)"))
                    .clicked()
                {
                    state.add_option();
                }
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);

            // Settings toggles
            ui.label(
                egui::RichText::new("Settings")
                    .strong()
                    .size(12.5)
                    .color(colors.active_text_fg),
            );
            ui.checkbox(&mut state.is_anonymous, "Anonymous Voting");

            let mut multi = state.multiple_choice;
            if ui
                .add_enabled(
                    !state.is_quiz,
                    egui::Checkbox::new(&mut multi, "Multiple Answers"),
                )
                .changed()
            {
                state.set_multiple_choice(multi);
            }

            let mut quiz = state.is_quiz;
            if ui.checkbox(&mut quiz, "Quiz Mode").changed() {
                state.set_quiz_mode(quiz);
            }
            if !state.is_quiz {
                ui.checkbox(&mut state.open_answers, "Allow Adding Options");
            }
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut state.shuffle_answers, "Shuffle Options");
                ui.checkbox(&mut state.revoting_disabled, "Disable Revoting");
                ui.checkbox(
                    &mut state.hide_results_until_close,
                    "Hide Results Until Close",
                );
            });
            if state.is_quiz {
                ui.label(
                    egui::RichText::new(
                        "Quiz polls have one correct answer that cannot be changed after voting.",
                    )
                    .size(11.0)
                    .color(colors.sub_text_fg),
                );
                ui.add_space(6.0);

                // Quiz Explanation field (`kSolutionLimit = 200`, warn `<= 60`)
                let e_len = state.explanation.chars().count();
                let e_rem = POLL_SOLUTION_LIMIT as isize - e_len as isize;
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Explanation (optional)")
                            .strong()
                            .size(12.0)
                            .color(colors.active_text_fg),
                    );
                    if e_rem <= POLL_WARN_SOLUTION_LIMIT as isize {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(format!("{e_rem}")).size(11.0).color(
                                if e_rem < 0 {
                                    colors.attention_fg
                                } else {
                                    colors.sub_text_fg
                                },
                            ));
                        });
                    }
                });
                ui.add(
                    TextEdit::multiline(&mut state.explanation)
                        .desired_width(f32::INFINITY)
                        .desired_rows(2)
                        .hint_text("Shown after a user selects an answer..."),
                );
            }

            if let Some(ref err) = state.error {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(colors.attention_fg),
                );
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_primary_button(ui, "Create", colors)
                        && let Ok((req, extras)) = state.build_request_with_extras()
                    {
                        let has_extras = extras.open_answers
                            || extras.shuffle_answers
                            || extras.revoting_disabled
                            || extras.hide_results_until_close
                            || extras.option_attachments.iter().any(Option::is_some);
                        *action = Some(if has_extras {
                            CreatePollBoxAction::SubmitWithExtras(req, extras)
                        } else {
                            CreatePollBoxAction::Submit(req)
                        });
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        *action = Some(CreatePollBoxAction::Cancelled);
                    }
                });
            });
        };

        if embedded {
            egui::Frame::NONE
                .fill(colors.bar_bg)
                .stroke(Stroke::new(1.0, colors.divider))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(390.0);
                    draw_body(ui, self, &mut action);
                });
        } else {
            show_modal_shell(ui, "tg_create_poll_box_modal", 408.0, colors, |ui| {
                draw_body(ui, self, &mut action);
            });
            if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                action = Some(CreatePollBoxAction::Cancelled);
            }
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 3. Location Sending Box (Windows Geolocation + Telegram static map tiles)
// ---------------------------------------------------------------------------

/// Action emitted by [`LocationPickerBoxState::show`].
#[derive(Debug, Clone, PartialEq)]
pub enum LocationPickerAction {
    /// Send selected location or venue.
    Send(LocationAttachment),
    /// User cancelled.
    Cancelled,
}

/// Default width/height in logical pixels requested for Telegram static map tiles.
pub const LOCATION_MAP_TILE_WIDTH: i32 = 368;
/// Default height in logical pixels requested for Telegram static map tiles.
pub const LOCATION_MAP_TILE_HEIGHT: i32 = 170;

/// Quantized cache key `(lat_1e4, lon_1e4, zoom)` for Telegram static map tiles.
pub type StaticMapTileKey = (i32, i32, u8);

/// Stateful Location & Venues picker modal backed by `tg-platform::geolocation` and
/// `upload.getWebFile(InputWebFileGeoPointLocation)` static map tiles.
#[derive(Clone)]
pub struct LocationPickerBoxState {
    /// Selected pin latitude.
    pub lat: f64,
    /// Selected pin longitude.
    pub lon: f64,
    /// Map zoom factor (`1..=5`).
    pub zoom: u8,
    /// Optional live location period in seconds (`None` = static pin).
    pub live_period_secs: Option<u32>,
    /// Venue search filter.
    pub search_query: String,
    /// Available nearby venues.
    pub venues: Vec<VenueItem>,
    /// Injectable OS geolocation provider (`WinRtGeoLocationProvider` in live mode, fake in tests).
    geo_provider: Option<Arc<dyn GeoLocationProvider>>,
    /// Country ISO 3166-1 alpha-2 code used for the initial map centre, when resolved from the
    /// account's phone country or `help.getNearestDc` (without prompting OS geolocation).
    default_country_iso2: Option<String>,
    /// Human-readable feedback shown after clicking `"My Location"`.
    pub location_status: Option<String>,
    /// Quantized key of the most recently requested static map tile.
    tile_requested_key: Option<StaticMapTileKey>,
    /// In-flight asynchronous static map tile fetch (`upload.getWebFile`).
    tile_pending: Option<PendingMapTileFetch>,
    /// Decoded RGBA8 map tile `(width, height, rgba)` ready for egui texture upload.
    tile_rgba: Option<(u32, u32, Vec<u8>)>,
    /// Cached egui texture handle for the decoded Telegram map tile.
    tile_texture: Option<TextureHandle>,
    /// Last error returned while fetching/decoding a static map tile.
    pub tile_error: Option<String>,
    /// Signals host widgets to refresh `nearby_venues_for_coords` after a pin change.
    venues_dirty: bool,
    /// Device location fix in flight on a worker thread (`ResolveCurrentGeoLocation`).
    location_pending: Option<PlatformCall<Result<Option<GeoLocation>, String>>>,
}

/// Outcome of a device location fix requested by [`LocationPickerBoxState`].
pub type LocationFixResult = Result<Option<GeoLocation>, String>;

/// In-flight static map tile request keyed by quantized coordinates and zoom.
type PendingMapTileFetch = (StaticMapTileKey, PlatformCall<Result<Vec<u8>, String>>);

impl std::fmt::Debug for LocationPickerBoxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocationPickerBoxState")
            .field("lat", &self.lat)
            .field("lon", &self.lon)
            .field("zoom", &self.zoom)
            .field("live_period_secs", &self.live_period_secs)
            .field("search_query", &self.search_query)
            .field("venues_len", &self.venues.len())
            .field("has_geo_provider", &self.geo_provider.is_some())
            .field("default_country_iso2", &self.default_country_iso2)
            .field("tile_pending", &self.tile_pending.is_some())
            .field("has_tile_rgba", &self.tile_rgba.is_some())
            .field("tile_error", &self.tile_error)
            .field("location_pending", &self.location_pending.is_some())
            .finish()
    }
}

impl LocationPickerBoxState {
    /// Creates a location picker populated with `venues`.
    #[must_use]
    pub fn new(venues: Vec<VenueItem>) -> Self {
        Self {
            lat: 47.3769,
            lon: 8.5417,
            zoom: 3,
            live_period_secs: None,
            search_query: String::new(),
            venues,
            geo_provider: None,
            default_country_iso2: None,
            location_status: None,
            tile_requested_key: None,
            tile_pending: None,
            tile_rgba: None,
            tile_texture: None,
            tile_error: None,
            venues_dirty: false,
            location_pending: None,
        }
    }

    /// Sets the initial map pin to explicit `(lat, lon)` coordinates without prompting for OS
    /// geolocation.
    #[must_use]
    pub fn with_initial_coords(mut self, lat: f64, lon: f64) -> Self {
        if lat.is_finite() && lon.is_finite() {
            self.lat = lat.clamp(-85.0511, 85.0511);
            self.lon = lon.clamp(-180.0, 180.0);
            self.invalidate_map_tile();
        }
        self
    }

    /// Centers the initial map pin on the midpoint of `iso2` from tdesktop's country bounding
    /// boxes (`Raw::CountryBounds`), with no OS location prompt.
    #[must_use]
    pub fn with_initial_country(mut self, iso2: Option<&str>) -> Self {
        if let Some(code) = iso2
            && let Some((lat, lon)) = tg_app_core::core_types::country_bounds::country_center(code)
        {
            self.lat = lat;
            self.lon = lon;
            self.default_country_iso2 = Some(code.trim().to_ascii_uppercase());
            self.invalidate_map_tile();
        }
        self
    }

    /// Returns the ISO 3166-1 alpha-2 country code used for the default centre, if any.
    #[must_use]
    pub fn default_country_iso2(&self) -> Option<&str> {
        self.default_country_iso2.as_deref()
    }

    /// Attaches a [`GeoLocationProvider`] (production WinRT or test fake) to power `"My Location"`.
    #[must_use]
    pub fn with_geo_provider(mut self, provider: Arc<dyn GeoLocationProvider>) -> Self {
        self.geo_provider = Some(provider);
        self
    }

    /// Replaces the attached [`GeoLocationProvider`].
    pub fn set_geo_provider(&mut self, provider: Arc<dyn GeoLocationProvider>) {
        self.geo_provider = Some(provider);
    }

    /// Returns the quantized static map tile cache key for the current pin and zoom.
    #[must_use]
    pub fn current_tile_key(&self) -> StaticMapTileKey {
        (
            (self.lat * 10_000.0).round() as i32,
            (self.lon * 10_000.0).round() as i32,
            self.zoom,
        )
    }

    /// Returns `true` when no map tile fetch has been attempted yet for the current `(lat, lon, zoom)`.
    #[must_use]
    pub fn needs_tile_fetch(&self) -> bool {
        self.tile_requested_key != Some(self.current_tile_key())
    }

    /// Returns `true` when a real decoded Telegram static map tile is loaded.
    #[must_use]
    pub const fn has_loaded_map_tile(&self) -> bool {
        self.tile_rgba.is_some()
    }

    /// Returns the decoded static map tile dimensions `(width, height)`, if loaded.
    #[must_use]
    pub fn loaded_tile_dimensions(&self) -> Option<(u32, u32)> {
        self.tile_rgba.as_ref().map(|(w, h, _)| (*w, *h))
    }

    /// Invalidates the cached map tile so the next frame fetches a fresh tile for the new pin/zoom.
    pub fn invalidate_map_tile(&mut self) {
        self.tile_requested_key = None;
        self.tile_pending = None;
        self.tile_rgba = None;
        self.tile_texture = None;
        self.tile_error = None;
    }

    /// Returns `true` while an asynchronous static map tile fetch is in flight.
    #[must_use]
    pub const fn is_tile_loading(&self) -> bool {
        self.tile_pending.is_some()
    }

    /// Installs an in-flight asynchronous static map tile fetch for the current `(lat, lon, zoom)`.
    pub fn start_static_map_tile_fetch(&mut self, call: PlatformCall<Result<Vec<u8>, String>>) {
        let key = self.current_tile_key();
        self.tile_requested_key = Some(key);
        self.tile_pending = Some((key, call));
    }

    /// Polls an in-flight asynchronous static map tile fetch without blocking the UI thread.
    ///
    /// Discards stale responses if the user moved the pin or changed zoom while the RPC ran.
    pub fn poll_static_map_tile(&mut self) -> Option<Result<(), String>> {
        let outcome = match self.tile_pending.as_ref()?.1.poll() {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("Map tile loader stopped responding.".to_owned()),
        };
        let (key, _) = self.tile_pending.take()?;
        Some(self.apply_tile_fetch_outcome(key, outcome))
    }

    /// Blocks up to `timeout` for an in-flight static map tile fetch (tests only, never a UI frame).
    #[doc(hidden)]
    pub fn wait_static_map_tile(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<Result<(), String>> {
        let outcome = match self.tile_pending.as_ref()?.1.wait(timeout) {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("Map tile loader stopped responding.".to_owned()),
        };
        let (key, _) = self.tile_pending.take()?;
        Some(self.apply_tile_fetch_outcome(key, outcome))
    }

    fn apply_tile_fetch_outcome(
        &mut self,
        key: StaticMapTileKey,
        outcome: Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        if key != self.current_tile_key() {
            return Ok(());
        }
        match outcome {
            Ok(bytes) => self.set_static_map_tile_bytes(&bytes),
            Err(err) => {
                self.set_static_map_tile_error(err.clone());
                Err(err)
            }
        }
    }

    /// Returns `true` once after the pin moved (e.g. via `"My Location"` or map click) so the host
    /// can refresh nearby venues via `nearby_venues_for_coords`.
    pub fn take_venues_dirty(&mut self) -> bool {
        std::mem::take(&mut self.venues_dirty)
    }

    /// Decodes and installs static map tile PNG/JPEG bytes fetched via `upload.getWebFile`.
    pub fn set_static_map_tile_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let key = self.current_tile_key();
        self.tile_requested_key = Some(key);
        let decoded = image::load_from_memory(bytes)
            .map_err(|e| format!("Failed to decode static map tile image: {e}"))
            .and_then(|img| {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if w == 0 || h == 0 {
                    Err("Decoded static map tile has zero dimensions.".to_owned())
                } else {
                    Ok((w, h, rgba.into_raw()))
                }
            });
        match decoded {
            Ok((w, h, rgba)) => {
                self.tile_rgba = Some((w, h, rgba));
                self.tile_texture = None;
                self.tile_error = None;
                Ok(())
            }
            Err(err) => {
                self.tile_rgba = None;
                self.tile_texture = None;
                self.tile_error = Some(err.clone()); // local error: image decode
                Err(err)
            }
        }
    }

    /// Records that fetching the static map tile for the current coordinates failed.
    pub fn set_static_map_tile_error(&mut self, error: impl Into<String>) {
        self.tile_requested_key = Some(self.current_tile_key());
        self.tile_rgba = None;
        self.tile_texture = None;
        self.tile_error = Some(error.into());
    }

    /// Fetches a static map tile using `fetcher` when [`Self::needs_tile_fetch`] is `true`.
    ///
    /// Returns `true` if a new tile was fetched and decoded successfully.
    pub fn sync_static_map_tile_with<F>(&mut self, fetcher: F) -> bool
    where
        F: FnOnce(f64, f64, i32, i32, u8, u8) -> Result<Vec<u8>, String>,
    {
        if !self.needs_tile_fetch() {
            return false;
        }
        match fetcher(
            self.lat,
            self.lon,
            LOCATION_MAP_TILE_WIDTH,
            LOCATION_MAP_TILE_HEIGHT,
            self.zoom,
            1,
        ) {
            Ok(bytes) => self.set_static_map_tile_bytes(&bytes).is_ok(),
            Err(err) => {
                self.set_static_map_tile_error(err);
                false
            }
        }
    }

    /// Starts resolving the device's current coordinates through the attached
    /// [`GeoLocationProvider`] on a worker thread (`LocationPicker::resolveCurrentLocation`), so
    /// a Windows location consent prompt or a slow GPS fix never freezes the window.
    ///
    /// [`Self::poll_current_location`] moves the pin once the fix arrives. Returns `true` when a
    /// lookup was started.
    pub fn start_current_location_request(&mut self, repaint: Option<egui::Context>) -> bool {
        if self.location_pending.is_some() {
            return false;
        }
        let Some(provider) = self.geo_provider.clone() else {
            self.location_status = Some("Location services are not configured.".to_owned());
            return false;
        };
        let status = provider.status();
        if matches!(
            status,
            GeolocationStatus::Disabled | GeolocationStatus::NotAvailable
        ) {
            self.location_status = Some(match status {
                GeolocationStatus::Disabled => {
                    "Windows Location Services are disabled in Settings.".to_owned()
                }
                _ => "Device geolocation is unavailable.".to_owned(),
            });
            return false;
        }
        self.location_status = Some("Locating\u{2026}".to_owned());
        self.location_pending = Some(PlatformCall::spawn(
            "geolocation-picker",
            repaint,
            move || {
                provider
                    .resolve_current_exact_location()
                    .map_err(|e| e.to_string())
            },
        ));
        true
    }

    /// Returns `true` while a device location fix is in flight.
    #[must_use]
    pub const fn is_locating(&self) -> bool {
        self.location_pending.is_some()
    }

    /// Applies the device location fix started by [`Self::start_current_location_request`]
    /// once it is ready (moving the pin), or returns `None` while it is still running.
    pub fn poll_current_location(&mut self) -> Option<LocationFixResult> {
        let outcome = match self.location_pending.as_ref()?.poll() {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the location service stopped responding".to_owned()),
        };
        self.location_pending = None;
        Some(self.apply_location_fix(outcome))
    }

    /// Blocks up to `timeout` for the device location fix (tests only, never a UI frame).
    #[doc(hidden)]
    pub fn wait_current_location(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<LocationFixResult> {
        let outcome = match self.location_pending.as_ref()?.wait(timeout) {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the location service stopped responding".to_owned()),
        };
        self.location_pending = None;
        Some(self.apply_location_fix(outcome))
    }

    fn apply_location_fix(&mut self, outcome: LocationFixResult) -> LocationFixResult {
        match outcome {
            Ok(Some(loc)) if loc.point.is_valid() => {
                self.lat = loc.point.latitude.clamp(-85.0511, 85.0511);
                self.lon = loc.point.longitude.clamp(-180.0, 180.0);
                self.invalidate_map_tile();
                self.venues_dirty = true;
                self.location_status = Some(match loc.horizontal_accuracy_meters {
                    Some(acc) => format!("Current location updated (±{acc:.0} m)"),
                    None => "Current location updated".to_owned(),
                });
                Ok(Some(loc))
            }
            Ok(_) => {
                self.location_status =
                    Some("Could not determine current device location.".to_owned());
                Ok(None)
            }
            Err(err) => {
                let msg = format!("Location lookup failed: {err}");
                self.location_status = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Renders the location picker modal.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<LocationPickerAction> {
        let mut action = None;
        if self.location_pending.is_some() && self.poll_current_location().is_none() {
            ui.ctx()
                .request_repaint_after(PLATFORM_CALL_REPAINT_INTERVAL);
        }
        if self.tile_pending.is_some() && self.poll_static_map_tile().is_none() {
            ui.ctx()
                .request_repaint_after(PLATFORM_CALL_REPAINT_INTERVAL);
        }
        let is_tile_loading = self.tile_pending.is_some();
        show_modal_shell(ui, "tg_location_picker_modal", 400.0, colors, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Send Location")
                        .strong()
                        .size(16.0)
                        .color(colors.bold_fg),
                );
                if let Some(ref iso2) = self.default_country_iso2
                    && self.location_status.is_none()
                {
                    let country_label =
                        tg_app_core::core_types::country_bounds::country_display_name(iso2)
                            .unwrap_or_else(|| iso2.clone());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(country_label)
                                .size(11.5)
                                .color(colors.sub_text_fg),
                        );
                    });
                }
            });
            ui.add_space(6.0);

            // Upload decoded static map tile texture when available.
            if self.tile_texture.is_none()
                && let Some((w, h, ref rgba)) = self.tile_rgba
            {
                let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba);
                let key = self.current_tile_key();
                let tex = ui.ctx().load_texture(
                    format!("tg_static_map_{}_{}_{}", key.0, key.1, key.2),
                    image,
                    TextureOptions::LINEAR,
                );
                self.tile_texture = Some(tex);
            }

            // Interactive map canvas (Telegram static map tile when loaded, clean vector fallback otherwise).
            let (map_rect, map_resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 170.0), Sense::click());
            if let Some(ref tex) = self.tile_texture {
                ui.painter().image(
                    tex.id(),
                    map_rect,
                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                paint_center_pin(ui.painter(), map_rect.center(), colors);
            } else {
                paint_map_fallback_canvas(
                    ui.painter(),
                    map_rect,
                    self.lat,
                    self.lon,
                    self.zoom,
                    colors,
                );
            }
            if is_tile_loading {
                let pill_rect = Rect::from_min_size(
                    Pos2::new(map_rect.right() - 102.0, map_rect.top() + 8.0),
                    Vec2::new(94.0, 22.0),
                );
                ui.painter().rect_filled(
                    pill_rect,
                    CornerRadius::same(11),
                    Color32::from_rgba_unmultiplied(18, 24, 34, 190),
                );
                ui.painter().text(
                    pill_rect.center(),
                    Align2::CENTER_CENTER,
                    "Loading map\u{2026}",
                    FontId::proportional(10.5),
                    Color32::WHITE,
                );
            }
            if map_resp.clicked()
                && let Some(ptr) = map_resp.interact_pointer_pos()
            {
                let nx = ((ptr.x - map_rect.center().x) / map_rect.width()) as f64;
                let ny = ((map_rect.center().y - ptr.y) / map_rect.height()) as f64;
                let scale = 0.04 / f64::from(self.zoom);
                self.lat = (self.lat + ny * scale).clamp(-85.0, 85.0);
                self.lon = (self.lon + nx * scale).clamp(-180.0, 180.0);
                self.invalidate_map_tile();
                self.venues_dirty = true;
            }

            ui.add_space(6.0);
            let compact = ui.available_width() < 365.0;
            if compact {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Pin: {:.4}, {:.4}", self.lat, self.lon))
                            .monospace()
                            .size(11.0)
                            .color(colors.sub_text_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+").clicked() && self.zoom < 5 {
                            self.zoom += 1;
                            self.invalidate_map_tile();
                        }
                        if ui.small_button("−").clicked() && self.zoom > 1 {
                            self.zoom -= 1;
                            self.invalidate_map_tile();
                        }
                        ui.label(
                            egui::RichText::new(format!("{}x", self.zoom))
                                .size(11.0)
                                .color(colors.sub_text_fg),
                        );
                    });
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let locating = self.location_pending.is_some();
                    if ui
                        .add_enabled(!locating, egui::Button::new("My Location").small())
                        .clicked()
                    {
                        self.start_current_location_request(Some(ui.ctx().clone()));
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Pin: {:.4}, {:.4}", self.lat, self.lon))
                            .monospace()
                            .size(11.5)
                            .color(colors.sub_text_fg),
                    );
                    let locating = self.location_pending.is_some();
                    if ui
                        .add_enabled(!locating, egui::Button::new("My Location").small())
                        .clicked()
                    {
                        self.start_current_location_request(Some(ui.ctx().clone()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+").clicked() && self.zoom < 5 {
                            self.zoom += 1;
                            self.invalidate_map_tile();
                        }
                        if ui.small_button("−").clicked() && self.zoom > 1 {
                            self.zoom -= 1;
                            self.invalidate_map_tile();
                        }
                        ui.label(
                            egui::RichText::new(format!("Zoom {}x", self.zoom))
                                .size(11.0)
                                .color(colors.sub_text_fg),
                        );
                    });
                });
            }

            if let Some(ref status) = self.location_status {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(status)
                        .size(11.0)
                        .color(colors.sub_text_fg),
                );
            }

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("Live location:")
                        .size(12.0)
                        .color(colors.bold_fg),
                );
                for (lbl, val) in [
                    ("Off", None),
                    ("15m", Some(900)),
                    ("1h", Some(3600)),
                    ("8h", Some(28800)),
                ] {
                    if ui
                        .selectable_label(self.live_period_secs == val, lbl)
                        .clicked()
                    {
                        self.live_period_secs = val;
                    }
                }
            });

            ui.add_space(6.0);
            ui.add(
                TextEdit::singleline(&mut self.search_query)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search nearby places\u{2026}"),
            );
            ui.add_space(4.0);

            let q = self.search_query.trim().to_lowercase();
            ScrollArea::vertical()
                .id_salt("tg_venues_scroll")
                .max_height(130.0)
                .show(ui, |ui| {
                    for v in &self.venues {
                        if !q.is_empty()
                            && !v.title.to_lowercase().contains(&q)
                            && !v.address.to_lowercase().contains(&q)
                            && !v.category.to_lowercase().contains(&q)
                        {
                            continue;
                        }
                        let (r, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 44.0),
                            Sense::click(),
                        );
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, &v.title)
                        });
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter()
                                .rect_filled(r, CornerRadius::same(6), colors.bar_bg_over);
                        }
                        // Crisp vector pin badge matching `pickLocationVenueItem` (`location_picker.style`).
                        let pin_center = Pos2::new(r.left() + 18.0, r.center().y);
                        ui.painter().circle_filled(
                            pin_center,
                            12.0,
                            colors.accent.gamma_multiply(0.16),
                        );
                        ui.painter().circle_filled(pin_center, 5.0, colors.accent);
                        ui.painter().circle_filled(pin_center, 2.0, Color32::WHITE);
                        let text_x = r.left() + 38.0;
                        let text_clip = Rect::from_min_max(
                            Pos2::new(text_x, r.top()),
                            Pos2::new(r.right() - 6.0, r.bottom()),
                        );
                        ui.painter().with_clip_rect(text_clip).text(
                            Pos2::new(text_x, r.top() + 6.0),
                            Align2::LEFT_TOP,
                            format!("{} · {}", v.title, v.category),
                            FontId::proportional(12.5),
                            colors.bold_fg,
                        );
                        ui.painter().with_clip_rect(text_clip).text(
                            Pos2::new(text_x, r.top() + 24.0),
                            Align2::LEFT_TOP,
                            &v.address,
                            FontId::proportional(11.0),
                            colors.sub_text_fg,
                        );
                        if resp.clicked() {
                            action = Some(LocationPickerAction::Send(LocationAttachment {
                                lat: v.lat,
                                lon: v.lon,
                                venue_title: Some(v.title.clone()),
                                venue_address: Some(v.address.clone()),
                                live_period_secs: None,
                            }));
                        }
                    }
                });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_primary_button(ui, "Send Selected Pin", colors) {
                        action = Some(LocationPickerAction::Send(LocationAttachment {
                            lat: self.lat,
                            lon: self.lon,
                            venue_title: None,
                            venue_address: None,
                            live_period_secs: self.live_period_secs,
                        }));
                    }
                    ui.add_space(8.0);
                    if paint_secondary_button(ui, &tg_lang::tr::lng_cancel(), colors, None) {
                        action = Some(LocationPickerAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(LocationPickerAction::Cancelled);
        }
        action
    }
}

fn paint_center_pin(painter: &egui::Painter, center: Pos2, colors: &HistoryColors) {
    painter.circle_filled(center, 11.0, colors.accent);
    painter.circle_filled(center, 4.5, Color32::WHITE);
}

fn paint_map_fallback_canvas(
    painter: &egui::Painter,
    rect: Rect,
    lat: f64,
    lon: f64,
    zoom: u8,
    colors: &HistoryColors,
) {
    let land = if colors.is_night {
        Color32::from_rgb(0x23, 0x32, 0x42)
    } else {
        Color32::from_rgb(0xE8, 0xF0, 0xE6)
    };
    let water = if colors.is_night {
        Color32::from_rgb(0x18, 0x26, 0x35)
    } else {
        Color32::from_rgb(0xB8, 0xDA, 0xF2)
    };
    let road = if colors.is_night {
        Color32::from_rgb(0x36, 0x48, 0x5C)
    } else {
        Color32::WHITE
    };

    painter.rect_filled(rect, CornerRadius::same(8), land);
    let river = Rect::from_min_max(
        Pos2::new(rect.left(), rect.bottom() - 42.0),
        Pos2::new(rect.right(), rect.bottom() - 14.0),
    );
    painter.rect_filled(river, CornerRadius::ZERO, water);

    let step = (24.0 + f32::from(zoom) * 6.0).min(54.0);
    let mut x = rect.left() + step;
    while x < rect.right() {
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.5, road),
        );
        x += step;
    }
    let mut y = rect.top() + step;
    while y < rect.bottom() {
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.5, road),
        );
        y += step;
    }

    paint_center_pin(painter, rect.center(), colors);
    painter.text(
        Pos2::new(rect.left() + 8.0, rect.top() + 6.0),
        Align2::LEFT_TOP,
        format!("Map ({lat:.4}, {lon:.4})"),
        FontId::proportional(10.5),
        colors.sub_text_fg,
    );
}

// ---------------------------------------------------------------------------
// 4. Share Contact Box
// ---------------------------------------------------------------------------

/// Action emitted by [`ShareContactBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareContactAction {
    /// Share chosen contact card.
    Share(ContactAttachment),
    /// User cancelled.
    Cancelled,
}

/// Stateful Share Contact modal dialog.
#[derive(Debug, Clone)]
pub struct ShareContactBoxState {
    /// Available contacts.
    pub contacts: Vec<ContactAttachment>,
    /// Filter query.
    pub search_query: String,
    /// Selected index in `contacts`.
    pub selected_index: Option<usize>,
    /// Editable first name override.
    pub edit_first: String,
    /// Editable last name override.
    pub edit_last: String,
}

impl ShareContactBoxState {
    /// Creates a Share Contact modal for `contacts`.
    #[must_use]
    pub fn new(contacts: Vec<ContactAttachment>) -> Self {
        let (sel, first, last) = if let Some(c) = contacts.first() {
            (Some(0), c.first_name.clone(), c.last_name.clone())
        } else {
            (None, String::new(), String::new())
        };
        Self {
            contacts,
            search_query: String::new(),
            selected_index: sel,
            edit_first: first,
            edit_last: last,
        }
    }

    /// Renders the Share Contact modal.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<ShareContactAction> {
        let mut action = None;
        show_modal_shell(ui, "tg_share_contact_modal", 360.0, colors, |ui| {
            ui.label(
                egui::RichText::new("Share Contact")
                    .strong()
                    .size(16.0)
                    .color(colors.bold_fg),
            );
            ui.add_space(6.0);
            ui.add(
                TextEdit::singleline(&mut self.search_query)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search contacts..."),
            );
            ui.add_space(6.0);

            let q = self.search_query.trim().to_lowercase();
            ScrollArea::vertical()
                .id_salt("tg_share_contacts_scroll")
                .max_height(170.0)
                .show(ui, |ui| {
                    for (idx, c) in self.contacts.iter().enumerate() {
                        let full = c.full_name();
                        if !q.is_empty()
                            && !full.to_lowercase().contains(&q)
                            && !c.phone_number.contains(&q)
                        {
                            continue;
                        }
                        let is_sel = self.selected_index == Some(idx);
                        let (r, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 42.0),
                            Sense::click(),
                        );
                        if resp.hovered() || is_sel {
                            ui.painter()
                                .rect_filled(r, CornerRadius::same(6), colors.bar_bg_over);
                        }
                        ui.painter().text(
                            Pos2::new(r.left() + 10.0, r.top() + 5.0),
                            Align2::LEFT_TOP,
                            &full,
                            FontId::proportional(13.0),
                            colors.bold_fg,
                        );
                        ui.painter().text(
                            Pos2::new(r.left() + 10.0, r.top() + 23.0),
                            Align2::LEFT_TOP,
                            &c.phone_number,
                            FontId::proportional(11.5),
                            colors.sub_text_fg,
                        );
                        if is_sel {
                            tg_app_core::ui_icons::paint_select_check(
                                ui.painter(),
                                Pos2::new(r.right() - 16.0, r.center().y),
                                14.0,
                                colors.accent,
                            );
                        }
                        if resp.clicked() {
                            self.selected_index = Some(idx);
                            self.edit_first = c.first_name.clone();
                            self.edit_last = c.last_name.clone();
                        }
                    }
                });

            if let Some(idx) = self.selected_index
                && let Some(selected) = self.contacts.get(idx).cloned()
            {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("First:");
                    ui.add(TextEdit::singleline(&mut self.edit_first).desired_width(110.0));
                    ui.label("Last:");
                    ui.add(TextEdit::singleline(&mut self.edit_last).desired_width(110.0));
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if paint_primary_button(ui, "Share", colors) {
                            let first = if self.edit_first.trim().is_empty() {
                                selected.first_name.clone()
                            } else {
                                self.edit_first.trim().to_owned()
                            };
                            action = Some(ShareContactAction::Share(ContactAttachment {
                                user_id: selected.user_id,
                                first_name: first,
                                last_name: self.edit_last.trim().to_owned(),
                                phone_number: selected.phone_number,
                            }));
                        }
                        ui.add_space(8.0);
                        if ui.button("Cancel").clicked() {
                            action = Some(ShareContactAction::Cancelled);
                        }
                    });
                });
            } else if ui.button("Cancel").clicked() {
                action = Some(ShareContactAction::Cancelled);
            }
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(ShareContactAction::Cancelled);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 5. Attach Files Box (`tg-chat::media_prepare` + `tg-editor` photo editing)
// ---------------------------------------------------------------------------

/// Result emitted when submitting the Attach Files box.
#[derive(Debug, Clone)]
pub struct AttachFilesSubmission {
    /// Grouped files partitioned by `tg_chat::media_prepare::divide_by_groups`.
    pub groups: Vec<PreparedGroup>,
    /// Send files flags (`group_files`, `send_images_as_photos`, `send_large_photos`).
    pub way: SendFilesWay,
    /// Optional caption text.
    pub caption: String,
    /// True if spoiler cover is enabled.
    pub spoiler: bool,
    /// Optional self-destruct / view-once TTL in seconds (`i32::MAX` = View Once).
    pub ttl_seconds: Option<i32>,
    /// Per-file JPEG bytes produced by `tg-editor` for edited photos.
    pub edited_jpegs: HashMap<String, Vec<u8>>,
    /// Per-file video modifications produced by `tg-editor::video` for edited videos.
    pub edited_videos: HashMap<String, VideoModifications>,
    /// Per-file custom JPEG cover bytes selected via [`VideoCoverPickerState`] or video editor
    /// (`InputMediaUploadedDocument::video_cover`).
    pub video_covers: HashMap<String, Vec<u8>>,
}

/// Action emitted by [`AttachFilesBoxState::show`].
#[derive(Debug, Clone)]
pub enum AttachFilesAction {
    /// Send the prepared media groups.
    Send(AttachFilesSubmission),
    /// Stash the box's files and caption into the chat's `ComposeStash`
    /// (`SendFilesBox::stash` -> `ComposeStash::take_from_box`).
    StashFiles,
    /// Caption exceeded the active free/Premium character limit (`PremiumLimitKind::CaptionLength`).
    CaptionLimitExceeded {
        /// Current caption length in Unicode scalar values.
        length: usize,
        /// Active limit (`1024` free / `2048` Premium).
        limit: usize,
    },
    /// User cancelled.
    Cancelled,
}

/// Integrated photo editor modal backed by `tg-editor` (`PhotoModifications`, `CropPreset`,
/// `PaintScene`, `BrushStroke`, `apply`, `export_jpeg`).
pub struct PhotoEditorModalState {
    /// Target file index in [`AttachFilesBoxState`].
    pub file_index: usize,
    /// Display name of the photo being edited.
    pub file_name: String,
    /// Synthetic or decoded source RGBA image buffer (`width x height`).
    pub source_image: image::RgbaImage,
    /// Source width.
    pub width: u32,
    /// Source height.
    pub height: u32,
    /// Current `tg-editor` modifications (rotation, flip, crop, enhance, paint scene).
    pub mods: PhotoModifications,
    /// Selected crop aspect preset.
    pub crop_preset: CropPreset,
    /// Enhance strength (`0.0..=1.0`, mapped to `mods.enhance` `0..=100`).
    pub enhance_strength: f32,
    /// Active brush tool (`Pen`, `Marker`, `Arrow`).
    pub brush_tool: BrushTool,
    /// Active brush RGBA color.
    pub brush_color: [u8; 4],
    /// `tg-editor` paint scene storing strokes, text, shapes, location, weather, and story areas.
    pub paint_scene: PaintScene,
    /// Draft text input for adding a [`TextItem`] overlay.
    pub text_overlay_draft: String,
    /// Selected text overlay background style.
    pub text_overlay_style: TextStyle,
    /// Selected vector shape preset for adding a [`ShapeItem`] overlay.
    pub selected_shape: ShapeType,
    /// Whether newly added shapes are filled.
    pub shape_filled: bool,
    /// Draft location label for adding a [`LocationItem`] story area.
    pub location_draft: String,
    /// Draft temperature (°C) for adding a [`WeatherItem`] story area.
    pub weather_celsius: f64,
    /// Optional draw-to-reply metadata when annotating an incoming photo to reply.
    pub draw_to_reply: Option<DrawToReplyRequest>,
    /// In-progress stroke while dragging.
    active_stroke: Option<Vec<StrokePoint>>,
    /// Cached preview texture.
    preview_tex: Option<TextureHandle>,
    /// Dirty flag to re-rasterize via `tg-editor::apply`.
    dirty: bool,
}

impl std::fmt::Debug for PhotoEditorModalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoEditorModalState")
            .field("file_index", &self.file_index)
            .field("file_name", &self.file_name)
            .field("angle", &self.mods.angle)
            .field("flipped", &self.mods.flipped)
            .field("entries", &self.paint_scene.entries().len())
            .finish()
    }
}

impl PhotoEditorModalState {
    /// Opens a `tg-editor` photo editing session for `file_index` with a procedural preview image.
    #[must_use]
    pub fn open_for_file(file_index: usize, file_name: impl Into<String>, seed: u8) -> Self {
        let width = 240u32;
        let height = 160u32;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let fx = x as f32 / width as f32;
                let fy = y as f32 / height as f32;
                let r = (55.0 + fx * 130.0 + f32::from(seed % 40)).clamp(0.0, 255.0) as u8;
                let g = (105.0 + fy * 110.0).clamp(0.0, 255.0) as u8;
                let b = (165.0 + (1.0 - fx) * 80.0).clamp(0.0, 255.0) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let source_image = image::RgbaImage::from_raw(width, height, rgba)
            .unwrap_or_else(|| image::RgbaImage::new(width, height));
        Self {
            file_index,
            file_name: file_name.into(),
            source_image,
            width,
            height,
            mods: PhotoModifications::default(),
            crop_preset: CropPreset::Original,
            enhance_strength: 0.0,
            brush_tool: BrushTool::Pen,
            brush_color: [0xFF, 0x52, 0x52, 0xFF],
            paint_scene: PaintScene::from_size(tg_editor::Size::new(width as i32, height as i32)),
            text_overlay_draft: String::new(),
            text_overlay_style: TextStyle::SemiTransparent,
            selected_shape: ShapeType::Arrow,
            shape_filled: false,
            location_draft: "Central Plaza".to_owned(),
            weather_celsius: 22.0,
            draw_to_reply: None,
            active_stroke: None,
            preview_tex: None,
            dirty: true,
        }
    }

    /// Opens the photo editor in Draw-to-Reply mode (`history_view_draw_to_reply.cpp`).
    #[must_use]
    pub fn for_draw_to_reply(request: DrawToReplyRequest, source_image: image::RgbaImage) -> Self {
        let width = source_image.width().max(1);
        let height = source_image.height().max(1);
        Self {
            file_index: 0,
            file_name: request.file_name.clone(),
            source_image,
            width,
            height,
            mods: PhotoModifications::default(),
            crop_preset: CropPreset::Original,
            enhance_strength: 0.0,
            brush_tool: BrushTool::Pen,
            brush_color: [0xFF, 0x52, 0x52, 0xFF],
            paint_scene: PaintScene::from_size(EditorSize::new(width as i32, height as i32)),
            text_overlay_draft: String::new(),
            text_overlay_style: TextStyle::SemiTransparent,
            selected_shape: ShapeType::Arrow,
            shape_filled: false,
            location_draft: "Central Plaza".to_owned(),
            weather_celsius: 22.0,
            draw_to_reply: Some(request),
            active_stroke: None,
            preview_tex: None,
            dirty: true,
        }
    }

    /// Adds a centered [`TextItem`] overlay to `self.paint_scene`.
    pub fn add_text_overlay(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let [r, g, b, a] = self.brush_color;
        let canvas = EditorSize::new(self.width as i32, self.height as i32);
        let item = TextItem::new(
            trimmed,
            tg_editor::Color::rgba(r, g, b, a),
            (f64::from(self.height) * 0.14).clamp(14.0, 36.0),
            self.text_overlay_style,
            TextTypeface::Default,
            TextAlignment::Center,
            canvas,
            PointF::new(f64::from(self.width) * 0.5, f64::from(self.height) * 0.5),
        );
        self.paint_scene.add_text(item);
        self.dirty = true;
    }

    /// Adds a centered [`ShapeItem`] overlay to `self.paint_scene`.
    pub fn add_shape_overlay(&mut self, shape: ShapeType) {
        let [r, g, b, a] = self.brush_color;
        let size = (f64::from(self.width.min(self.height)) * 0.38).max(36.0);
        let item = ShapeItem::new(
            shape,
            tg_editor::Color::rgba(r, g, b, a),
            4.0,
            self.shape_filled,
            PointF::new(f64::from(self.width) * 0.5, f64::from(self.height) * 0.5),
            size,
        );
        self.paint_scene.add_shape(item);
        self.dirty = true;
    }

    /// Adds a [`LocationItem`] story area pill to `self.paint_scene`.
    pub fn add_location_overlay(&mut self, title: &str, lat: f64, lon: f64) {
        let canvas = EditorSize::new(self.width as i32, self.height as i32);
        let item = LocationItem::new(
            LocationData {
                lat,
                lon,
                title: title.trim().to_owned(),
                ..LocationData::default()
            },
            canvas,
            PointF::new(f64::from(self.width) * 0.5, f64::from(self.height) * 0.72),
        );
        self.paint_scene.add_location(item);
        self.dirty = true;
    }

    /// Adds a [`WeatherItem`] story area pill to `self.paint_scene`.
    pub fn add_weather_overlay(&mut self, emoji: &str, celsius: f64) {
        let canvas = EditorSize::new(self.width as i32, self.height as i32);
        let item = WeatherItem::new(
            WeatherData {
                emoji: emoji.to_owned(),
                celsius,
                color: tg_editor::Color::rgb(0x40, 0xA7, 0xE3),
            },
            canvas,
            PointF::new(f64::from(self.width) * 0.28, f64::from(self.height) * 0.25),
        );
        self.paint_scene.add_weather(item);
        self.dirty = true;
    }

    /// Adds a clickable [`StoryLinkItem`] story area pill to `self.paint_scene`.
    pub fn add_story_link_overlay(&mut self, url: &str, title: &str) {
        let canvas = EditorSize::new(self.width as i32, self.height as i32);
        let item = StoryLinkItem::new(
            url,
            title,
            canvas,
            PointF::new(f64::from(self.width) * 0.5, f64::from(self.height) * 0.82),
        );
        self.paint_scene.add_story_link(item);
        self.dirty = true;
    }

    /// Adds an interactive [`StoryReactionItem`] sticker area to `self.paint_scene`.
    pub fn add_story_reaction_overlay(&mut self, emoji: &str) {
        let item = StoryReactionItem::new(
            emoji,
            false,
            40.0,
            PointF::new(f64::from(self.width) * 0.75, f64::from(self.height) * 0.30),
        );
        self.paint_scene.add_story_reaction(item);
        self.dirty = true;
    }

    /// Rotates clockwise by 90 degrees via `tg_editor::normalize_angle_90`.
    pub fn rotate_cw(&mut self) {
        self.mods.angle = tg_editor::normalize_angle_90(self.mods.angle + 90);
        self.dirty = true;
    }

    /// Toggles horizontal flip.
    pub fn toggle_flip(&mut self) {
        self.mods.flipped = !self.mods.flipped;
        self.dirty = true;
    }

    /// Applies a `CropPreset` ratio to `self.mods.crop`.
    pub fn set_crop_preset(&mut self, preset: CropPreset) {
        self.crop_preset = preset;
        let sz = tg_editor::Size::new(self.width as i32, self.height as i32);
        self.mods.crop = match preset {
            CropPreset::Original | CropPreset::Free => None,
            _ => {
                let ratio = preset.ratio(sz);
                let cur_w = f64::from(self.width);
                let cur_h = f64::from(self.height);
                let (w, h) = if cur_w / cur_h > ratio {
                    (cur_h * ratio, cur_h)
                } else {
                    (cur_w, cur_w / ratio)
                };
                Some(tg_editor::Rect::new(
                    ((cur_w - w) * 0.5).round() as i32,
                    ((cur_h - h) * 0.5).round() as i32,
                    w.round().max(1.0) as i32,
                    h.round().max(1.0) as i32,
                ))
            }
        };
        self.dirty = true;
    }

    /// Undoes the most recent paint scene stroke.
    pub fn undo(&mut self) {
        if self.paint_scene.undo() {
            self.dirty = true;
        }
    }

    /// Redoes the most recently undone paint scene stroke.
    pub fn redo(&mut self) {
        if self.paint_scene.redo() {
            self.dirty = true;
        }
    }

    /// Applies the full `tg-editor` pipeline (enhance + paint scene + crop + rotate + flip).
    #[must_use]
    pub fn render_edited_image(&self) -> image::RgbaImage {
        let mut mods = self.mods.clone();
        mods.enhance = (self.enhance_strength * 100.0).round().clamp(0.0, 100.0) as i32;
        mods.paint = self
            .paint_scene
            .has_active_items()
            .then(|| self.paint_scene.clone());
        editor_apply(&self.source_image, &mods)
    }

    /// Exports the edited image to Telegram-compliant JPEG bytes via `tg_editor::export_jpeg`.
    pub fn export_edited_jpeg(&self, high_quality: bool) -> Result<Vec<u8>, String> {
        let edited = self.render_edited_image();
        let settings = if high_quality {
            PhotoExportSettings::large()
        } else {
            PhotoExportSettings::standard()
        };
        export_jpeg(&edited, &settings)
            .map(|res| res.bytes)
            .map_err(|e| e.to_string())
    }

    /// Captures the photo editor's crop/enhance parameters and paint scene into a versioned
    /// [`tg_editor::SceneDraft`].
    #[must_use]
    pub fn build_scene_draft(&self) -> tg_editor::SceneDraft {
        let mut mods = self.mods.clone();
        mods.enhance = (self.enhance_strength * 100.0).round().clamp(0.0, 100.0) as i32;
        tg_editor::SceneDraft::new(
            tg_editor::CropEnhanceParams::from(&mods),
            self.paint_scene.clone(),
            None,
        )
    }

    /// Restores crop/enhance parameters and paint scene items from a [`tg_editor::SceneDraft`].
    pub fn apply_scene_draft(
        &mut self,
        draft: &tg_editor::SceneDraft,
    ) -> Result<(), tg_editor::EditorError> {
        draft.validate()?;
        draft.crop_enhance.apply_to(&mut self.mods);
        self.enhance_strength = (f32::from(draft.crop_enhance.enhance.clamp(0, 100) as u8)) / 100.0;
        self.paint_scene = draft.scene.clone();
        self.dirty = true;
        Ok(())
    }

    /// Renders the photo editor modal. Returns `Some(Some(jpeg_bytes))` on Save, `Some(None)` on Cancel.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<Option<Vec<u8>>> {
        let mut outcome = None;
        if self.dirty || self.preview_tex.is_none() {
            let edited = self.render_edited_image();
            let (w, h) = edited.dimensions();
            let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], edited.as_raw());
            self.preview_tex = Some(ui.ctx().load_texture(
                format!("tg_editor_preview_{}", self.file_index),
                img,
                TextureOptions::LINEAR,
            ));
            self.dirty = false;
        }

        show_modal_shell(ui, "tg_photo_editor_modal", 430.0, colors, |ui| {
            let header_title = if self.draw_to_reply.is_some() {
                format!("Draw to Reply — {}", self.file_name)
            } else {
                format!("Photo Editor — {}", self.file_name)
            };
            ui.label(
                egui::RichText::new(header_title)
                    .strong()
                    .size(15.0)
                    .color(colors.bold_fg),
            );
            ui.add_space(6.0);

            // Preview canvas supporting freehand brush drawing
            let (canvas, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 220.0), Sense::drag());
            ui.painter().rect_filled(
                canvas,
                CornerRadius::same(8),
                Color32::from_rgb(0x14, 0x18, 0x1E),
            );
            if let Some(ref tex) = self.preview_tex {
                let sz = tex.size_vec2();
                let scale = (canvas.width() / sz.x.max(1.0)).min(canvas.height() / sz.y.max(1.0));
                let draw_rect = Rect::from_center_size(canvas.center(), sz * scale);
                ui.painter().image(
                    tex.id(),
                    draw_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );

                // Drag to paint freehand `tg-editor` brush strokes
                if let Some(ptr) = resp.interact_pointer_pos() {
                    let nx = ((ptr.x - draw_rect.left()) / draw_rect.width().max(1.0))
                        .clamp(0.0, 1.0)
                        * self.width as f32;
                    let ny = ((ptr.y - draw_rect.top()) / draw_rect.height().max(1.0))
                        .clamp(0.0, 1.0)
                        * self.height as f32;
                    if resp.drag_started() {
                        self.active_stroke =
                            Some(vec![StrokePoint::new(f64::from(nx), f64::from(ny))]);
                    } else if resp.dragged()
                        && let Some(ref mut pts) = self.active_stroke
                    {
                        pts.push(StrokePoint::new(f64::from(nx), f64::from(ny)));
                    }
                }
                if resp.drag_stopped()
                    && let Some(pts) = self.active_stroke.take()
                    && pts.len() >= 2
                {
                    let [r, g, b, a] = self.brush_color;
                    self.paint_scene.add_stroke(BrushStroke {
                        tool: self.brush_tool,
                        color: tg_editor::Color::rgba(r, g, b, a),
                        size: 8.0,
                        points: pts,
                    });
                    self.dirty = true;
                }
            }

            ui.add_space(6.0);

            // Transform & Crop controls
            ui.horizontal_wrapped(|ui| {
                if tg_app_core::ui_icons::icon_label_button(
                    ui,
                    "Rotate 90°",
                    false,
                    tg_app_core::ui_icons::paint_rotate_cw,
                )
                .clicked()
                {
                    self.rotate_cw();
                }
                if tg_app_core::ui_icons::icon_label_button(
                    ui,
                    "Flip",
                    false,
                    tg_app_core::ui_icons::paint_flip_horizontal,
                )
                .clicked()
                {
                    self.toggle_flip();
                }
                if ui
                    .selectable_label(self.crop_preset == CropPreset::Original, "Original")
                    .clicked()
                {
                    self.set_crop_preset(CropPreset::Original);
                }
                if ui
                    .selectable_label(self.crop_preset == CropPreset::Square, "1:1")
                    .clicked()
                {
                    self.set_crop_preset(CropPreset::Square);
                }
                if ui
                    .selectable_label(self.crop_preset == CropPreset::Ratio16x9, "16:9")
                    .clicked()
                {
                    self.set_crop_preset(CropPreset::Ratio16x9);
                }
            });

            // Scene items toolbar: Text, Shapes, Location, Weather & Story Areas (`tg-editor::scene_items`)
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut self.text_overlay_draft)
                        .desired_width(140.0)
                        .hint_text("Overlay text..."),
                );
                if ui.small_button("+ Text").clicked() {
                    let txt = self.text_overlay_draft.clone();
                    self.add_text_overlay(&txt);
                    self.text_overlay_draft.clear();
                }
                if ui.small_button("+ Shape").clicked() {
                    let sh = self.selected_shape;
                    self.add_shape_overlay(sh);
                }
                ui.checkbox(&mut self.shape_filled, "Fill");
            });
            ui.horizontal_wrapped(|ui| {
                for (sh, name) in [
                    (ShapeType::Circle, "Circle"),
                    (ShapeType::Rectangle, "Rectangle"),
                    (ShapeType::Star, "Star"),
                    (ShapeType::Bubble, "Bubble"),
                    (ShapeType::Arrow, "Arrow"),
                ] {
                    let selected = self.selected_shape == sh;
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(26.0, 22.0), Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, selected, name)
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if selected {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(4),
                            ui.visuals().selection.bg_fill,
                        );
                    } else if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(4), colors.bar_bg_over);
                    }
                    let fg = if selected {
                        colors.active_text_fg
                    } else {
                        colors.text_fg
                    };
                    let c = rect.center();
                    match sh {
                        ShapeType::Circle => {
                            ui.painter().circle_stroke(c, 5.5, Stroke::new(1.5, fg));
                        }
                        ShapeType::Rectangle => {
                            ui.painter().rect_stroke(
                                Rect::from_center_size(c, Vec2::splat(10.5)),
                                CornerRadius::same(1),
                                Stroke::new(1.5, fg),
                                egui::StrokeKind::Inside,
                            );
                        }
                        ShapeType::Star => {
                            tg_app_core::ui_icons::paint_star(ui.painter(), c, 6.0, fg);
                        }
                        ShapeType::Bubble => {
                            tg_app_core::ui_icons::paint_speech_bubble(ui.painter(), c, 13.0, fg);
                        }
                        ShapeType::Arrow => {
                            tg_app_core::ui_icons::paint_arrow(
                                ui.painter(),
                                c,
                                13.0,
                                tg_app_core::ui_icons::ArrowDir::Right,
                                fg,
                            );
                        }
                    }
                    if resp.on_hover_text(name).clicked() {
                        self.selected_shape = sh;
                    }
                }
                if ui.small_button("+ Location").clicked() {
                    let lbl = self.location_draft.clone();
                    self.add_location_overlay(&lbl, 47.3769, 8.5417);
                }
                if ui.small_button("+ Weather").clicked() {
                    let c = self.weather_celsius;
                    self.add_weather_overlay("☀️", c);
                }
                if ui.small_button("+ Reaction").clicked() {
                    self.add_story_reaction_overlay("🔥");
                }
                if ui.small_button("+ Link").clicked() {
                    self.add_story_link_overlay("https://example.org", "Link");
                }
            });
            let story_areas = collect_story_areas(
                &self.paint_scene,
                EditorSize::new(self.width as i32, self.height as i32),
            );
            let active_layers = self.paint_scene.active_items_by_z().len();
            if active_layers > 0 || !story_areas.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "Layers: {active_layers} · Story areas: {}",
                        story_areas.total_count()
                    ))
                    .size(11.0)
                    .color(colors.sub_text_fg),
                );
            }

            // Enhance slider & Undo/Redo
            ui.horizontal(|ui| {
                ui.label("Enhance:");
                if ui
                    .add(egui::Slider::new(&mut self.enhance_strength, 0.0..=1.0).show_value(false))
                    .changed()
                {
                    self.dirty = true;
                }
                if ui
                    .add_enabled(self.paint_scene.has_undo(), egui::Button::new("Undo"))
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(self.paint_scene.has_redo(), egui::Button::new("Redo"))
                    .clicked()
                {
                    self.redo();
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_primary_button(ui, "Apply Edit", colors)
                        && let Ok(jpeg) = self.export_edited_jpeg(false)
                    {
                        outcome = Some(Some(jpeg));
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        outcome = Some(None);
                    }
                });
            });
        });

        outcome
    }
}

/// Stateful Attach Files box powered by `tg-chat::media_prepare` album/compression rules
/// and `tg-editor` photo editing.
#[derive(Debug)]
pub struct AttachFilesBoxState {
    /// Candidate files added to the box.
    pub candidates: Vec<CandidateFile>,
    /// Prepared file list produced by `tg_chat::media_prepare::prepare_media_list`.
    pub prepared: PreparedList,
    /// Compression & album grouping preferences (`SendFilesWay`).
    pub way: SendFilesWay,
    /// Whether the target peer has slow mode enabled.
    pub slowmode: bool,
    /// Whether the current user has Premium limits.
    pub is_premium: bool,
    /// Spoiler toggle.
    pub spoiler: bool,
    /// Self-destruct / view-once media TTL preset (`Off`, `ViewOnce`, `5s..=60s`).
    pub ttl_preset: MediaTtlPreset,
    /// Caption text.
    pub caption: String,
    /// Validation errors from `prepare_media_list`.
    pub last_errors: Vec<String>,
    /// Edited JPEG payloads keyed by candidate file path.
    pub edited_jpegs: HashMap<String, Vec<u8>>,
    /// Edited video modifications keyed by candidate file path.
    pub edited_videos: HashMap<String, VideoModifications>,
    /// Custom video cover JPEG bytes keyed by candidate file path (`history_view_video_cover_uploader.cpp`).
    pub video_covers: HashMap<String, Vec<u8>>,
    /// Versioned `tg-editor` scene drafts keyed by candidate file path so reopening restores
    /// multi-clip timelines, overlay visibility windows, and paint scenes.
    pub scene_drafts: HashMap<String, tg_editor::SceneDraft>,
    /// Optional profile directory used to back [`VideoEditorModalState::draft_path`] on disk.
    pub video_drafts_dir: Option<PathBuf>,
    /// Active `tg-editor` modal when editing one photo item.
    pub active_editor: Option<PhotoEditorModalState>,
    /// Active `tg-editor::video` modal when editing one video item.
    pub active_video_editor: Option<VideoEditorModalState>,
    /// Active custom video cover frame picker modal when choosing a cover for one video item.
    pub active_cover_picker: Option<VideoCoverPickerState>,
}

impl Default for AttachFilesBoxState {
    fn default() -> Self {
        Self::new(false, false)
    }
}

impl AttachFilesBoxState {
    /// Creates an Attach Files box initialized with 2 sample photos ready for album/compression toggling.
    #[must_use]
    pub fn new(slowmode: bool, is_premium: bool) -> Self {
        let mut state = Self {
            candidates: Vec::new(),
            prepared: PreparedList::default(),
            way: SendFilesWay::new(),
            slowmode,
            is_premium,
            spoiler: false,
            ttl_preset: MediaTtlPreset::Off,
            caption: String::new(),
            last_errors: Vec::new(),
            edited_jpegs: HashMap::new(),
            edited_videos: HashMap::new(),
            video_covers: HashMap::new(),
            scene_drafts: HashMap::new(),
            video_drafts_dir: None,
            active_editor: None,
            active_video_editor: None,
            active_cover_picker: None,
        };
        state.add_sample_photo("workspace_overview.jpg", 2560, 1440, 1_420_000);
        state.add_sample_photo("architecture_diagram.png", 1920, 1080, 980_000);
        state
    }

    /// Reconstructs an Attach Files box from previously stashed [`crate::history::compose_stash::StashedFile`]
    /// entries and caption (`StashManager::applyStash` -> `openFiles`).
    #[must_use]
    pub fn from_stashed_files(
        files: &[crate::history::compose_stash::StashedFile],
        caption: &str,
        slowmode: bool,
        is_premium: bool,
    ) -> Self {
        let mut state = Self {
            candidates: Vec::new(),
            prepared: PreparedList::default(),
            way: SendFilesWay::new(),
            slowmode,
            is_premium,
            spoiler: false,
            ttl_preset: MediaTtlPreset::Off,
            caption: caption.to_owned(),
            last_errors: Vec::new(),
            edited_jpegs: HashMap::new(),
            edited_videos: HashMap::new(),
            video_covers: HashMap::new(),
            scene_drafts: HashMap::new(),
            video_drafts_dir: None,
            active_editor: None,
            active_video_editor: None,
            active_cover_picker: None,
        };
        for sf in files {
            let path = sf.path.clone().unwrap_or_else(|| sf.name.clone());
            let size = sf.size_bytes.min(i64::MAX as u64) as i64;
            let probe = if sf.mime_type.starts_with("image/") {
                ProbedMedia::Image {
                    dimensions: tg_chat::Size::new(1920, 1080),
                    animated: sf.mime_type.eq_ignore_ascii_case("image/gif"),
                }
            } else if sf.mime_type.starts_with("video/") {
                ProbedMedia::Video {
                    thumbnail_dimensions: tg_chat::Size::new(1280, 720),
                    has_audio: true,
                    is_gifv: false,
                    is_webm_sticker: false,
                    modified_as_gif: false,
                }
            } else {
                ProbedMedia::Other
            };
            state.candidates.push(CandidateFile {
                path,
                is_local: true,
                is_dir: false,
                is_readable_file: true,
                size,
                mime: sf.mime_type.clone(),
                probe,
            });
        }
        state.reprepare();
        state
    }

    /// Returns the active caption character limit (`1024` free / `2048` Premium).
    #[must_use]
    pub fn caption_limit(&self) -> usize {
        let (free_limit, premium_limit) =
            crate::business::model::PremiumLimitsData::default().caption_length;
        (if self.is_premium {
            premium_limit
        } else {
            free_limit
        }) as usize
    }

    /// Validates the current caption against [`Self::caption_limit`] and builds the
    /// [`AttachFilesAction`] for submitting the box.
    #[must_use]
    pub fn build_action_for_send(&self) -> AttachFilesAction {
        let trimmed = self.caption.trim();
        let len = trimmed.chars().count();
        let limit = self.caption_limit();
        if self
            .prepared
            .can_add_caption(self.way.send_images_as_photos())
            && len > limit
        {
            return AttachFilesAction::CaptionLimitExceeded { length: len, limit };
        }
        AttachFilesAction::Send(AttachFilesSubmission {
            groups: self.grouped_batches(),
            way: self.way,
            caption: trimmed.to_owned(),
            spoiler: self.spoiler,
            ttl_seconds: self.ttl_preset.to_ttl_seconds(),
            edited_jpegs: self.edited_jpegs.clone(),
            edited_videos: self.edited_videos.clone(),
            video_covers: self.video_covers.clone(),
        })
    }

    /// Adds a sample photo candidate and re-runs `tg_chat::media_prepare::prepare_media_list`.
    pub fn add_sample_photo(&mut self, name: &str, width: i32, height: i32, size: i64) {
        self.candidates.push(CandidateFile {
            path: name.to_owned(),
            is_local: true,
            is_dir: false,
            is_readable_file: true,
            size,
            mime: if name.ends_with(".png") {
                "image/png".to_owned()
            } else {
                "image/jpeg".to_owned()
            },
            probe: ProbedMedia::Image {
                dimensions: tg_chat::Size::new(width, height),
                animated: false,
            },
        });
        self.reprepare();
    }

    /// Adds a sample video candidate and re-runs `prepare_media_list`.
    pub fn add_sample_video(&mut self, name: &str, width: i32, height: i32, size: i64) {
        self.candidates.push(CandidateFile {
            path: name.to_owned(),
            is_local: true,
            is_dir: false,
            is_readable_file: true,
            size,
            mime: "video/mp4".to_owned(),
            probe: ProbedMedia::Video {
                thumbnail_dimensions: tg_chat::Size::new(width, height),
                has_audio: true,
                is_gifv: false,
                is_webm_sticker: false,
                modified_as_gif: false,
            },
        });
        self.reprepare();
    }

    /// Adds a sample document candidate and re-runs `prepare_media_list`.
    pub fn add_sample_document(&mut self, name: &str, mime: &str, size: i64) {
        self.candidates.push(CandidateFile {
            path: name.to_owned(),
            is_local: true,
            is_dir: false,
            is_readable_file: true,
            size,
            mime: mime.to_owned(),
            probe: ProbedMedia::Other,
        });
        self.reprepare();
    }

    /// Removes candidate at `index` and re-runs `prepare_media_list`.
    pub fn remove_item(&mut self, index: usize) {
        if index < self.candidates.len() {
            let removed = self.candidates.remove(index);
            self.edited_jpegs.remove(&removed.path);
            self.edited_videos.remove(&removed.path);
            self.video_covers.remove(&removed.path);
            self.reprepare();
        }
    }

    /// Re-runs `tg_chat::media_prepare::prepare_media_list` with current `SendFilesWay`.
    pub fn reprepare(&mut self) {
        let (mut list, errs) = prepare_media_list(
            self.candidates.clone(),
            self.is_premium,
            self.way.send_large_photos(),
            false,
        );
        // Match `SendFilesBox::enqueueNextPrepare()` (`boxes/send_files_box.cpp:900-923`):
        // `prepare_media_list` splits candidates after `MAX_ALBUM_ITEMS` (10) into
        // `files_to_process` for async detail loading in tdesktop, whereas here
        // `PreparedFile::from_probe` has already prepared every candidate synchronously.
        // Drain them into `list.files` so display, grouping, sending, and stashing
        // (up to `STASH_FILES_LIMIT = 100`) see every prepared file.
        list.files.extend(list.files_to_process.drain(..));
        self.prepared = list;
        self.last_errors = errs
            .into_iter()
            .map(|(e, p)| format!("{p}: {e:?}"))
            .collect();
    }

    /// Computes album/standalone groups via `tg_chat::media_prepare::divide_by_groups`.
    #[must_use]
    pub fn grouped_batches(&self) -> Vec<PreparedGroup> {
        divide_by_groups(self.prepared.files.clone(), self.way, self.slowmode)
    }

    /// Renders the Attach Files modal (and nested `tg-editor` photo or video editor when open).
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<AttachFilesAction> {
        if let Some(ref mut editor) = self.active_editor {
            if let Some(result) = editor.show(ui, colors) {
                let idx = editor.file_index;
                let draft = editor.build_scene_draft();
                if let Some(jpeg) = result
                    && let Some(cand) = self.candidates.get(idx)
                {
                    self.edited_jpegs.insert(cand.path.clone(), jpeg);
                    self.scene_drafts.insert(cand.path.clone(), draft);
                }
                self.active_editor = None;
            }
            return None;
        }
        if let Some(ref mut video_editor) = self.active_video_editor {
            if let Some(result) = video_editor.show(ui, colors) {
                let idx = video_editor.file_index;
                if let Some(outcome) = result
                    && let Some(cand) = self.candidates.get_mut(idx)
                {
                    let is_gif = outcome.modifications.gif;
                    if let ProbedMedia::Video {
                        ref mut modified_as_gif,
                        ..
                    } = cand.probe
                    {
                        *modified_as_gif = is_gif;
                    }
                    if let Some(cover_jpeg) = outcome.cover_jpeg {
                        self.video_covers.insert(cand.path.clone(), cover_jpeg);
                    }
                    self.edited_videos
                        .insert(cand.path.clone(), outcome.modifications);
                    if let Some(draft) = outcome.scene_draft {
                        self.scene_drafts.insert(cand.path.clone(), draft);
                    }
                    self.reprepare();
                }
                self.active_video_editor = None;
            }
            return None;
        }
        if let Some(ref mut cover_picker) = self.active_cover_picker {
            if let Some(result) = cover_picker.show(ui, colors) {
                match result {
                    VideoCoverPickerOutcome::Applied {
                        file_path,
                        cover_jpeg,
                        ..
                    } => {
                        self.video_covers.insert(file_path, cover_jpeg);
                    }
                    VideoCoverPickerOutcome::Cleared { file_path, .. } => {
                        self.video_covers.remove(&file_path);
                    }
                    VideoCoverPickerOutcome::Cancelled => {}
                }
                self.active_cover_picker = None;
            }
            return None;
        }

        let mut action = None;
        show_modal_shell(ui, "tg_attach_files_modal", 436.0, colors, |ui| {
            let groups = self.grouped_batches();
            let header = if groups.len() == 1 && groups[0].album_type != AlbumType::None {
                format!("Send as Album ({} items)", self.prepared.files.len())
            } else {
                format!("Send {} File(s)", self.prepared.files.len())
            };
            ui.label(
                egui::RichText::new(header)
                    .strong()
                    .size(16.0)
                    .color(colors.bold_fg),
            );
            ui.add_space(4.0);

            // Add sample items buttons
            ui.horizontal(|ui| {
                if ui.small_button("+ Add Photo").clicked() {
                    let n = self.candidates.len() + 1;
                    self.add_sample_photo(&format!("photo_{n}.jpg"), 1920, 1280, 1_150_000);
                }
                if ui.small_button("+ Add Video").clicked() {
                    let n = self.candidates.len() + 1;
                    self.add_sample_video(&format!("clip_{n}.mp4"), 1280, 720, 2_450_000);
                }
                if ui.small_button("+ Add PDF").clicked() {
                    let n = self.candidates.len() + 1;
                    self.add_sample_document(&format!("notes_{n}.pdf"), "application/pdf", 340_000);
                }
            });
            ui.add_space(6.0);

            // File items list with Edit Photo / Edit Video / Cover (`tg-editor`) & Remove controls
            let mut remove_idx = None;
            let mut edit_photo_idx = None;
            let mut edit_video_idx = None;
            let mut edit_cover_idx = None;
            let side_limit = photo_side_limit(self.way.send_large_photos());
            ScrollArea::vertical()
                .id_salt("tg_attach_files_scroll")
                .max_height(180.0)
                .show(ui, |ui| {
                    for (idx, file) in self.prepared.files.iter().enumerate() {
                        let is_photo = matches!(
                            file.file_type,
                            tg_chat::media_prepare::PreparedFileType::Photo
                        );
                        let is_video = matches!(
                            file.file_type,
                            tg_chat::media_prepare::PreparedFileType::Video
                        ) || file.mime.starts_with("video/");
                        let photo_edited = self.edited_jpegs.contains_key(&file.path);
                        let video_edited = self.edited_videos.get(&file.path);
                        let has_custom_cover = self.video_covers.contains_key(&file.path);
                        let dim_str = if is_photo {
                            format!(
                                "{}×{} (max {}px){}",
                                file.shown_dimensions.width,
                                file.shown_dimensions.height,
                                side_limit,
                                if photo_edited { " · Edited" } else { "" }
                            )
                        } else if is_video {
                            let mut suffix = match video_edited {
                                Some(mods) if mods.gif => " · GIF".to_owned(),
                                Some(mods) if mods.muted => " · Muted".to_owned(),
                                Some(_) => " · Edited".to_owned(),
                                None => String::new(),
                            };
                            if has_custom_cover {
                                suffix.push_str(" · Cover");
                            }
                            format!(
                                "{}×{} · {}{}",
                                file.shown_dimensions.width.max(320),
                                file.shown_dimensions.height.max(180),
                                crate::history::media::format_file_size(file.size.max(0) as u64),
                                suffix
                            )
                        } else {
                            crate::history::media::format_file_size(file.size.max(0) as u64)
                        };
                        let badge_label = if is_photo {
                            "IMG"
                        } else if is_video {
                            "VID"
                        } else {
                            "FILE"
                        };
                        let badge_bg = if is_photo {
                            colors.accent.gamma_multiply(0.16)
                        } else if is_video {
                            Color32::from_rgba_unmultiplied(0x8B, 0x5C, 0xF6, 40)
                        } else {
                            colors.bar_bg_over
                        };
                        let badge_fg = if is_photo {
                            colors.active_text_fg
                        } else if is_video {
                            Color32::from_rgb(0x7C, 0x3A, 0xED)
                        } else {
                            colors.sub_text_fg
                        };
                        let row_w = ui.available_width();
                        let has_edit = is_photo || is_video;
                        let controls_w = if is_video {
                            136.0
                        } else if has_edit {
                            74.0
                        } else {
                            30.0
                        };
                        let info_w = (row_w - controls_w - 8.0).max(110.0);
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                Vec2::new(info_w, 30.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let (br, _) = ui
                                        .allocate_exact_size(Vec2::new(34.0, 20.0), Sense::hover());
                                    ui.painter()
                                        .rect_filled(br, CornerRadius::same(4), badge_bg);
                                    ui.painter().text(
                                        br.center(),
                                        Align2::CENTER_CENTER,
                                        badge_label,
                                        FontId::proportional(9.5),
                                        badge_fg,
                                    );
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&file.path)
                                                    .size(12.5)
                                                    .color(colors.bold_fg),
                                            )
                                            .truncate(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(dim_str)
                                                    .size(10.5)
                                                    .color(colors.sub_text_fg),
                                            )
                                            .truncate(),
                                        );
                                    });
                                },
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").on_hover_text("Remove file").clicked()
                                    {
                                        remove_idx = Some(idx);
                                    }
                                    if is_photo && ui.small_button("Edit").clicked() {
                                        edit_photo_idx = Some((idx, file.path.clone()));
                                    } else if is_video {
                                        if ui.small_button("Edit").clicked() {
                                            edit_video_idx = Some((
                                                idx,
                                                file.path.clone(),
                                                file.shown_dimensions.width.max(1280),
                                                file.shown_dimensions.height.max(720),
                                                file.size.max(0) as u64,
                                            ));
                                        }
                                        let cover_btn = if has_custom_cover {
                                            tg_app_core::ui_icons::icon_label_button(
                                                ui,
                                                "Cover",
                                                true,
                                                |p, c, s, _col| {
                                                    tg_app_core::ui_icons::paint_select_check(
                                                        p,
                                                        c,
                                                        s,
                                                        colors.active_text_fg,
                                                    );
                                                },
                                            )
                                        } else {
                                            ui.small_button("Cover")
                                        }
                                        .on_hover_text(tg_lang::tr::lng_choose_cover());
                                        if cover_btn.clicked() {
                                            edit_cover_idx = Some((
                                                idx,
                                                file.path.clone(),
                                                file.shown_dimensions.width.max(1280),
                                                file.shown_dimensions.height.max(720),
                                            ));
                                        }
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                    }
                });

            if let Some(idx) = remove_idx {
                self.remove_item(idx);
            }
            if let Some((idx, name)) = edit_photo_idx {
                let mut editor = PhotoEditorModalState::open_for_file(
                    idx,
                    name.clone(),
                    (idx as u8).wrapping_mul(37),
                );
                if let Some(draft) = self.scene_drafts.get(&name) {
                    let _ = editor.apply_scene_draft(draft);
                }
                self.active_editor = Some(editor);
            }
            if let Some((idx, name, w, h, sz)) = edit_video_idx {
                let existing = self.edited_videos.get(&name).cloned();
                let mut editor = VideoEditorModalState::open_for_file(
                    idx,
                    name.clone(),
                    w,
                    h,
                    12_000,
                    sz,
                    existing,
                    (idx as u8).wrapping_mul(53),
                );
                if let Some(ref drafts_dir) = self.video_drafts_dir {
                    let draft_path = VideoEditorModalState::default_draft_path(drafts_dir, &name);
                    editor.draft_path = Some(draft_path.clone());
                    if !self.scene_drafts.contains_key(&name) && draft_path.is_file() {
                        let _ = editor.load_draft_from_path(&draft_path);
                    }
                }
                if let Some(draft) = self.scene_drafts.get(&name) {
                    let _ = editor.apply_scene_draft(draft);
                }
                self.active_video_editor = Some(editor);
            }
            if let Some((idx, name, w, h)) = edit_cover_idx {
                let existing = self.video_covers.get(&name).cloned();
                self.active_cover_picker = Some(VideoCoverPickerState::open_for_video(
                    idx,
                    name,
                    w,
                    h,
                    12_000,
                    existing,
                    (idx as u8).wrapping_mul(71),
                ));
            }

            ui.add_space(6.0);
            ui.separator();

            // `tg-chat::media_prepare` checkboxes
            let compress_opt = self
                .prepared
                .has_send_images_as_photos_option(self.slowmode);
            if compress_opt {
                let mut compress = self.way.send_images_as_photos();
                if ui.checkbox(&mut compress, "Compress images").changed() {
                    self.way.set_send_images_as_photos(compress);
                    self.reprepare();
                }
            }
            if self.prepared.has_group_option(self.slowmode) {
                let mut group = self.way.group_files();
                if ui.checkbox(&mut group, "Group items into album").changed() {
                    self.way.set_group_files(group);
                }
            }
            if self
                .prepared
                .has_send_large_photos_option(self.way.send_images_as_photos())
            {
                let mut hd = self.way.send_large_photos();
                if ui
                    .checkbox(&mut hd, "Send in High Quality (2560px)")
                    .changed()
                {
                    self.way.set_send_large_photos(hd);
                    self.reprepare();
                }
            }
            if self
                .prepared
                .has_spoiler_menu(self.way.send_images_as_photos())
            {
                ui.checkbox(&mut self.spoiler, "Hide with Spoiler");
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Self-Destruct:")
                            .size(12.0)
                            .color(colors.sub_text_fg),
                    );
                    for &preset in MediaTtlPreset::ALL {
                        if ui
                            .selectable_label(self.ttl_preset == preset, preset.label())
                            .clicked()
                        {
                            self.ttl_preset = preset;
                        }
                    }
                });
            }

            ui.add_space(6.0);
            if self
                .prepared
                .can_add_caption(self.way.send_images_as_photos())
            {
                ui.add(
                    TextEdit::singleline(&mut self.caption)
                        .desired_width(f32::INFINITY)
                        .hint_text("Add a caption..."),
                );
                let caption_len = self.caption.trim().chars().count();
                let limit = self.caption_limit();
                if caption_len > limit {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "Caption too long ({caption_len}/{limit})"
                            ))
                            .size(11.0)
                            .color(colors.attention_fg),
                        );
                        if ui.small_button("Increase limit").clicked() {
                            action = Some(AttachFilesAction::CaptionLimitExceeded {
                                length: caption_len,
                                limit,
                            });
                        }
                    });
                } else if caption_len + 128 >= limit {
                    ui.label(
                        egui::RichText::new(format!("{caption_len}/{limit} characters"))
                            .size(11.0)
                            .color(colors.sub_text_fg),
                    );
                }
            }

            // Grouping preview line
            let batches = self.grouped_batches();
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Dispatch plan: {} batch(es) · {}",
                    batches.len(),
                    batches
                        .iter()
                        .map(|g| format!("{:?}({})", g.album_type, g.files.len()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .size(11.0)
                .color(colors.sub_text_fg),
            );

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if !self.prepared.files.is_empty()
                    && paint_secondary_button(
                        ui,
                        "Stash files",
                        colors,
                        Some(&tg_lang::tr::lng_stash_hint("Ctrl+S")),
                    )
                {
                    action = Some(AttachFilesAction::StashFiles);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.prepared.files.is_empty()
                        && paint_primary_button(ui, &tg_lang::tr::lng_send_button(), colors)
                    {
                        action = Some(self.build_action_for_send());
                    }
                    ui.add_space(8.0);
                    if paint_secondary_button(ui, &tg_lang::tr::lng_cancel(), colors, None) {
                        action = Some(AttachFilesAction::Cancelled);
                    }
                });
            });
        });

        if !self.prepared.files.is_empty()
            && action.is_none()
            && ui.ctx().input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::COMMAND,
                    egui::Key::S,
                ))
            })
        {
            action = Some(AttachFilesAction::StashFiles);
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(AttachFilesAction::Cancelled);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 6. Delete Messages Box: moved to `history::delete_box` (TGB-3052)
// ---------------------------------------------------------------------------

pub use crate::history::delete_box::{DeleteConfirmAction, DeleteConfirmBoxState};

// ---------------------------------------------------------------------------
// 7. Report Message Box (`boxes/report_messages_box.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`ReportMessageBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportMessageAction {
    /// Report submitted.
    Submit {
        /// Reported message ids.
        msg_ids: Vec<MsgId>,
        /// Selected reason.
        reason: MessageReportReason,
        /// Optional comment.
        comment: String,
    },
    /// User cancelled.
    Cancelled,
}

/// Stateful Report Message modal dialog.
#[derive(Debug, Clone)]
pub struct ReportMessageBoxState {
    /// Target message ids.
    pub msg_ids: Vec<MsgId>,
    /// Selected abuse reason.
    pub reason: MessageReportReason,
    /// Optional details comment.
    pub comment: String,
    /// Optional validation error.
    pub error: Option<String>,
}

impl ReportMessageBoxState {
    /// Creates a new report dialog for `msg_ids`.
    #[must_use]
    pub fn new(msg_ids: Vec<MsgId>) -> Self {
        Self {
            msg_ids,
            reason: MessageReportReason::Spam,
            comment: String::new(),
            error: None,
        }
    }

    /// Renders the report modal.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<ReportMessageAction> {
        let mut action = None;
        show_modal_shell(ui, "tg_report_message_modal", 360.0, colors, |ui| {
            ui.label(
                egui::RichText::new("Report Message")
                    .strong()
                    .size(16.0)
                    .color(colors.bold_fg),
            );
            ui.add_space(6.0);
            for r in MessageReportReason::ALL {
                if ui.radio(self.reason == r, r.label()).clicked() {
                    self.reason = r;
                    self.error = None;
                }
            }
            ui.add_space(6.0);
            ui.add(
                TextEdit::multiline(&mut self.comment)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .hint_text("Additional details (required for Other)..."),
            );
            if let Some(ref err) = self.error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(colors.attention_fg),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_primary_button(ui, "Report", colors) {
                        if self.reason == MessageReportReason::Other
                            && self.comment.trim().is_empty()
                        {
                            self.error =
                                Some("Please provide details when reporting as Other.".to_owned());
                        } else {
                            action = Some(ReportMessageAction::Submit {
                                msg_ids: self.msg_ids.clone(),
                                reason: self.reason,
                                comment: self.comment.trim().to_owned(),
                            });
                        }
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        action = Some(ReportMessageAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(ReportMessageAction::Cancelled);
        }
        action
    }
}

/// Outcome of [`BlockFromRepliesBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockFromRepliesAction {
    /// Confirmed `contacts.blockFromReplies` submission.
    Submit(tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest),
    /// User cancelled the modal.
    Cancelled,
}

/// Confirmation dialog for blocking a sender from the official `@Replies` chat
/// (`window/window_peer_menu.cpp:2788-2855`).
#[derive(Debug, Clone)]
pub struct BlockFromRepliesBoxState {
    /// Reply message id inside the Replies chat.
    pub msg_id: MsgId,
    /// Display name of the offending sender.
    pub sender_name: String,
    /// Whether to report the sender for spam (`flags.2`, default `true`).
    pub report_spam: bool,
    /// Whether to delete this reply message (`flags.0`, default `true`).
    pub delete_message: bool,
    /// Whether to delete all replies from this sender (`flags.1`, default `true`).
    pub delete_history: bool,
}

impl BlockFromRepliesBoxState {
    /// Creates a new Replies-chat block confirmation modal for `(msg_id, sender_name)`.
    #[must_use]
    pub fn new(msg_id: MsgId, sender_name: impl Into<String>) -> Self {
        let raw = sender_name.into();
        let clean = if raw.trim().is_empty() {
            "User".to_owned()
        } else {
            raw
        };
        Self {
            msg_id,
            sender_name: clean,
            report_spam: true,
            delete_message: true,
            delete_history: true,
        }
    }

    /// Renders the confirmation modal (`contacts.blockFromReplies`).
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<BlockFromRepliesAction> {
        let mut action = None;
        show_modal_shell(ui, "tg_block_from_replies_modal", 380.0, colors, |ui| {
            ui.label(
                egui::RichText::new(tg_lang::tr::lng_blocked_list_confirm_title(
                    &self.sender_name,
                ))
                .strong()
                .size(16.0)
                .color(colors.bold_fg),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Block this user from sending you further replies in the Replies chat.",
                )
                .size(13.0)
                .color(colors.sub_text_fg),
            );
            ui.add_space(10.0);

            ui.checkbox(&mut self.report_spam, tg_lang::tr::lng_report_spam());
            ui.add_space(4.0);
            ui.checkbox(
                &mut self.delete_message,
                tg_lang::tr::lng_blocked_list_confirm_reply(),
            );
            ui.add_space(4.0);
            ui.checkbox(
                &mut self.delete_history,
                tg_lang::tr::lng_blocked_list_confirm_reply_all(&self.sender_name),
            );

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirm_btn = egui::Button::new(
                        egui::RichText::new(tg_lang::tr::lng_blocked_list_confirm_ok())
                            .strong()
                            .size(13.0)
                            .color(Color32::WHITE),
                    )
                    .fill(colors.attention_fg)
                    .corner_radius(CornerRadius::same(8));
                    if ui.add(confirm_btn).clicked() {
                        action = Some(BlockFromRepliesAction::Submit(
                            tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest {
                                msg_id: self.msg_id,
                                delete_message: self.delete_message,
                                delete_history: self.delete_history,
                                report_spam: self.report_spam,
                            },
                        ));
                    }
                    ui.add_space(8.0);
                    if ui.button(tg_lang::tr::lng_cancel()).clicked() {
                        action = Some(BlockFromRepliesAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(BlockFromRepliesAction::Cancelled);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// Shared modal shell helpers
// ---------------------------------------------------------------------------

fn show_modal_shell(
    ui: &mut Ui,
    id_salt: &str,
    width: f32,
    colors: &HistoryColors,
    add_contents: impl FnOnce(&mut Ui),
) {
    let screen = ui.ctx().viewport_rect();
    let inner_margin: i8 = if screen.width() < 460.0 { 14 } else { 18 };
    let max_inner_w = (screen.width() - 24.0 - f32::from(inner_margin) * 2.0).max(240.0);
    let clamped_w = width.min(max_inner_w);

    egui::Area::new(Id::unique((id_salt, "backdrop")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ui.ctx(), |ui| {
            let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(
                r,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(0, 0, 0, 110),
            );
        });

    egui::Area::new(Id::unique(id_salt))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            egui::Frame::NONE
                .fill(colors.bar_bg)
                .stroke(Stroke::new(1.0, colors.divider))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::same(inner_margin))
                .show(ui, |ui| {
                    apply_modal_visuals(ui, colors);
                    ui.set_width(clamped_w);
                    ui.set_max_width(clamped_w);
                    add_contents(ui);
                });
        });
}

fn apply_modal_visuals(ui: &mut Ui, colors: &HistoryColors) {
    let mut visuals = if colors.is_night {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.override_text_color = Some(colors.bold_fg);
    visuals.extreme_bg_color = colors.bar_bg_over;
    visuals.faint_bg_color = colors.bar_bg_over;
    visuals.widgets.noninteractive.bg_fill = colors.bar_bg;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors.bold_fg);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.divider);
    visuals.widgets.inactive.bg_fill = colors.bar_bg_over;
    visuals.widgets.inactive.weak_bg_fill = colors.bar_bg_over;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, colors.bold_fg);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors.divider);
    visuals.widgets.hovered.bg_fill = colors.bar_bg_over;
    visuals.widgets.hovered.weak_bg_fill = colors.bar_bg_over;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.2, colors.bold_fg);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.accent);
    visuals.widgets.active.bg_fill = colors.accent;
    visuals.widgets.active.fg_stroke = Stroke::new(1.2, Color32::WHITE);
    visuals.selection.bg_fill = colors.accent;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    ui.style_mut().visuals = visuals;
}

fn paint_primary_button(ui: &mut Ui, label: &str, colors: &HistoryColors) -> bool {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), Color32::WHITE);
    let w = (galley.size().x + 28.0).max(76.0);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.is_pointer_button_down_on() {
        colors.accent.gamma_multiply(0.88)
    } else if resp.hovered() {
        colors.accent.gamma_multiply(0.94)
    } else {
        colors.accent
    };
    ui.painter().rect_filled(r, CornerRadius::same(6), fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            r,
            CornerRadius::same(6),
            Stroke::new(2.0, colors.bold_fg),
            egui::StrokeKind::Outside,
        );
    }
    ui.painter().galley(
        Pos2::new(
            r.center().x - galley.size().x * 0.5,
            r.center().y - galley.size().y * 0.5,
        ),
        galley,
        Color32::WHITE,
    );
    resp.clicked()
}

fn paint_secondary_button(
    ui: &mut Ui,
    label: &str,
    colors: &HistoryColors,
    tooltip: Option<&str>,
) -> bool {
    let fg = colors.active_text_fg;
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), fg);
    let w = (galley.size().x + 22.0).max(68.0);
    let (r, mut resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if let Some(tip) = tooltip {
        resp = resp.on_hover_text(tip);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if resp.is_pointer_button_down_on() || resp.hovered() {
        colors.bar_bg_over
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(r, CornerRadius::same(6), bg);
    }
    ui.painter().rect_stroke(
        r,
        CornerRadius::same(6),
        Stroke::new(
            1.0,
            if resp.has_focus() {
                colors.accent
            } else {
                colors.divider
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(
        Pos2::new(
            r.center().x - galley.size().x * 0.5,
            r.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}
