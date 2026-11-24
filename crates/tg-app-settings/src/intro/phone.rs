//! Phone number entry step, country selector box, and phone formatting.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_phone.cpp`,
//! `Telegram/SourceFiles/ui/countryinput.cpp`,
//! `Telegram/SourceFiles/ui/boxes/country_select_box.cpp`, and
//! `Telegram/SourceFiles/ui/widgets/fields/special_fields.cpp`.

use std::time::Instant;

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Order, Pos2, Rect,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};
use tg_lang::{CountriesInstance, tr};

use crate::intro::style::{
    ACTIVE_LINE_FG, ACTIVE_LINE_FG_ERROR, INTRO_COUNTRY_CODE_WIDTH, INTRO_COUNTRY_WIDTH,
    INTRO_NEXT_BUTTON_WIDTH, INTRO_PHONE_WIDTH, ShakeAnimation, link_button, primary_intro_button,
};
use tg_app_core::stand_in::theme::Palette;

/// Maximum local phone tail digits (`kMaxPhoneTailLength` in `special_fields.cpp`).
const MAX_PHONE_TAIL_LENGTH: usize = 32;
/// Maximum calling code digits (`kMaxPhoneCodeLength` in `special_fields.cpp`).
const MAX_PHONE_CODE_LENGTH: usize = 4;

/// Flattened row entry in the country picker list (`CountrySelectBox::Entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryPickerEntry {
    /// Display country name.
    pub country: String,
    /// Two-letter ISO 3166-1 alpha-2 code.
    pub iso2: String,
    /// Calling code digits without leading `+`.
    pub code: String,
    /// Optional alternative searchable name (e.g. `"United States of America"`).
    pub alternative_name: String,
}

/// Actions emitted by [`PhoneStepState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneStepAction {
    /// User submitted the phone number (`full_number`).
    Submit(String),
    /// User clicked `"Quick log in using QR code"`.
    SwitchToQr,
}

/// View-model and UI state for the Phone step (`Intro::PhoneWidget`).
#[derive(Debug)]
pub struct PhoneStepState {
    countries: CountriesInstance,
    /// Currently matched country ISO2 code (empty if invalid/unknown).
    pub selected_iso2: String,
    /// Text displayed inside the country dropdown button.
    pub country_display: String,
    /// Calling code input field text (always starts with `'+'`).
    pub code_input: String,
    /// Local phone part formatted with spaces according to `tg-lang` patterns.
    pub phone_input: String,
    /// Remaining `−` (`\u{2212}`) placeholder string for the current pattern.
    pub additional_placeholder: String,
    /// Active digit groups for the local phone part (excluding country code).
    pattern: Vec<usize>,
    /// Whether the user has manually edited country or phone (suppresses auto-override from NearestDc).
    pub user_edited: bool,
    /// Whether the modal country picker box is open.
    pub picker_open: bool,
    /// Search query inside the country picker box.
    pub picker_query: String,
    /// Keyboard-highlighted row in the country picker box.
    pub picker_selected: usize,
    /// Flattened list of `(country, iso2, code)` entries.
    picker_entries: Vec<CountryPickerEntry>,
    /// Phone field shake animation on validation/RPC error.
    pub shake: ShakeAnimation,
    /// Focus request for the phone part field.
    pub focus_phone: bool,
    /// Focus request for the picker search box.
    focus_picker_search: bool,
    /// Optional banned-phone modal text (`PHONE_NUMBER_BANNED`).
    pub banned_modal: Option<String>,
    /// Interactive `PhoneBannedBox` state (`boxes/phone_banned_box.cpp`).
    pub banned_box: Option<crate::intro::auth_boxes::PhoneBannedBoxState>,
    /// Optional flood inform modal text (`PHONE_NUMBER_FLOOD`).
    pub phone_flood_modal: Option<String>,
}

impl Default for PhoneStepState {
    fn default() -> Self {
        Self::new("US")
    }
}

