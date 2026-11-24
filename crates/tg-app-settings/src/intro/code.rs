//! Verification code step, segmented digit input, delivery texts, and call countdown.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_code.cpp` and
//! `Telegram/SourceFiles/intro/intro_code_input.cpp`.

use std::time::{Duration, Instant};

use egui::{
    Align, Color32, CornerRadius, Event, FontId, Key, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use tg_api::login::{CallStatus, CodeInputState};
use tg_lang::tr;
use tg_tl::enums::auth::SentCodeType;

use crate::intro::style::{
    ACTIVE_LINE_FG, ACTIVE_LINE_FG_ERROR, CODE_DIGIT_BORDER_DAY, INTRO_CODE_DIGIT_HEIGHT,
    INTRO_CODE_DIGIT_SKIP, INTRO_CODE_DIGIT_WIDTH, INTRO_NEXT_BUTTON_WIDTH, ShakeAnimation,
    link_button, primary_intro_button, rich_markdown_label,
};
use tg_app_core::stand_in::theme::Palette;

/// Maximum digit slots supported (`kMaxDigitsCount` in `intro_code_input.cpp`).
const MAX_DIGITS_COUNT: usize = 32;

/// Visual classification of how the verification code was delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeDeliveryKind {
    /// Sent to another active Telegram app session (`SentCodeType::App`).
    App,
    /// Sent via SMS (`SentCodeType::Sms`).
    Sms,
    /// Delivered via automated phone call (`SentCodeType::Call`).
    Call,
    /// Flash call with phone pattern (`SentCodeType::FlashCall`).
    FlashCall {
        /// Phone pattern of incoming flash call.
        pattern: String,
    },
    /// Missed call with prefix (`SentCodeType::MissedCall`).
    MissedCall {
        /// Calling number prefix.
        prefix: String,
    },
    /// Anonymous number code retrieved via Fragment (`SentCodeType::FragmentSms`).
    Fragment {
        /// Fragment verification URL.
        url: String,
    },
    /// Login code sent to configured login email (`SentCodeType::EmailCode`).
    EmailLogin {
        /// Masked email pattern.
        email_pattern: String,
    },
    /// Email verification code during initial login email setup (`LoginStep::EmailVerify`).
    EmailSetup {
        /// Masked email pattern.
        email_pattern: String,
    },
}

impl CodeDeliveryKind {
    /// Derives [`CodeDeliveryKind`] from `tg-api`'s [`CodeInputState`].
    #[must_use]
    pub fn from_code_state(state: &CodeInputState) -> Self {
        if let Some(email) = &state.email_pattern_login {
            Self::EmailLogin {
                email_pattern: email.clone(),
            }
        } else if let Some(url) = &state.fragment_url {
            Self::Fragment { url: url.clone() }
        } else if state.code_by_telegram {
            Self::App
        } else if state.call_status == CallStatus::Called {
            Self::Call
        } else {
            Self::Sms
        }
    }

    /// Derives [`CodeDeliveryKind`] directly from a generated `tg_tl` [`SentCodeType`].
    #[must_use]
    pub fn from_sent_code_type(code_type: &SentCodeType) -> Self {
        match code_type {
            SentCodeType::App(_) => Self::App,
            SentCodeType::Sms(_)
            | SentCodeType::FirebaseSms(_)
            | SentCodeType::SmsWord(_)
            | SentCodeType::SmsPhrase(_)
            | SentCodeType::SetUpEmailRequired(_) => Self::Sms,
            SentCodeType::Call(_) => Self::Call,
            SentCodeType::FlashCall(t) => Self::FlashCall {
                pattern: t.pattern.clone(),
            },
            SentCodeType::MissedCall(t) => Self::MissedCall {
                prefix: t.prefix.clone(),
            },
            SentCodeType::FragmentSms(t) => Self::Fragment { url: t.url.clone() },
            SentCodeType::EmailCode(t) => Self::EmailLogin {
                email_pattern: t.email_pattern.clone(),
            },
        }
    }

