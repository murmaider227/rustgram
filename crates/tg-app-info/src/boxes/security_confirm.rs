//! Security confirmation modals for hidden links and external file launches.
//!
//! Port origin:
//! - `Telegram/SourceFiles/core/click_handler_types.cpp:504-655` (`HiddenUrlClickHandler::Open`)
//! - `Telegram/SourceFiles/data/data_document_resolver.cpp:44-156` (`ConfirmDontWarnBox`, `LaunchWithWarning`)

use egui::text::{LayoutJob, TextFormat};
use egui::{CornerRadius, Frame, Margin, Stroke, Ui};
use tg_app_core::media::backend::MediaItemId;
use tg_lang::tr;
use tg_ui::anim::{SimpleAnimation, Transition, with_alpha};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::FlatLabel;

// ---------------------------------------------------------------------------
// 1. Hidden Link Confirmation Box (`HiddenUrlClickHandler::Open`)
// ---------------------------------------------------------------------------

/// Confirmation dialog shown before opening an untrusted hidden link (`messageEntityTextUrl`)
/// or a sandboxed widget `ConfirmExternal` link.
///
/// Mirrors tdesktop's `HiddenUrlClickHandler::Open` (`core/click_handler_types.cpp:590-655`):
/// - Displays the real destination URL without truncation, bolding the real host (skipping
///   leading `www.`).
/// - Highlights mixed-script / confusable host characters in `attentionButtonFg` and displays
///   `lng_open_link_suspicious_chars`.
/// - Provides a `"Copy Link"` button (`lng_context_copy_link`) alongside `"Cancel"` and `"Open"`.
#[derive(Debug, Clone)]
pub struct HiddenLinkConfirmBoxState {
    /// Validated destination URL (must have already passed `tg_platform::url_policy::validate_external_url`).
    pub url: String,
    /// Optional UTF-8 byte range `(start, end)` of the host inside `url` for bolding.
    pub host_range: Option<(usize, usize)>,
    /// Suspicious mixed-script UTF-8 byte ranges inside `url`'s host, if any.
    pub suspicious_ranges: Vec<(usize, usize)>,
    /// Set to `true` after the user copies the link from this dialog.
    pub copied: bool,
    prev_copied: bool,
    copied_anim: SimpleAnimation,
}