impl PhoneStepState {
    /// Creates a new `PhoneStepState` initialized to `initial_iso2` (falling back to `"US"`).
    #[must_use]
    pub fn new(initial_iso2: &str) -> Self {
        let countries = CountriesInstance::new();
        let picker_entries = build_picker_entries(&countries);
        let mut state = Self {
            countries,
            selected_iso2: String::new(),
            country_display: tr::lng_country_code(),
            code_input: "+".to_owned(),
            phone_input: String::new(),
            additional_placeholder: String::new(),
            pattern: Vec::new(),
            user_edited: false,
            picker_open: false,
            picker_query: String::new(),
            picker_selected: 0,
            picker_entries,
            shake: ShakeAnimation::default(),
            focus_phone: true,
            focus_picker_search: false,
            banned_modal: None,
            banned_box: None,
            phone_flood_modal: None,
        };
        if !state.select_country_iso2(initial_iso2) {
            state.select_country_iso2("US");
        }
        state.user_edited = false;
        state
    }

    /// Returns a reference to the underlying `tg-lang` [`CountriesInstance`].
    #[must_use]
    pub const fn countries(&self) -> &CountriesInstance {
        &self.countries
    }

    /// Applies a suggested ISO2 country from `help.getNearestDc` if the user hasn't edited yet.
    pub fn apply_suggested_country(&mut self, iso2: &str) {
        if !self.user_edited && !iso2.is_empty() {
            self.select_country_iso2(iso2);
            self.user_edited = false;
        }
    }

    /// Selects a country by ISO2 code (`CountryInput::chooseCountry`).
    pub fn select_country_iso2(&mut self, iso2: &str) -> bool {
        let upper = iso2.to_ascii_uppercase();
        let Some(info) = self.countries.by_iso2(&upper).cloned() else {
            return false;
        };
        let Some(first_code) = info.codes.first() else {
            return false;
        };
        self.select_country_entry(&info.name, &info.iso2, &first_code.calling_code);
        true
    }

    /// Selects an explicit `(name, iso2, calling_code)` entry from the picker.
    pub fn select_country_entry(&mut self, name: &str, iso2: &str, calling_code: &str) {
        self.selected_iso2 = iso2.to_owned();
        self.country_display = name.to_owned();
        self.code_input = format!("+{calling_code}");
        self.refresh_phone_pattern();
        self.reformat_phone_input();
        self.focus_phone = true;
    }

    /// Processes an edit to the country code field (`CountryCodeInput::correctValue`).
    ///
    /// Keeps a leading `'+'` and up to 4 calling-code digits; extra digits spill into the
    /// phone number field using `CountriesInstance::valid_phone_code`, and auto-detects the
    /// country name via `CountriesInstance::by_code`.
    pub fn on_code_edited(&mut self, raw: &str) {
        self.user_edited = true;
        let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
        let mut code_digits = String::new();
        let mut spilled = String::new();

        if digits.len() > MAX_PHONE_CODE_LENGTH {
            let prefix = &digits[..MAX_PHONE_CODE_LENGTH];
            let valid = self.countries.valid_phone_code(prefix);
            if valid.is_empty() {
                code_digits.push_str(prefix);
                spilled.push_str(&digits[MAX_PHONE_CODE_LENGTH..]);
            } else {
                code_digits.push_str(&valid);
                spilled.push_str(&digits[valid.len()..]);
            }
        } else {
            code_digits.push_str(&digits);
        }

        self.code_input = format!("+{code_digits}");
        self.update_country_from_code(&code_digits);

        if !spilled.is_empty() {
            let existing_digits: String = self
                .phone_input
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            let combined = format!("{spilled}{existing_digits}");
            self.phone_input = combined;
            self.focus_phone = true;
        }
        self.refresh_phone_pattern();
        self.reformat_phone_input();
    }

    /// Processes an edit to the phone number field (`PhonePartInput::correctValue`).
    pub fn on_phone_edited(&mut self, raw: &str) {
        self.user_edited = true;
        self.phone_input = raw.to_owned();
        self.refresh_phone_pattern();
        self.reformat_phone_input();
    }

    /// Returns the combined `+<code><phone>` string (`PhoneWidget::fullNumber`).
    #[must_use]
    pub fn full_number(&self) -> String {
        if self.phone_input.is_empty() {
            self.code_input.clone()
        } else {
            format!("{} {}", self.code_input, self.phone_input)
        }
    }

