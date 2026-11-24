//! Report Peer / Messages modal box.
//!
//! Port origin: `Telegram/SourceFiles/boxes/report_messages_box.cpp`
//! and `Telegram/SourceFiles/ui/boxes/report_box_graphics.h`.

use egui::{Align2, CornerRadius, CursorIcon, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle, RadioStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::ColorsResolver;
use tg_ui::widgets::checkbox::paint_radio_view;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::FlatLabel;

use crate::boxes::strings;
use crate::info::backend::{InfoBackend, ReportReason};
use crate::info::values::MAX_REPORT_COMMENT_LENGTH;
use tg_app_core::stand_in::types::PeerId;

/// Active step in [`ReportBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStep {
    /// Step 1: Select abuse category.
    SelectReason,
    /// Step 2: Provide optional (or required for `Other`) details comment.
    Details,
}

/// Stateful Report modal box (`ReportReasonBox` + `ReportDetailsBox`).
#[derive(Debug, Clone)]
pub struct ReportBoxState {
    /// Target peer being reported.
    pub peer_id: PeerId,
    /// Display title of the peer being reported.
    pub peer_title: String,
    /// Active step.
    pub step: ReportStep,
    /// Chosen report reason.
    pub reason: ReportReason,
    /// Additional comment (max 512 chars).
    pub comment: String,
    /// Validation error message.
    pub error: Option<String>,
}

impl ReportBoxState {
    /// Creates a new report dialog for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        Self {
            peer_id,
            peer_title: peer_title.into(),
            step: ReportStep::SelectReason,
            reason: ReportReason::Spam,
            comment: String::new(),
            error: None,
        }
    }

    /// Advances from reason selection to the comment step.
    pub fn next_step(&mut self) {
        self.step = ReportStep::Details;
        self.error = None;
    }

    /// Returns to reason selection.
    pub fn prev_step(&mut self) {
        self.step = ReportStep::SelectReason;
        self.error = None;
    }

    /// Submits the report to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        if self.reason == ReportReason::Other && self.comment.trim().is_empty() {
            let msg = strings::REPORT_DESCRIBE_REQUIRED.to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        if self.comment.chars().count() > MAX_REPORT_COMMENT_LENGTH {
            let msg =
                format!("Report details must be at most {MAX_REPORT_COMMENT_LENGTH} characters.");
            self.error = Some(msg.clone());
            return Err(msg);
        }
        backend.submit_report(self.peer_id, self.reason, &self.comment)?;
        self.error = None;
        Ok(())
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match self.step {
            ReportStep::SelectReason => BoxModal::new(id, tr::lng_report_button())
                .additional_title(&self.peer_title)
                .width(st::box_wide_width.0 as f32)
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                .add_button(BoxButton::default_btn("next", tr::lng_create_group_next())),
            ReportStep::Details => BoxModal::new(id, self.reason.localized_label())
                .additional_title(&self.peer_title)
                .width(st::box_wide_width.0 as f32)
                .with_left_button(BoxButton::default_btn("back", tr::lng_create_group_back()))
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                .add_button(BoxButton::attention("report", tr::lng_report_button())),
        }
    }

    /// Renders the report modal interior with real `tg_style` report category icons.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        match self.step {
            ReportStep::SelectReason => {
                FlatLabel::plain(strings::REPORT_INTRO, FlatLabelStyle::sub_text(colors))
                    .id_salt("report_intro")
                    .show(ui);
                ui.add_space(6.0);
                let rad_st = RadioStyle::default_radio(colors);
                let resolver = ColorsResolver(colors);
                for reason in ReportReason::ALL {
                    let selected = self.reason == reason;
                    let width = ui.available_width().max(180.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            colors.color("windowBgOver"),
                        );
                    }
                    let icon_rect = Rect::from_center_size(
                        Pos2::new(rect.min.x + 16.0, rect.center().y),
                        Vec2::splat(20.0),
                    );
                    let icon_fg = if selected {
                        colors.color("windowActiveTextFg")
                    } else {
                        colors.color("menuIconFg")
                    };
                    paint_style_icon(
                        ui.painter(),
                        icon_rect,
                        report_reason_icon(reason),
                        &resolver,
                        Some(icon_fg),
                    );
                    ui.painter().text(
                        Pos2::new(rect.min.x + 38.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        reason.localized_label(),
                        font_id_from_style(st::normal_font),
                        colors.color("windowFg"),
                    );
                    let radio_rect = Rect::from_center_size(
                        Pos2::new(rect.max.x - 16.0, rect.center().y),
                        Vec2::splat(rad_st.diameter),
                    );
                    paint_radio_view(
                        ui.painter(),
                        radio_rect,
                        &rad_st,
                        if selected { 1.0 } else { 0.0 },
                        1.0,
                    );
                    if resp.clicked() {
                        self.reason = reason;
                    }
                }
            }
            ReportStep::Details => {
                let placeholder = if self.reason == ReportReason::Other {
                    tr::lng_report_details_non_optional()
                } else {
                    tr::lng_report_details_optional()
                };
                InputField::multi_line(
                    "report_details_comment",
                    &mut self.comment,
                    &placeholder,
                    InputFieldStyle::default_input(colors),
                )
                .max_length(MAX_REPORT_COMMENT_LENGTH)
                .error(self.error.is_some())
                .show(ui);

                ui.add_space(8.0);
                FlatLabel::plain(
                    strings::REPORT_MODERATORS_HINT,
                    FlatLabelStyle::sub_text(colors),
                )
                .id_salt("report_hint")
                .show(ui);

                if let Some(ref err) = self.error {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(err)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("boxTextFgError")),
                    );
                }
            }
        }
    }
}

fn report_reason_icon(reason: ReportReason) -> Icon {
    match reason {
        ReportReason::Spam => st::menu_icon_block,
        ReportReason::Violence => st::menu_icon_violence,
        ReportReason::Pornography => st::menu_icon_porn,
        ReportReason::ChildAbuse => st::menu_icon_report_attention,
        ReportReason::Copyright => st::menu_icon_copyright,
        ReportReason::IllegalDrugs => st::menu_icon_drugs,
        ReportReason::PersonalDetails => st::menu_icon_personal,
        ReportReason::Fake => st::menu_icon_fake,
        ReportReason::Other => st::menu_icon_report,
    }
}