    /// Returns the localized description text for this delivery channel (`CodeWidget::updateDescText`).
    #[must_use]
    pub fn description_text(&self, formatted_phone: &str) -> String {
        match self {
            Self::App => tr::lng_code_from_telegram(),
            Self::Sms => tr::lng_code_desc(),
            Self::Call => tr::lng_code_called(),
            Self::FlashCall { pattern } => {
                format!("{}\n({pattern})", tr::lng_code_called())
            }
            Self::MissedCall { prefix } => {
                format!("{}\n({prefix}…)", tr::lng_code_called())
            }
            Self::Fragment { .. } => tr::lng_intro_fragment_about(formatted_phone),
            Self::EmailLogin { email_pattern } | Self::EmailSetup { email_pattern } => {
                tr::lng_intro_email_confirm_subtitle(email_pattern)
            }
        }
    }
}

/// Formats the phone-call resend status label (`CodeWidget::updateCallText`).
#[must_use]
pub fn format_call_status_text(
    code_by_telegram: bool,
    call_status: CallStatus,
    call_timeout_secs: i32,
) -> Option<String> {
    if code_by_telegram {
        return None;
    }
    match call_status {
        CallStatus::Disabled => None,
        CallStatus::Waiting => {
            let timeout = call_timeout_secs.max(0);
            let seconds = format!("{:02}", timeout % 60);
            let minutes = if timeout >= 3600 {
                format!("{}:{:02}", timeout / 3600, (timeout / 60) % 60)
            } else {
                format!("{}", timeout / 60)
            };
            Some(tr::lng_code_call(&minutes, &seconds))
        }
        CallStatus::Calling => Some(tr::lng_code_calling()),
        CallStatus::Called => Some(tr::lng_code_called()),
    }
}

/// Segmented verification code input model (`Ui::CodeInput` in `intro_code_input.cpp`).
#[derive(Debug, Clone)]
pub struct SegmentedCodeInput {
    digits: Vec<Option<u8>>,
    /// Active cursor slot index (`0..digits.len()`).
    pub focused_index: usize,
    /// Whether error border color is currently active.
    pub error_border: bool,
    /// Shake animation triggered on invalid digit/code.
    pub shake: ShakeAnimation,
}

impl Default for SegmentedCodeInput {
    fn default() -> Self {
        Self::new(5)
    }
}

impl SegmentedCodeInput {
    /// Creates a segmented code input with `length` digit slots (`1..=32`).
    #[must_use]
    pub fn new(length: usize) -> Self {
        let len = length.clamp(1, MAX_DIGITS_COUNT);
        Self {
            digits: vec![None; len],
            focused_index: 0,
            error_border: false,
            shake: ShakeAnimation::default(),
        }
    }

    /// Resizes the input to `length` digit slots and clears existing digits.
    pub fn set_length(&mut self, length: usize) {
        let len = length.clamp(1, MAX_DIGITS_COUNT);
        if self.digits.len() != len {
            self.digits = vec![None; len];
            self.focused_index = 0;
            self.error_border = false;
        }
    }

    /// Number of expected code digits.
    #[must_use]
    pub fn len(&self) -> usize {
        self.digits.len()
    }