impl HiddenLinkConfirmBoxState {
    /// Creates a new hidden-link confirmation modal for a validated `url`.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        host_range: Option<(usize, usize)>,
        suspicious_ranges: Vec<(usize, usize)>,
    ) -> Self {
        Self {
            url: url.into(),
            host_range,
            suspicious_ranges,
            copied: false,
            prev_copied: false,
            copied_anim: SimpleAnimation::new(),
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor matching `st::box_width` and `st::default_box`.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_open_this_link())
            .width(st::box_width.0 as f32 + 40.0)
            .with_left_button(BoxButton::default_btn("copy", tr::lng_context_copy_link()))
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("open", tr::lng_open_link()))
    }

    /// Renders the destination URL (bolding the real host or highlighting suspicious characters)
    /// and optional homograph warning inside `ui`.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let regular_font = egui::FontId::new(14.0, family_regular());
        let semibold_font = egui::FontId::new(14.0, family_semibold());
        let text_fg = colors.color("windowFg");
        let bold_fg = colors.color("windowBoldFg");
        let danger_fg = colors.color("attentionButtonFg");
        let sub_bg = colors.color("windowBgOver");
        let divider_fg = colors.color("shadowFg");

        // Render the full URL inside a calm tinted container so long URLs wrap cleanly to the
        // container's true inner width and remain visually distinct from prose without truncation.
        Frame::NONE
            .fill(sub_bg)
            .stroke(Stroke::new(1.0, with_alpha(divider_fg, 0.25)))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                let inner_w = ui.available_width().max(160.0);
                ui.set_min_width(inner_w);

                let mut job = LayoutJob {
                    wrap: egui::text::TextWrapping {
                        max_width: inner_w,
                        break_anywhere: true,
                        ..Default::default()
                    },
                    ..Default::default()
                };

                let normal_fmt = TextFormat {
                    font_id: regular_font,
                    color: text_fg,
                    line_height: Some(21.0),
                    ..Default::default()
                };
                let host_fmt = TextFormat {
                    font_id: semibold_font.clone(),
                    color: bold_fg,
                    line_height: Some(21.0),
                    ..Default::default()
                };
                let suspicious_fmt = TextFormat {
                    font_id: semibold_font,
                    color: danger_fg,
                    line_height: Some(21.0),
                    ..Default::default()
                };

                if !self.suspicious_ranges.is_empty() {
                    // Exact port of `HighlightSuspicious` (`click_handler_types.cpp:93-107`).
                    let mut cursor = 0usize;
                    for &(from, to) in &self.suspicious_ranges {
                        if from > cursor
                            && from <= self.url.len()
                            && self.url.is_char_boundary(from)
                        {
                            job.append(&self.url[cursor..from], 0.0, normal_fmt.clone());
                        }
                        if to > from
                            && to <= self.url.len()
                            && self.url.is_char_boundary(from)
                            && self.url.is_char_boundary(to)
                        {
                            job.append(&self.url[from..to], 0.0, suspicious_fmt.clone());
                            cursor = to;
                        }
                    }
                    if cursor < self.url.len() {
                        job.append(&self.url[cursor..], 0.0, normal_fmt);
                    }
                } else if let Some((host_start, host_end)) = self.host_range
                    && host_start < host_end
                    && host_end <= self.url.len()
                    && self.url.is_char_boundary(host_start)
                    && self.url.is_char_boundary(host_end)
                {
                    // Exact port of `BoldDomainInUrl` (`click_handler_types.cpp:109-132`): bold the
                    // host, skipping a leading `www.`.
                    let bold_start = if self.url[host_start..host_end]
                        .to_ascii_lowercase()
                        .starts_with("www.")
                    {
                        host_start + 4
                    } else {
                        host_start
                    };
                    if bold_start > 0 {
                        job.append(&self.url[..bold_start], 0.0, normal_fmt.clone());
                    }
                    if host_end > bold_start {
                        job.append(&self.url[bold_start..host_end], 0.0, host_fmt);
                    }
                    if host_end < self.url.len() {
                        job.append(&self.url[host_end..], 0.0, normal_fmt);
                    }
                } else {
                    job.append(&self.url, 0.0, normal_fmt);
                }

                ui.label(job);
            });

        if !self.suspicious_ranges.is_empty() {
            ui.add_space(12.0);
            let mut warn_style = FlatLabelStyle::box_label(colors);
            warn_style.text_fg = danger_fg;
            FlatLabel::plain(&tr::lng_open_link_suspicious_chars(), warn_style)
                .id_salt("hidden_link_suspicious_warn")
                .show(ui);
        }

        let now = ui.ctx().input(|i| i.time);
        if self.copied != self.prev_copied {
            self.prev_copied = self.copied;
            self.copied_anim.change(
                now,
                if self.copied { 1.0 } else { 0.0 },
                160,
                Transition::EaseOutCubic,
            );
            ui.ctx().request_repaint();
        }
        let copied_alpha = self
            .copied_anim
            .sample(now, if self.copied { 1.0 } else { 0.0 });
        if self.copied_anim.animating() {
            ui.ctx().request_repaint();
        }
        if copied_alpha > 0.01 {
            ui.add_space(8.0);
            let mut copied_style = FlatLabelStyle::box_label(colors);
            copied_style.text_fg = with_alpha(colors.color("windowSubTextFg"), copied_alpha);
            FlatLabel::plain(&tr::lng_username_copied(), copied_style)
                .id_salt("hidden_link_copied_note")
                .show(ui);
        }
    }
}