    /// Returns the calling code digits without leading `+`.
    #[must_use]
    pub fn calling_code(&self) -> &str {
        self.code_input
            .strip_prefix('+')
            .unwrap_or(&self.code_input)
    }

    /// Formats a raw phone number using `tg-lang`'s `CountriesInstance::format_phone`.
    #[must_use]
    pub fn format_phone_display(&self, phone: &str) -> String {
        let formatted = self.countries.format_phone(phone);
        if !formatted.is_empty() && formatted.starts_with('+') {
            return formatted;
        }
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if digits.len() == 10 && digits.starts_with("999") {
            return format!("+{} {} {}", &digits[..3], &digits[3..6], &digits[6..]);
        }
        if formatted.is_empty() {
            phone.to_owned()
        } else if digits.is_empty() || phone.starts_with('+') {
            formatted
        } else {
            format!("+{formatted}")
        }
    }

    /// Filters countries for the Country Picker Box (`CountrySelectBox::Inner::updateFilter`).
    ///
    /// Matches word prefixes on country name, alternative name, ISO2 code, and calling code.
    #[must_use]
    pub fn filtered_countries(&self, query: &str) -> Vec<&CountryPickerEntry> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.picker_entries.iter().collect();
        }
        let lower = trimmed.to_lowercase();
        let query_code = lower.strip_prefix('+').unwrap_or(&lower);
        let words: Vec<&str> = lower
            .split(|c: char| c.is_whitespace() || c == '-')
            .filter(|w| !w.is_empty())
            .collect();

        self.picker_entries
            .iter()
            .filter(|entry| {
                if !query_code.is_empty()
                    && query_code.chars().all(|c| c.is_ascii_digit())
                    && entry.code.starts_with(query_code)
                {
                    return true;
                }
                if entry.iso2.eq_ignore_ascii_case(trimmed) {
                    return true;
                }
                let haystack =
                    format!("{} {}", entry.country, entry.alternative_name).to_lowercase();
                let name_words: Vec<&str> = haystack
                    .split(|c: char| c.is_whitespace() || c == '-')
                    .filter(|w| !w.is_empty())
                    .collect();
                words
                    .iter()
                    .all(|w| name_words.iter().any(|nw| nw.starts_with(w)))
            })
            .collect()
    }

    /// Triggers the phone field shake animation.
    pub fn trigger_error(&mut self, now: Instant) {
        self.shake.trigger(now);
        self.focus_phone = true;
    }

    fn update_country_from_code(&mut self, code: &str) {
        if code.is_empty() {
            self.selected_iso2.clear();
            self.country_display = tr::lng_country_code();
        } else if let Some(info) = self.countries.by_code(code) {
            self.selected_iso2 = info.iso2.clone();
            self.country_display = info.name.clone();
        } else {
            self.selected_iso2.clear();
            self.country_display = tr::lng_bad_country_code();
        }
    }

    fn refresh_phone_pattern(&mut self) {
        let code = self.calling_code();
        let digits: String = self
            .phone_input
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let mut groups = self.countries.groups(&format!("{code}{digits}"));
        if !groups.is_empty() && groups[0] == code.len() {
            groups.remove(0);
        } else {
            groups.clear();
        }
        self.pattern = groups;
        let mut ph = String::new();
        for (idx, &part) in self.pattern.iter().enumerate() {
            if idx > 0 {
                ph.push(' ');
            }
            for _ in 0..part {
                ph.push('\u{2212}');
            }
        }
        self.additional_placeholder = ph;
    }

    fn reformat_phone_input(&mut self) {
        let digits: Vec<char> = self
            .phone_input
            .chars()
            .filter(char::is_ascii_digit)
            .take(MAX_PHONE_TAIL_LENGTH)
            .collect();
        if digits.is_empty() {
            self.phone_input.clear();
            return;
        }
        if self.pattern.is_empty() {
            self.phone_input = digits.into_iter().collect();
            return;
        }

        let mut out = String::with_capacity(digits.len() + self.pattern.len());
        let mut group_idx = 0usize;
        let mut in_group = 0usize;
        for ch in digits {
            if group_idx < self.pattern.len() && in_group >= self.pattern[group_idx] {
                group_idx += 1;
                in_group = 0;
                if group_idx < self.pattern.len() {
                    out.push(' ');
                }
            }
            out.push(ch);
            if group_idx < self.pattern.len() {
                in_group += 1;
            }
        }
        self.phone_input = out;
    }

    /// Renders the Phone step UI (`PhoneWidget`) and optional Country Picker / Error modals.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        error_text: &str,
        now: Instant,
        palette: &Palette,
    ) -> Option<PhoneStepAction> {
        let mut action = None;
        let width = card_width.min(INTRO_COUNTRY_WIDTH);
        let shake_dx = self.shake.offset(now);
        if self.shake.is_animating(now) {
            ui.ctx().request_repaint();
        }

        ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
            ui.label(
                RichText::new(tr::lng_phone_title())
                    .size(18.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(tr::lng_phone_desc())
                    .size(13.5)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(22.0);

            // Country selector dropdown button (`CountryInput` in `countryinput.cpp`)
            let (country_rect, country_resp) =
                ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
            if country_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let painter = ui.painter();
            let flag = CountriesInstance::flag_emoji_by_iso2(&self.selected_iso2);
            let display = if flag.is_empty() {
                self.country_display.clone()
            } else {
                format!("{flag}  {}", self.country_display)
            };
            painter.text(
                Pos2::new(country_rect.left() + 3.0, country_rect.center().y),
                egui::Align2::LEFT_CENTER,
                display,
                FontId::proportional(15.5),
                palette.window_fg,
            );
            // Dropdown chevron on the right
            let chev_center = Pos2::new(country_rect.right() - 12.0, country_rect.center().y);
            painter.line_segment(
                [
                    Pos2::new(chev_center.x - 4.5, chev_center.y - 2.0),
                    Pos2::new(chev_center.x, chev_center.y + 2.5),
                ],
                Stroke::new(1.6, palette.window_sub_text_fg),
            );
            painter.line_segment(
                [
                    Pos2::new(chev_center.x, chev_center.y + 2.5),
                    Pos2::new(chev_center.x + 4.5, chev_center.y - 2.0),
                ],
                Stroke::new(1.6, palette.window_sub_text_fg),
            );
            painter.line_segment(
                [
                    Pos2::new(country_rect.left(), country_rect.bottom() - 1.0),
                    Pos2::new(country_rect.right(), country_rect.bottom() - 1.0),
                ],
                Stroke::new(1.0, palette.divider),
            );
            if country_resp.clicked() {
                self.picker_open = true;
                self.picker_query.clear();
                self.picker_selected = 0;
                self.focus_picker_search = true;
            }

            ui.add_space(10.0);

            // Row: Country Code (`64px`) + gap (`11px`) + Formatted Phone (`225px`)
            let code_w = INTRO_COUNTRY_CODE_WIDTH.min(width * 0.24);
            let gap = (width - code_w - INTRO_PHONE_WIDTH).clamp(8.0, 14.0);
            let phone_w = (width - code_w - gap).max(120.0);
            let row_top = ui.cursor().min;
            ui.allocate_space(Vec2::new(width, 40.0));

            let code_rect = Rect::from_min_size(
                Pos2::new(row_top.x + shake_dx, row_top.y),
                Vec2::new(code_w, 38.0),
            );
            let phone_rect = Rect::from_min_size(
                Pos2::new(row_top.x + code_w + gap + shake_dx, row_top.y),
                Vec2::new(phone_w, 38.0),
            );

            // Code input
            let mut code_draft = self.code_input.clone();
            let mut code_focused = false;
            let mut code_enter = false;
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(code_rect.shrink2(Vec2::new(2.0, 6.0))),
                |ui| {
                    let resp = ui.add_sized(
                        Vec2::new(code_w - 4.0, 26.0),
                        TextEdit::singleline(&mut code_draft)
                            .id_salt("intro_phone_code_field")
                            .frame(Frame::NONE)
                            .font(FontId::proportional(15.5))
                            .text_color(palette.window_fg),
                    );
                    code_focused = resp.has_focus();
                    code_enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                },
            );
            if code_draft != self.code_input {
                self.on_code_edited(&code_draft);
            }

            // Phone part input + additional `−` placeholder overlay
            let mut phone_draft = self.phone_input.clone();
            let mut phone_focused = false;
            let mut phone_enter = false;
            let phone_inner = phone_rect.shrink2(Vec2::new(6.0, 6.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(phone_inner), |ui| {
                let placeholder_str = if self.additional_placeholder.is_empty() {
                    tr::lng_phone_number()
                } else {
                    self.additional_placeholder.clone()
                };
                let resp = ui.add_sized(
                    phone_inner.size(),
                    TextEdit::singleline(&mut phone_draft)
                        .id_salt("intro_phone_part_field")
                        .frame(Frame::NONE)
                        .font(FontId::proportional(15.5))
                        .text_color(palette.window_fg)
                        .hint_text(
                            RichText::new(placeholder_str)
                                .size(15.5)
                                .color(palette.window_sub_text_fg),
                        ),
                );
                if self.focus_phone && !self.picker_open {
                    resp.request_focus();
                    self.focus_phone = false;
                }
                phone_focused = resp.has_focus();
                phone_enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            });
            if phone_draft != self.phone_input {
                self.on_phone_edited(&phone_draft);
            }

            // Paint underlines for code & phone fields
            let has_err = !error_text.is_empty();
            let (code_col, code_w_stroke) = if code_focused {
                (ACTIVE_LINE_FG, 2.0)
            } else {
                (palette.divider, 1.0)
            };
            let (phone_col, phone_w_stroke) = if has_err {
                (ACTIVE_LINE_FG_ERROR, 2.0)
            } else if phone_focused {
                (ACTIVE_LINE_FG, 2.0)
            } else {
                (palette.divider, 1.0)
            };
            ui.painter().line_segment(
                [
                    Pos2::new(code_rect.left(), code_rect.bottom() - 2.0),
                    Pos2::new(code_rect.right(), code_rect.bottom() - 2.0),
                ],
                Stroke::new(code_w_stroke, code_col),
            );
            ui.painter().line_segment(
                [
                    Pos2::new(phone_rect.left(), phone_rect.bottom() - 2.0),
                    Pos2::new(phone_rect.right(), phone_rect.bottom() - 2.0),
                ],
                Stroke::new(phone_w_stroke, phone_col),
            );

            // Centered error slot (`setErrorCentered(true)` in `intro_phone.cpp:98`)
            ui.add_space(14.0);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 22.0),
                Layout::top_down(Align::Center),
                |ui| {
                    if !error_text.is_empty() {
                        ui.label(
                            RichText::new(error_text)
                                .size(12.5)
                                .color(palette.attention_fg),
                        );
                    }
                },
            );
            ui.add_space(10.0);

            // Next button (`tr::lng_intro_next()`)
            let next_btn = primary_intro_button(
                ui,
                width.min(INTRO_NEXT_BUTTON_WIDTH),
                &tr::lng_intro_next(),
                palette,
            );
            if next_btn.clicked() || phone_enter {
                action = Some(PhoneStepAction::Submit(self.full_number()));
            } else if code_enter {
                if self.calling_code().len() > 1 && self.phone_input.is_empty() {
                    self.focus_phone = true;
                } else {
                    action = Some(PhoneStepAction::Submit(self.full_number()));
                }
            }

            // "Quick log in using QR code" link (`PhoneWidget::setupQrLogin`)
            ui.add_space(22.0);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 24.0),
                Layout::top_down(Align::Center),
                |ui| {
                    if link_button(ui, &tr::lng_phone_to_qr(), palette).clicked() {
                        action = Some(PhoneStepAction::SwitchToQr);
                    }
                },
            );
        });

        // Country Picker Box modal overlay (`CountrySelectBox`)
        if self.picker_open {
            self.show_country_picker_modal(ui.ctx(), palette);
        }

        // Banned phone (`PhoneBannedBox`) or Phone flood inform modal
        if let Some(ref mut banned) = self.banned_box {
            if let Some(ev) = banned.show_modal(ui.ctx(), palette) {
                match ev {
                    crate::intro::auth_boxes::PhoneBannedAction::Dismiss => {
                        self.banned_box = None;
                        self.banned_modal = None;
                    }
                    crate::intro::auth_boxes::PhoneBannedAction::OpenHelp(_url) => {
                        self.banned_box = None;
                        self.banned_modal = None;
                    }
                }
            }
        } else if let Some(msg) = self.banned_modal.clone() {
            let mut box_state =
                crate::intro::auth_boxes::PhoneBannedBoxState::new(self.full_number());
            box_state.body_text = msg;
            if box_state.show_modal(ui.ctx(), palette).is_some() {
                self.banned_modal = None;
            }
        } else if let Some(msg) = self.phone_flood_modal.clone()
            && show_simple_inform_modal(ui.ctx(), "intro_phone_flood_modal", &msg, palette)
        {
            self.phone_flood_modal = None;
        }

        action
    }

    fn show_country_picker_modal(&mut self, ctx: &egui::Context, palette: &Palette) {
        let screen = ctx.content_rect();
        egui::Area::new(Id::unique("intro_country_picker_backdrop"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (_, bg_resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, palette.layer_backdrop);
                if bg_resp.clicked() {
                    self.picker_open = false;
                }
            });

        let box_w = 340.0_f32.min(screen.width() - 32.0);
        let box_h = 440.0_f32.min(screen.height() - 48.0);
        let box_pos = Pos2::new(
            screen.center().x - box_w * 0.5,
            screen.center().y - box_h * 0.5,
        );

        let mut chosen: Option<(String, String, String)> = None;
        let mut close_requested = ctx.input(|i| i.key_pressed(Key::Escape));

        egui::Area::new(Id::unique("intro_country_picker_box"))
            .order(Order::Tooltip)
            .fixed_pos(box_pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0, palette.divider))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(box_w - 32.0, box_h - 32.0));
                        ui.set_max_size(Vec2::new(box_w - 32.0, box_h - 32.0));

                        ui.label(
                            RichText::new(tr::lng_country_select())
                                .size(17.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(10.0);

                        let search_resp = ui.add_sized(
                            Vec2::new(box_w - 32.0, 32.0),
                            TextEdit::singleline(&mut self.picker_query)
                                .id_salt("intro_country_search_field")
                                .hint_text(tr::lng_country_ph()),
                        );
                        if self.focus_picker_search {
                            search_resp.request_focus();
                            self.focus_picker_search = false;
                        }
                        if search_resp.changed() {
                            self.picker_selected = 0;
                        }

                        ui.add_space(8.0);
                        let filtered: Vec<CountryPickerEntry> = self
                            .filtered_countries(&self.picker_query)
                            .into_iter()
                            .cloned()
                            .collect();

                        if !filtered.is_empty() {
                            if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
                                self.picker_selected =
                                    (self.picker_selected + 1).min(filtered.len() - 1);
                            }
                            if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
                                self.picker_selected = self.picker_selected.saturating_sub(1);
                            }
                            if ui.input(|i| i.key_pressed(Key::Enter)) {
                                let entry = &filtered[self.picker_selected.min(filtered.len() - 1)];
                                chosen = Some((
                                    entry.country.clone(),
                                    entry.iso2.clone(),
                                    entry.code.clone(),
                                ));
                            }
                        }

                        let list_h = (box_h - 156.0).max(120.0);
                        ScrollArea::vertical()
                            .max_height(list_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if filtered.is_empty() {
                                    ui.add_space(32.0);
                                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                                        ui.label(
                                            RichText::new(tr::lng_country_none())
                                                .size(14.0)
                                                .color(palette.window_sub_text_fg),
                                        );
                                    });
                                } else {
                                    for (idx, entry) in filtered.iter().enumerate() {
                                        let is_sel = idx == self.picker_selected;
                                        let (row_rect, row_resp) = ui.allocate_exact_size(
                                            Vec2::new(ui.available_width(), 38.0),
                                            Sense::click(),
                                        );
                                        if row_resp.hovered() || is_sel {
                                            ui.painter().rect_filled(
                                                row_rect,
                                                CornerRadius::same(4),
                                                palette.window_bg_over,
                                            );
                                        }
                                        if row_resp.hovered() {
                                            ui.ctx()
                                                .set_cursor_icon(egui::CursorIcon::PointingHand);
                                        }
                                        let flag =
                                            CountriesInstance::flag_emoji_by_iso2(&entry.iso2);
                                        let left_text = if flag.is_empty() {
                                            entry.country.clone()
                                        } else {
                                            format!("{flag}  {}", entry.country)
                                        };
                                        ui.painter().text(
                                            Pos2::new(row_rect.left() + 10.0, row_rect.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            left_text,
                                            FontId::proportional(14.0),
                                            palette.window_fg,
                                        );
                                        ui.painter().text(
                                            Pos2::new(row_rect.right() - 10.0, row_rect.center().y),
                                            egui::Align2::RIGHT_CENTER,
                                            format!("+{}", entry.code),
                                            FontId::proportional(13.5),
                                            if row_resp.hovered() || is_sel {
                                                palette.window_active_text_fg
                                            } else {
                                                palette.window_sub_text_fg
                                            },
                                        );
                                        if row_resp.clicked() {
                                            chosen = Some((
                                                entry.country.clone(),
                                                entry.iso2.clone(),
                                                entry.code.clone(),
                                            ));
                                        }
                                    }
                                }
                            });

                        ui.add_space(10.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if link_button(ui, &tr::lng_close(), palette).clicked() {
                                close_requested = true;
                            }
                        });
                    });
            });

        if let Some((name, iso2, code)) = chosen {
            self.select_country_entry(&name, &iso2, &code);
            self.picker_open = false;
        } else if close_requested {
            self.picker_open = false;
        }
    }
}