    /// Returns `true` if there are zero slots (never true after clamp).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.digits.is_empty()
    }

    /// Returns the digit slice for rendering/inspection.
    #[must_use]
    pub fn digits(&self) -> &[Option<u8>] {
        &self.digits
    }

    /// Clears all digits and resets focus to slot 0 (`CodeInput::clear`).
    pub fn clear(&mut self) {
        self.digits.fill(None);
        self.focused_index = 0;
        self.error_border = false;
    }

    /// Clears digits, turns borders red, and triggers shake (`CodeInput::showError`).
    pub fn show_error(&mut self, now: Instant) {
        self.clear();
        self.error_border = true;
        self.shake.trigger(now);
    }

    /// Collects all entered digits into a string (`CodeInput::collectDigits`).
    #[must_use]
    pub fn collect_code(&self) -> String {
        let mut out = String::with_capacity(self.digits.len());
        for d in self.digits.iter().flatten() {
            out.push(char::from(b'0' + *d));
        }
        out
    }

    /// Returns `true` if every digit slot is filled.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.digits.iter().all(Option::is_some)
    }

    /// Sets digits from a string (stripping non-digits) (`CodeInput::setCode`).
    ///
    /// Returns `Some(code)` if all slots are now filled and ready for auto-submit.
    pub fn set_code(&mut self, text: &str) -> Option<String> {
        self.error_border = false;
        let only_digits: Vec<u8> = text
            .bytes()
            .filter(u8::is_ascii_digit)
            .take(self.digits.len())
            .map(|b| b - b'0')
            .collect();
        if only_digits.is_empty() {
            return None;
        }
        for (i, d) in only_digits.into_iter().enumerate() {
            self.digits[i] = Some(d);
        }
        self.focused_index = self
            .digits
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| self.digits.len().saturating_sub(1));
        if self.is_complete() {
            Some(self.collect_code())
        } else {
            None
        }
    }

    /// Processes one typed digit `0..=9` (`CodeInput::processDigit`).
    ///
    /// Returns `Some(code)` when entering the final digit completes the code.
    pub fn input_digit(&mut self, digit: u8) -> Option<String> {
        if digit > 9 || self.digits.is_empty() {
            return None;
        }
        self.error_border = false;
        let len = self.digits.len();
        self.digits[self.focused_index] = Some(digit);
        self.focused_index = (self.focused_index + 1) % len;
        if self.focused_index == 0 {
            if self.is_complete() {
                self.focused_index = len - 1;
                return Some(self.collect_code());
            }
            if let Some(first_empty) = self.digits.iter().position(Option::is_none) {
                self.focused_index = first_empty;
            }
        }
        None
    }

    /// Processes Backspace matching `CodeInput::keyPressEvent`.
    pub fn backspace(&mut self) {
        if self.digits.is_empty() {
            return;
        }
        self.error_border = false;
        let was_empty = self.digits[self.focused_index].is_none();
        self.digits[self.focused_index] = None;
        self.focused_index = self.focused_index.saturating_sub(1);
        if was_empty {
            self.digits[self.focused_index] = None;
        }
    }

    /// Moves focus circularly left/right (`CodeInput::Circular`).
    pub fn move_focus(&mut self, delta: isize) {
        let len = self.digits.len() as isize;
        if len <= 0 {
            return;
        }
        let next = ((self.focused_index as isize + delta) % len + len) % len;
        self.focused_index = next as usize;
    }
}

/// Actions emitted by [`CodeStepState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeStepAction {
    /// Submit the completed verification code.
    SubmitCode(String),
    /// User clicked `"Send code via SMS"` (`CodeWidget::noTelegramCode`).
    ResendViaSms,
    /// 1-second timer tick for the phone-call fallback countdown (`CodeWidget::sendCall`).
    TickCallTimer,
    /// User clicked `"Open Fragment"` on a Fragment anonymous-number code step.
    OpenFragment(String),
}

/// UI state for the verification code step (`Intro::CodeWidget`).
#[derive(Debug)]
pub struct CodeStepState {
    /// Segmented digit input widget state.
    pub input: SegmentedCodeInput,
    /// Optional delivery override (used for FlashCall / MissedCall previews and tests).
    pub delivery_override: Option<CodeDeliveryKind>,
    /// Last time the 1-second call countdown ticked.
    last_call_tick: Instant,
    /// Last submitted code (prevents duplicate submission of identical code).
    pub last_submitted_code: String,
    /// Optional status note (e.g. after clicking Open Fragment).
    pub info_note: Option<String>,
}