// ---------------------------------------------------------------------------
// 2. External File Launch Warning Box (`Data::LaunchWithWarning`)
// ---------------------------------------------------------------------------

/// Classification of why launching a downloaded file externally requires user confirmation
/// (`Data::LauncherWouldWarn` + `Data::LaunchWithWarning` in `data/data_document_resolver.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchWarningKind {
    /// Known executable or script extension (`WINDOWS_EXECUTABLE_EXTS`).
    Executable {
        /// Lowercase extension without leading dot (e.g. `"exe"`, `"bat"`).
        extension: String,
    },
    /// Unknown file extension not on Telegram's benign list.
    UnknownExtension {
        /// Lowercase extension without leading dot.
        extension: String,
    },
    /// File without an extension (Windows may resolve sibling `.bat`/`.exe`; always warns).
    Extensionless,
    /// File extension that can reveal the user's IP address when opened externally (`.svg`, `.html`, ...).
    IpRevealing {
        /// Lowercase extension without leading dot.
        extension: String,
    },
}

/// Confirmation dialog shown before launching an executable, unknown, extensionless, or
/// IP-revealing file in an external OS application (`Data::LaunchWithWarning` + `ConfirmDontWarnBox`).
#[derive(Debug, Clone)]
pub struct LaunchFileConfirmBoxState {
    /// Target media item ID to launch if confirmed.
    pub item_id: MediaItemId,
    /// Sanitized file name displayed as the modal title.
    pub file_name: String,
    /// Specific launch warning category driving copy, button emphasis, and remember-checkbox rules.
    pub kind: LaunchWarningKind,
    /// Whether the user checked "Remember for this file type" / "Don't ask me again".
    pub dont_ask_again: bool,
    prev_dont_ask: bool,
    hint_anim: SimpleAnimation,
}

impl LaunchFileConfirmBoxState {
    /// Creates a new external-launch confirmation modal with a pre-sanitized display `file_name`.
    #[must_use]
    pub fn new(
        item_id: MediaItemId,
        file_name: impl Into<String>,
        kind: LaunchWarningKind,
    ) -> Self {
        Self {
            item_id,
            file_name: file_name.into(),
            kind,
            dont_ask_again: false,
            prev_dont_ask: false,
            hint_anim: SimpleAnimation::new(),
        }
    }

    /// Returns `true` if this warning allows remembering the user's choice (`canRemember` in
    /// `data_document_resolver.cpp:140-141`). Extensionless files can never be suppressed.
    #[must_use]
    pub fn can_remember(&self) -> bool {
        match &self.kind {
            LaunchWarningKind::Extensionless => false,
            LaunchWarningKind::IpRevealing { .. } => true,
            LaunchWarningKind::Executable { extension }
            | LaunchWarningKind::UnknownExtension { extension } => !extension.contains(' '),
        }
    }

    /// Returns the extension to add to `CoreSettings::no_warning_extensions` if the user confirmed
    /// with `dont_ask_again == true` (excluding IP-revealing files which flip `ip_reveal_warning`).
    #[must_use]
    pub fn remembered_extension(&self) -> Option<&str> {
        if !self.dont_ask_again || !self.can_remember() {
            return None;
        }
        match &self.kind {
            LaunchWarningKind::Executable { extension }
            | LaunchWarningKind::UnknownExtension { extension } => Some(extension.as_str()),
            LaunchWarningKind::Extensionless | LaunchWarningKind::IpRevealing { .. } => None,
        }
    }