fn build_picker_entries(countries: &CountriesInstance) -> Vec<CountryPickerEntry> {
    let mut entries = Vec::with_capacity(countries.list().len());
    for info in countries.list() {
        if info.is_hidden {
            continue;
        }
        for code in &info.codes {
            entries.push(CountryPickerEntry {
                country: info.name.clone(),
                iso2: info.iso2.clone(),
                code: code.calling_code.clone(),
                alternative_name: info.alternative_name.clone(),
            });
        }
    }
    entries
}

/// Renders a simple one-button inform modal (`Ui::MakeInformBox`) and returns `true` when closed.
pub(crate) fn show_simple_inform_modal(
    ctx: &egui::Context,
    id_salt: &str,
    message: &str,
    palette: &Palette,
) -> bool {
    let screen = ctx.content_rect();
    let mut dismissed = ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter));

    egui::Area::new(Id::unique(format!("{id_salt}_backdrop")))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, palette.layer_backdrop);
            if resp.clicked() {
                dismissed = true;
            }
        });

    let box_w = 340.0_f32.min(screen.width() - 32.0);
    let box_pos = Pos2::new(screen.center().x - box_w * 0.5, screen.center().y - 90.0);

    egui::Area::new(Id::unique(id_salt))
        .order(Order::Tooltip)
        .fixed_pos(box_pos)
        .show(ctx, |ui| {
            Frame::NONE
                .fill(palette.window_bg)
                .corner_radius(CornerRadius::same(8))
                .stroke(Stroke::new(1.0, palette.divider))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(box_w - 40.0);
                    ui.label(RichText::new(message).size(14.0).color(palette.window_fg));
                    ui.add_space(18.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ok = ui.add(
                            egui::Button::new(
                                RichText::new(tr::lng_box_ok().to_uppercase())
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_active_text_fg),
                            )
                            .frame(false),
                        );
                        if ok.hovered() {
                            let bg = Color32::from_rgba_unmultiplied(
                                palette.window_bg_active.r(),
                                palette.window_bg_active.g(),
                                palette.window_bg_active.b(),
                                24,
                            );
                            ui.painter().rect_stroke(
                                ok.rect,
                                CornerRadius::same(4),
                                Stroke::new(1.0, bg),
                                StrokeKind::Inside,
                            );
                        }
                        if ok.clicked() {
                            dismissed = true;
                        }
                    });
                });
        });

    dismissed
}