impl Default for CodeStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeStepState {
    /// Creates a new `CodeStepState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            input: SegmentedCodeInput::new(5),
            delivery_override: None,
            last_call_tick: tg_app_core::clock::now(),
            last_submitted_code: String::new(),
            info_note: None,
        }
    }

    /// Resets the code input when entering a new code step of length `code_length`.
    pub fn prepare(&mut self, code_length: i32, now: Instant) {
        let len = if code_length > 0 {
            code_length as usize
        } else {
            5
        };
        self.input = SegmentedCodeInput::new(len);
        self.delivery_override = None;
        self.last_call_tick = now;
        self.last_submitted_code.clear();
        self.info_note = None;
    }

    /// Triggers error shake and clears the entered code.
    pub fn trigger_error(&mut self, now: Instant) {
        self.last_submitted_code.clear();
        self.input.show_error(now);
    }

    /// Renders the verification code step (`CodeWidget` + `CodeInput`).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        formatted_phone: &str,
        delivery: &CodeDeliveryKind,
        code_length: i32,
        code_by_telegram: bool,
        call_status: CallStatus,
        call_timeout_secs: i32,
        error_text: &str,
        now: Instant,
        palette: &Palette,
    ) -> Option<CodeStepAction> {
        let mut action = None;
        let expected_len = if code_length > 0 {
            code_length as usize
        } else {
            5
        };
        if self.input.len() != expected_len {
            self.input.set_length(expected_len);
        }

        // Advance 1-second call countdown when waiting (`CodeWidget::_callTimer`).
        if !code_by_telegram && call_status == CallStatus::Waiting {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
            if now.saturating_duration_since(self.last_call_tick) >= Duration::from_secs(1) {
                self.last_call_tick = now;
                action = Some(CodeStepAction::TickCallTimer);
            }
        }

        let effective_delivery = self.delivery_override.as_ref().unwrap_or(delivery);
        let is_fragment = matches!(effective_delivery, CodeDeliveryKind::Fragment { .. });
        let width = card_width.min(INTRO_NEXT_BUTTON_WIDTH);
        let shake_dx = self.input.shake.offset(now);
        if self.input.shake.is_animating(now) {
            ui.ctx().request_repaint();
        }

        // Handle keyboard input for segmented digit boxes (`CodeInput::keyPressEvent`)
        let events = ui.input(|i| i.events.clone());
        let mut auto_submit_code: Option<String> = None;
        for ev in &events {
            match ev {
                Event::Text(text) => {
                    for ch in text.chars() {
                        if let Some(d) = ch.to_digit(10) {
                            if let Some(done) = self.input.input_digit(d as u8) {
                                auto_submit_code = Some(done);
                            }
                        } else if ch.is_alphabetic() {
                            self.input.shake.trigger(now);
                        }
                    }
                }
                Event::Paste(text) => {
                    if let Some(done) = self.input.set_code(text) {
                        auto_submit_code = Some(done);
                    }
                }
                Event::Key {
                    key, pressed: true, ..
                } => match key {
                    Key::Backspace => self.input.backspace(),
                    Key::ArrowLeft | Key::ArrowUp => self.input.move_focus(-1),
                    Key::ArrowRight | Key::ArrowDown | Key::Space => self.input.move_focus(1),
                    Key::Home => self.input.focused_index = 0,
                    Key::End => {
                        self.input.focused_index = self.input.len().saturating_sub(1);
                    }
                    Key::Enter => {
                        if self.input.is_complete() {
                            auto_submit_code = Some(self.input.collect_code());
                        } else {
                            self.input.shake.trigger(now);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if let Some(code) = auto_submit_code
            && code != self.last_submitted_code
        {
            self.last_submitted_code = code.clone();
            action = Some(CodeStepAction::SubmitCode(code));
        }

        ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
            // Title: formatted phone number or `lng_intro_fragment_title`
            let title = if is_fragment {
                tr::lng_intro_fragment_title()
            } else {
                formatted_phone.to_owned()
            };
            ui.label(
                RichText::new(title)
                    .size(18.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);

            // Description text (supports `**via Telegram**` bold span)
            let desc = effective_delivery.description_text(formatted_phone);
            rich_markdown_label(
                ui,
                &desc,
                13.5,
                palette.window_sub_text_fg,
                palette.window_bold_fg,
                false,
            );
            ui.add_space(24.0);

            // Segmented digit boxes (`CodeDigit` in `intro_code_input.cpp`)
            let count = self.input.len();
            let skip = if count > 5 {
                6.0
            } else {
                INTRO_CODE_DIGIT_SKIP
            };
            let box_w = ((width - skip * (count.saturating_sub(1)) as f32) / count as f32)
                .clamp(28.0, INTRO_CODE_DIGIT_WIDTH);
            let box_h = INTRO_CODE_DIGIT_HEIGHT;
            let total_w = box_w * count as f32 + skip * (count.saturating_sub(1)) as f32;
            let row_origin = ui.cursor().min;
            let start_x = row_origin.x + ((width - total_w) * 0.5).max(0.0) + shake_dx;
            ui.allocate_space(Vec2::new(width, box_h));

            let neutral_border = if palette.window_bg.r() > 128 {
                CODE_DIGIT_BORDER_DAY
            } else {
                palette.divider
            };

            for i in 0..count {
                let bx = start_x + (box_w + skip) * i as f32;
                let rect =
                    Rect::from_min_size(Pos2::new(bx, row_origin.y), Vec2::new(box_w, box_h));
                let resp = ui.interact(
                    rect,
                    ui.scope_id().with(("intro_code_digit", i)),
                    Sense::click(),
                );
                if resp.clicked() {
                    self.input.focused_index = i;
                }
                let is_focused = i == self.input.focused_index;
                let border_color = if self.input.error_border {
                    ACTIVE_LINE_FG_ERROR
                } else if is_focused {
                    ACTIVE_LINE_FG
                } else {
                    neutral_border
                };
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(8), palette.window_bg_over);
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(8),
                    Stroke::new(2.0, border_color),
                    StrokeKind::Inside,
                );
                if let Some(d) = self.input.digits()[i] {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        d.to_string(),
                        FontId::proportional(21.0),
                        palette.window_fg,
                    );
                } else if is_focused {
                    // Subtle cursor bar in active empty box
                    let bar_top = Pos2::new(rect.center().x, rect.center().y - 10.0);
                    let bar_bot = Pos2::new(rect.center().x, rect.center().y + 10.0);
                    painter.line_segment(
                        [bar_top, bar_bot],
                        Stroke::new(
                            1.5,
                            Color32::from_rgba_unmultiplied(
                                ACTIVE_LINE_FG.r(),
                                ACTIVE_LINE_FG.g(),
                                ACTIVE_LINE_FG.b(),
                                180,
                            ),
                        ),
                    );
                }
            }

            ui.add_space(18.0);

            // "Send code via SMS" link OR call countdown status label
            if code_by_telegram {
                if link_button(ui, &tr::lng_code_no_telegram(), palette).clicked() {
                    action = Some(CodeStepAction::ResendViaSms);
                }
            } else if let Some(call_text) =
                format_call_status_text(code_by_telegram, call_status, call_timeout_secs)
            {
                ui.label(
                    RichText::new(call_text)
                        .size(13.0)
                        .color(palette.window_sub_text_fg),
                );
            } else {
                ui.add_space(18.0);
            }

            // Error slot (`errorTop() = introErrorBelowLinkTop`)
            ui.add_space(10.0);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 20.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    if !error_text.is_empty() {
                        ui.label(
                            RichText::new(error_text)
                                .size(12.5)
                                .color(palette.attention_fg),
                        );
                    } else if let Some(note) = &self.info_note {
                        ui.label(
                            RichText::new(note)
                                .size(12.5)
                                .color(palette.window_active_text_fg),
                        );
                    }
                },
            );
            ui.add_space(10.0);

            // Primary action button: "Open Fragment" for Fragment, otherwise "Next"
            let btn_label = if is_fragment {
                tr::lng_intro_fragment_button()
            } else {
                tr::lng_intro_next()
            };
            if primary_intro_button(ui, width, &btn_label, palette).clicked() {
                if let CodeDeliveryKind::Fragment { url } = effective_delivery {
                    action = Some(CodeStepAction::OpenFragment(url.clone()));
                } else if self.input.is_complete() {
                    let code = self.input.collect_code();
                    self.last_submitted_code = code.clone();
                    action = Some(CodeStepAction::SubmitCode(code));
                } else {
                    self.input.shake.trigger(now);
                }
            }
        });

        action
    }
}