    /// Returns `true` if confirming with `dont_ask_again == true` should disable `ip_reveal_warning`.
    #[must_use]
    pub const fn should_disable_ip_reveal_warning(&self) -> bool {
        self.dont_ask_again && matches!(self.kind, LaunchWarningKind::IpRevealing { .. })
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor with destructive red emphasis on `"Run"`.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let confirm_btn = match self.kind {
            LaunchWarningKind::Executable { .. } | LaunchWarningKind::Extensionless => {
                BoxButton::attention("launch", tr::lng_launch_exe_sure())
            }
            LaunchWarningKind::UnknownExtension { .. } | LaunchWarningKind::IpRevealing { .. } => {
                BoxButton::default_btn("launch", tr::lng_launch_other_sure())
            }
        };
        BoxModal::new(id, &self.file_name)
            .width(st::box_width.0 as f32 + 40.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(confirm_btn)
    }

    /// Renders the localized launch warning text, optional "Remember for this file type" checkbox,
    /// and smooth `180ms` `EaseOutCubic` disclosure of `lng_launch_dont_ask_settings`.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let regular_font = egui::FontId::new(14.0, family_regular());
        let semibold_font = egui::FontId::new(14.0, family_semibold());
        let text_fg = colors.color("windowFg");
        let bold_fg = colors.color("windowBoldFg");

        let (raw_body, bold_token) = match &self.kind {
            LaunchWarningKind::Executable { extension } => {
                let token = format!(".{extension}");
                (tr::lng_launch_exe_warning(&token), Some(token))
            }
            LaunchWarningKind::Extensionless => {
                let token = "(no extension)".to_owned();
                (tr::lng_launch_exe_warning(&token), Some(token))
            }
            LaunchWarningKind::UnknownExtension { extension } => {
                let token = format!(".{extension}");
                (tr::lng_launch_other_warning(&token), Some(token))
            }
            LaunchWarningKind::IpRevealing { .. } => (tr::lng_launch_svg_warning(), None),
        };

        let mut job = LayoutJob {
            wrap: egui::text::TextWrapping {
                max_width: ui.available_width().max(160.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let normal_fmt = TextFormat {
            font_id: regular_font,
            color: text_fg,
            line_height: Some(22.0),
            ..Default::default()
        };
        let bold_fmt = TextFormat {
            font_id: semibold_font,
            color: bold_fg,
            line_height: Some(22.0),
            ..Default::default()
        };

        if let Some(ref token) = bold_token
            && let Some(pos) = raw_body.find(token.as_str())
        {
            if pos > 0 {
                job.append(&raw_body[..pos], 0.0, normal_fmt.clone());
            }
            job.append(&raw_body[pos..pos + token.len()], 0.0, bold_fmt);
            if pos + token.len() < raw_body.len() {
                job.append(&raw_body[pos + token.len()..], 0.0, normal_fmt);
            }
        } else {
            job.append(&raw_body, 0.0, normal_fmt);
        }

        ui.label(job);

        if self.can_remember() {
            ui.add_space(14.0);
            let check_label = if matches!(self.kind, LaunchWarningKind::IpRevealing { .. }) {
                tr::lng_launch_exe_dont_ask()
            } else {
                tr::lng_launch_dont_ask()
            };
            Checkbox::new(
                &mut self.dont_ask_again,
                &check_label,
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);

            let now = ui.ctx().input(|i| i.time);
            if self.dont_ask_again != self.prev_dont_ask {
                self.prev_dont_ask = self.dont_ask_again;
                self.hint_anim.change(
                    now,
                    if self.dont_ask_again { 1.0 } else { 0.0 },
                    180,
                    Transition::EaseOutCubic,
                );
                ui.ctx().request_repaint();
            }

            let alpha = self
                .hint_anim
                .sample(now, if self.dont_ask_again { 1.0 } else { 0.0 });
            if self.hint_anim.animating() {
                ui.ctx().request_repaint();
            }
            if alpha > 0.01 {
                ui.add_space(8.0);
                let mut hint_style = FlatLabelStyle::box_label(colors);
                hint_style.text_fg = with_alpha(colors.color("windowSubTextFg"), alpha);
                FlatLabel::plain(&tr::lng_launch_dont_ask_settings(), hint_style)
                    .id_salt("launch_dont_ask_settings_hint")
                    .show(ui);
            }
        }
    }
}
