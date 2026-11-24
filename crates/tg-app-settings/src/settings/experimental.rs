//! Experimental Settings section: categorized feature flags persisted to [`CoreSettings`],
//! real-time multi-word search, Restore Defaults, and `tdesktop-flags:` export/import.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/settings_experimental.h`
//! - `Telegram/SourceFiles/settings/settings_experimental.cpp`
//! - `Telegram/SourceFiles/settings/settings.style` (`settingsExperimentalTitle`,
//!   `settingsExperimentalAbout`, `settingsExperimentalTitlePadding`, `settingsExperimentalAboutPadding`)

use std::collections::BTreeMap;

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_core::CoreSettings;
use tg_lang::tr;
use tg_ui::anim::with_alpha;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::checkbox::{Toggle, paint_toggle_view};
use tg_ui::widgets::input_field::InputField;

use super::widgets::{
    SettingsIconKind, paint_tinted_icon, settings_divider, settings_row, settings_row_pad_x,
    subsection_title,
};

/// Clipboard code prefix used by tdesktop's Export/Import (`kOptionsClipboardPrefix`).
pub const OPTIONS_CLIPBOARD_PREFIX: &str = "tdesktop-flags:";

/// Descriptor for a single boolean experimental feature flag (`base::options::option<bool>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalOptionSpec {
    /// Stable option identifier (e.g. `"use-small-msg-bubble-radius"`).
    pub id: &'static str,
    /// Category heading matching `SetupExperimental` (`settings_experimental.cpp:441-524`).
    pub category: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Detailed explanation shown below the title (`settingsExperimentalAbout`).
    pub description: &'static str,
    /// Default value when unset.
    pub default_value: bool,
    /// Whether toggling this option requires restarting Rustgram.
    pub restart_required: bool,
    /// Whether this option is relevant on the current platform.
    pub relevant: bool,
}

/// Ordered category names matching `settings_experimental.cpp:441-524`.
pub const EXPERIMENTAL_CATEGORIES: &[&str] = &[
    "Chats",
    "Messages",
    "Profile",
    "Stickers and emoji",
    "Media",
    "Notifications",
    "Interface",
    "System",
];

/// Full catalog of Rustgram experimental options matching `settings_experimental.cpp`.
pub const EXPERIMENTAL_OPTIONS: &[ExperimentalOptionSpec] = &[
    // Chats
    ExperimentalOptionSpec {
        id: "forum-hide-chats-list",
        category: "Chats",
        name: "Hide chats list in forums",
        description: "Hide the main chat list column when browsing topics inside a forum group.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "dialogs-unread-on-top",
        category: "Chats",
        name: "Unread chats on top",
        description: "Keep unread chats grouped above read conversations inside custom folders.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "dialogs-mute-icon",
        category: "Chats",
        name: "Show mute icon in chat list",
        description: "Display a small muted speaker icon next to muted chat titles.",
        default_value: true,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "use-new-chat-view",
        category: "Chats",
        name: "Adaptive chat transitions",
        description: "Use smooth slide transitions when navigating between chats and topics.",
        default_value: true,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "auto-scroll-inactive-chat",
        category: "Chats",
        name: "Auto-scroll inactive chat",
        description: "Keep chat scrolled to the newest message even when the window is unfocused.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "moderate-common-groups",
        category: "Chats",
        name: "Moderate across common groups",
        description: "Show cross-group moderation options when banning spammers.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "classic-profile-scroll",
        category: "Chats",
        name: "Classic profile scroll",
        description: "Use classic scrolling layout instead of collapsible header in profile view.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    // Messages
    ExperimentalOptionSpec {
        id: "use-small-msg-bubble-radius",
        category: "Messages",
        name: "Small message bubble corners",
        description: "Render chat message bubbles with tighter corner rounding.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "unlimited-message-width",
        category: "Messages",
        name: "Unlimited message width",
        description: "Allow text bubbles to expand across the full chat column width.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "hide-ai-button",
        category: "Messages",
        name: "Hide Compose with AI button",
        description: "Hide the inline AI compose helper icon from the message input bar.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "force-compose-search-one-column",
        category: "Messages",
        name: "Single-column compose search",
        description: "Display inline search results in one column regardless of window width.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    // Profile
    ExperimentalOptionSpec {
        id: "view-profile-in-chats-list-context-menu",
        category: "Profile",
        name: "View Profile in chat list menu",
        description: "Add a direct 'View Profile' item to the right-click context menu on chats.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "show-peer-id-below-about",
        category: "Profile",
        name: "Show Peer IDs in Profile",
        description: "Show raw numeric Telegram user, group, and channel IDs in profile info.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "show-channel-joined-below-about",
        category: "Profile",
        name: "Show channel join date",
        description: "Display when you joined a channel below its description.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "profile-media-tabs",
        category: "Profile",
        name: "Profile shared media tabs",
        description: "Embed shared media, links, and music tabs directly inside the profile pane.",
        default_value: true,
        restart_required: false,
        relevant: true,
    },
    // Stickers and emoji
    ExperimentalOptionSpec {
        id: "tabbed-panel-show-on-click",
        category: "Stickers and emoji",
        name: "Open emoji panel on click only",
        description: "Disable hover-open for the emoji, stickers, and GIFs panel.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "unlimited-recent-stickers",
        category: "Stickers and emoji",
        name: "Unlimited recent stickers",
        description: "Lift the 20-sticker cap in the Recently Used stickers section.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    // Media
    ExperimentalOptionSpec {
        id: "disable-autoplay-next",
        category: "Media",
        name: "Disable auto-advance playlist",
        description: "Stop audio playback after the current track finishes instead of playing next.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "external-media-viewer",
        category: "Media",
        name: "Use external media viewer",
        description: "Open photos and videos in the OS default viewer instead of the built-in overlay.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "ffmpeg-multi-thread",
        category: "Media",
        name: "Multi-threaded FFmpeg decoding",
        description: "Use multi-threaded decoding for high-resolution H.264/HEVC/AV1 streams.",
        default_value: true,
        restart_required: true,
        relevant: true,
    },
    // Notifications
    ExperimentalOptionSpec {
        id: "hide-reply-button",
        category: "Notifications",
        name: "Hide Reply in notifications",
        description: "Omit the inline quick-reply field from desktop notification toasts.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "custom-notification",
        category: "Notifications",
        name: "Always allow custom notifications",
        description: "Prefer Rustgram's built-in notification popups even when OS toasts are available.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    // Interface
    ExperimentalOptionSpec {
        id: "fractional-scaling-enabled",
        category: "Interface",
        name: "Fractional DPI scaling",
        description: "Respect Windows 125% / 150% fractional display scaling without rounding.",
        default_value: true,
        restart_required: true,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "high-dpi-downscale",
        category: "Interface",
        name: "High-DPI thumbnail downscaling",
        description: "Use higher quality downsampling for media previews on 4K displays.",
        default_value: true,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "kinetic-scroller",
        category: "Interface",
        name: "Smooth kinetic scrolling",
        description: "Enable momentum-based smooth scrolling in chat history and settings.",
        default_value: true,
        restart_required: false,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "new-windows-size-as-first",
        category: "Interface",
        name: "Match main window size for popouts",
        description: "Open secondary windows with the same dimensions as the primary window.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
    // System
    ExperimentalOptionSpec {
        id: "prefer-ipv6",
        category: "System",
        name: "Prefer IPv6 connections",
        description: "Attempt IPv6 MTProto endpoints before IPv4 when dual-stack is available.",
        default_value: false,
        restart_required: true,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "skip-url-scheme-register",
        category: "System",
        name: "Skip tg:// URL registration",
        description: "Do not re-register the tg:// Windows protocol handler on startup.",
        default_value: false,
        restart_required: true,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "deadlock-detector",
        category: "System",
        name: "UI watchdog detector",
        description: "Log diagnostic warnings if the UI thread stalls for more than 5 seconds.",
        default_value: false,
        restart_required: true,
        relevant: true,
    },
    ExperimentalOptionSpec {
        id: "webview-debug-enabled",
        category: "System",
        name: "WebView DevTools enabled",
        description: "Allow opening Chromium DevTools inside Mini Apps and Instant View.",
        default_value: false,
        restart_required: false,
        relevant: true,
    },
];

/// Looks up an [`ExperimentalOptionSpec`] by its stable `id`.
#[must_use]
pub fn find_option_spec(id: &str) -> Option<&'static ExperimentalOptionSpec> {
    EXPERIMENTAL_OPTIONS.iter().find(|opt| opt.id == id)
}

/// Reads the effective boolean value of experimental option `id` from `core`.
#[must_use]
pub fn experimental_option_value(core: &CoreSettings, id: &str) -> bool {
    let default = find_option_spec(id).is_some_and(|s| s.default_value);
    core.experimental_options
        .get(id)
        .copied()
        .unwrap_or(default)
}

/// Updates experimental option `id` inside `core`, removing the override if it equals the default.
pub fn set_experimental_option(core: &mut CoreSettings, id: &str, value: bool) {
    let default = find_option_spec(id).is_some_and(|s| s.default_value);
    if value == default {
        core.experimental_options.remove(id);
    } else {
        core.experimental_options.insert(id.to_owned(), value);
    }
}

/// Returns `true` if any experimental option differs from its default value
/// (`base::options::changed()`).
#[must_use]
pub fn any_experimental_changed(core: &CoreSettings) -> bool {
    EXPERIMENTAL_OPTIONS
        .iter()
        .any(|opt| experimental_option_value(core, opt.id) != opt.default_value)
}

/// Resets all experimental options back to their default values (`base::options::reset()`).
pub fn reset_experimental_options(core: &mut CoreSettings) {
    core.experimental_options.clear();
}

/// Result of importing a `tdesktop-flags:` code (`ImportResult` in `settings_experimental.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentalImportResult {
    /// Successfully decoded and applied overrides.
    Done,
    /// Prefix, base64url, zlib framing, or JSON object was invalid.
    Invalid,
    /// Valid JSON object, but contained no supported experimental flags.
    Unsupported,
}

/// Encodes active experimental overrides into tdesktop's `tdesktop-flags:<base64url>` format
/// (`EncodeOptionsToText`, `settings_experimental.cpp:78-83`).
#[must_use]
pub fn encode_options_to_text(core: &CoreSettings) -> String {
    let json =
        serde_json::to_string(&core.experimental_options).unwrap_or_else(|_| "{}".to_owned());
    let payload = qcompress_stored(json.as_bytes());
    format!("{OPTIONS_CLIPBOARD_PREFIX}{}", base64url_encode(&payload))
}

/// Decodes a `tdesktop-flags:<base64url>` string into a map of option overrides
/// (`DecodeOptionsFromText` + `ImportOptions`, `settings_experimental.cpp:85-128`).
pub fn import_options_from_text(core: &mut CoreSettings, text: &str) -> ExperimentalImportResult {
    let trimmed = text.trim();
    let Some(encoded) = trimmed.strip_prefix(OPTIONS_CLIPBOARD_PREFIX) else {
        return ExperimentalImportResult::Invalid;
    };
    let Some(bytes) = base64url_decode(encoded) else {
        return ExperimentalImportResult::Invalid;
    };
    let json_bytes = match quncompress_stored(&bytes) {
        Some(raw) => raw,
        None => {
            // Also accept raw uncompressed UTF-8 JSON for forward compatibility.
            if bytes.first() == Some(&b'{') {
                bytes
            } else {
                return ExperimentalImportResult::Invalid;
            }
        }
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&json_bytes) else {
        return ExperimentalImportResult::Invalid;
    };
    let Some(obj) = val.as_object() else {
        return ExperimentalImportResult::Invalid;
    };
    let mut applied = BTreeMap::new();
    for (k, v) in obj {
        if let Some(b) = v.as_bool()
            && find_option_spec(k).is_some()
        {
            applied.insert(k.clone(), b);
        }
    }
    if applied.is_empty() && !obj.is_empty() {
        return ExperimentalImportResult::Unsupported;
    }
    reset_experimental_options(core);
    for (k, b) in applied {
        set_experimental_option(core, &k, b);
    }
    ExperimentalImportResult::Done
}

/// Splits `text` into lowercase search terms (`SearchWords` in `settings_experimental.cpp`).
#[must_use]
pub fn search_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Returns `true` when every query word matches at least one prefix/substring in `searchable`
/// (`MatchesWords` in `settings_experimental.cpp`).
#[must_use]
pub fn matches_experimental_query(opt: &ExperimentalOptionSpec, query_words: &[String]) -> bool {
    if query_words.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {}",
        opt.id.to_lowercase(),
        opt.category.to_lowercase(),
        opt.name.to_lowercase(),
        opt.description.to_lowercase()
    );
    query_words.iter().all(|w| haystack.contains(w.as_str()))
}

/// UI state for the Experimental Settings section (`Settings::Experimental`).
#[derive(Debug, Clone, Default)]
pub struct ExperimentalSettingsState {
    /// Live search query filtering experimental options.
    pub search_query: String,
    /// Inline import dialog open flag.
    pub import_open: bool,
    /// Text buffer for importing a `tdesktop-flags:` code.
    pub import_code_input: String,
    /// Highlighted option ID (e.g. opened via `tg://settings/experimental/<id>`).
    pub highlighted_id: Option<String>,
    /// Scrolls the highlighted option row into view on the next frame.
    pub scroll_to_highlighted: bool,
}

impl ExperimentalSettingsState {
    /// Highlights an experimental option opened via `tg://settings/experimental/<id>`
    /// (accepting either `<id>` or `"experimental/<id>"` as registered in `settings_experimental.cpp`).
    pub fn highlight_option(&mut self, raw_id: &str) {
        let stripped = raw_id
            .trim()
            .strip_prefix("experimental/")
            .unwrap_or(raw_id.trim())
            .trim()
            .to_ascii_lowercase();
        if stripped.is_empty() {
            self.highlighted_id = None;
            self.scroll_to_highlighted = false;
        } else {
            self.search_query.clear();
            self.highlighted_id = Some(stripped);
            self.scroll_to_highlighted = true;
        }
    }
}

/// Renders the Experimental Settings section (`settings_experimental.cpp`).
///
/// Returns an optional toast message when an action is performed (e.g. export, import, copy deep
/// link, restart notice, or restore defaults).
pub fn render_experimental_section(
    ui: &mut Ui,
    state: &mut ExperimentalSettingsState,
    core: &mut CoreSettings,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    let pad_x = settings_row_pad_x();

    // 1. Pinned search bar + Export / Import action strip.
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let btn_area = 156.0;
        let search_w = (ui.available_width() - pad_x - btn_area).max(120.0);
        let search_placeholder = tr::lng_dlg_filter();
        InputField::single_line(
            "settings_experimental_search",
            &mut state.search_query,
            &search_placeholder,
            InputFieldStyle::rounded_search(colors),
        )
        .width(search_w)
        .show(ui);

        ui.add_space(8.0);
        if RoundButton::new("Export", RoundButtonStyle::default_light(colors))
            .show(ui)
            .clicked()
        {
            let code = encode_options_to_text(core);
            ui.ctx().copy_text(code);
            toast = Some("Experimental settings code copied to clipboard.".to_owned());
        }
        ui.add_space(4.0);
        if RoundButton::new("Import", RoundButtonStyle::default_light(colors))
            .show(ui)
            .clicked()
        {
            state.import_open = !state.import_open;
        }
    });

    if state.import_open {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            let avail = (ui.available_width() - pad_x - 86.0).max(120.0);
            InputField::single_line(
                "settings_experimental_import_code",
                &mut state.import_code_input,
                "Paste tdesktop-flags:... code",
                InputFieldStyle::default_input(colors),
            )
            .width(avail)
            .show(ui);
            ui.add_space(6.0);
            let save_lbl = tr::lng_settings_save();
            if RoundButton::new(&save_lbl, RoundButtonStyle::default_active(colors))
                .show(ui)
                .clicked()
            {
                match import_options_from_text(core, &state.import_code_input) {
                    ExperimentalImportResult::Done => {
                        state.import_code_input.clear();
                        state.import_open = false;
                        toast = Some("Experimental settings imported.".to_owned());
                    }
                    ExperimentalImportResult::Invalid => {
                        toast = Some("This is not a valid experimental settings code.".to_owned());
                    }
                    ExperimentalImportResult::Unsupported => {
                        toast = Some(
                            "Experimental settings code is valid, but data format is not supported."
                                .to_owned(),
                        );
                    }
                }
            }
        });
    }

    let query_words = search_words(&state.search_query);

    // 2. Warning header & conditional "Restore default values" button (hidden while searching).
    if query_words.is_empty() {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            let max_w = (ui.available_width() - pad_x).max(160.0);
            ui.vertical(|ui| {
                ui.set_max_width(max_w);
                ui.label(
                    egui::RichText::new(tr::lng_settings_experimental_about())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
        });

        if any_experimental_changed(core) {
            ui.add_space(8.0);
            settings_divider(ui, None, colors);
            let restore_lbl = tr::lng_settings_experimental_restore();
            if settings_row(
                ui,
                &restore_lbl,
                None,
                None,
                Some(SettingsIconKind::Debug),
                false,
                false,
                colors,
            )
            .clicked()
            {
                reset_experimental_options(core);
                toast = Some("Restored experimental settings to default values.".to_owned());
            }
        }
        settings_divider(ui, None, colors);
    } else {
        ui.add_space(6.0);
    }

    // 3. Categorized option rows.
    let mut matched_any = false;
    for &category in EXPERIMENTAL_CATEGORIES {
        let cat_opts: Vec<&ExperimentalOptionSpec> = EXPERIMENTAL_OPTIONS
            .iter()
            .filter(|o| o.category == category && matches_experimental_query(o, &query_words))
            .collect();
        if cat_opts.is_empty() {
            continue;
        }
        matched_any = true;
        subsection_title(ui, category, colors);

        for opt in cat_opts {
            let current = experimental_option_value(core, opt.id);
            let is_highlighted = state.highlighted_id.as_deref() == Some(opt.id);
            let should_scroll = is_highlighted && state.scroll_to_highlighted;
            let row_toast = render_experimental_option_row(
                ui,
                opt,
                current,
                is_highlighted,
                should_scroll,
                core,
                colors,
            );
            if should_scroll {
                state.scroll_to_highlighted = false;
            }
            if row_toast.is_some() {
                toast = row_toast;
            }
        }
        settings_divider(ui, None, colors);
    }

    if !matched_any {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "No experimental options matching \"{}\".",
                    state.search_query.trim()
                ))
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowSubTextFg")),
            );
        });
    }

    ui.add_space(16.0);
    toast
}

/// Renders a single experimental option row using tdesktop's `settingsExperimentalTitlePadding`
/// (`22px, 9px, 66px, 1px`) and `settingsExperimentalAboutPadding` (`22px, 0px, 66px, 9px`).
#[allow(clippy::too_many_arguments)]
fn render_experimental_option_row(
    ui: &mut Ui,
    opt: &ExperimentalOptionSpec,
    current: bool,
    highlighted: bool,
    scroll_into_view: bool,
    core: &mut CoreSettings,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    let pad_left = st::settings_experimental_title_padding
        .left
        .scaled()
        .max(18) as f32;
    let pad_right = st::settings_experimental_title_padding
        .right
        .scaled()
        .max(64) as f32;
    let avail_w = ui.available_width().max(200.0);
    let text_w = (avail_w - pad_left - pad_right).max(100.0);

    let title_font = FontId::new(14.0, family_semibold());
    let about_font = FontId::new(12.5, family_regular());
    let title_color = if opt.relevant {
        colors.color("windowBoldFg")
    } else {
        colors.color("windowSubTextFg")
    };
    let about_color = colors.color("windowSubTextFg");

    let title_galley = ui
        .painter()
        .layout(opt.name.to_owned(), title_font, title_color, text_w);
    let about_galley =
        ui.painter()
            .layout(opt.description.to_owned(), about_font, about_color, text_w);

    let changed = current != opt.default_value;
    let restart_h = if opt.restart_required && changed {
        18.0
    } else {
        0.0
    };
    let row_h =
        (9.0 + title_galley.size().y + 3.0 + about_galley.size().y + restart_h + 9.0).max(52.0);

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, opt.relevant, current, opt.name)
    });
    if resp.hovered() && opt.relevant {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if highlighted {
        if scroll_into_view {
            resp.scroll_to_me(Some(egui::Align::Center));
        }
        ui.painter().rect_filled(
            rect,
            CornerRadius::ZERO,
            with_alpha(colors.color("windowBgActive"), 0.14),
        );
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
            CornerRadius::ZERO,
            colors.color("windowBgActive"),
        );
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    let title_pos = Pos2::new(rect.left() + pad_left, rect.top() + 9.0);
    let about_pos = Pos2::new(
        rect.left() + pad_left,
        title_pos.y + title_galley.size().y + 3.0,
    );
    ui.painter().galley(title_pos, title_galley, title_color);
    ui.painter().galley(about_pos, about_galley, about_color);

    if opt.restart_required && changed {
        let badge_pos = Pos2::new(rect.left() + pad_left, rect.bottom() - 10.0);
        ui.painter().text(
            badge_pos,
            Align2::LEFT_BOTTOM,
            "Restart required to apply",
            FontId::new(11.0, family_semibold()),
            colors.color("windowActiveTextFg"),
        );
    }

    let toggle_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&toggle_st);
    let toggle_center = Pos2::new(rect.right() - 22.0 - tsize.x * 0.5, rect.center().y);
    let toggle_rect = Rect::from_center_size(toggle_center, tsize);
    paint_toggle_view(
        ui.painter(),
        toggle_rect,
        &toggle_st,
        if current { 1.0 } else { 0.0 },
        if opt.relevant { 1.0 } else { 0.45 },
    );

    if resp.clicked() {
        if !opt.relevant {
            toast = Some(tr::lng_settings_experimental_irrelevant());
        } else {
            let next = !current;
            set_experimental_option(core, opt.id, next);
            if opt.restart_required {
                toast = Some(format!("{} updated — restart required.", opt.name));
            }
        }
    }

    // Right-click copies `tg://settings/experimental/<id>` (`SetupCopyDeepLink`).
    if resp.secondary_clicked() {
        let link = format!("tg://settings/experimental/{}", opt.id);
        ui.ctx().copy_text(link);
        toast = Some("Deep link copied to clipboard.".to_owned());
    }

    // Keep unused import clean when icon badge isn't drawn on plain rows.
    let _ = paint_tinted_icon;
    toast
}

// ============================================================================
// Pure-Rust Base64URL + qCompress (RFC 1950 stored-block zlib) helpers
// ============================================================================

const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64URL_ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(BASE64URL_ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64URL_ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(BASE64URL_ALPHABET[(n & 0x3F) as usize] as char);
        }
    }
    out
}

fn base64url_val(b: u8) -> Option<u32> {
    match b {
        b'A'..=b'Z' => Some(u32::from(b - b'A')),
        b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let v0 = base64url_val(chunk[0])?;
        let v1 = base64url_val(chunk[1])?;
        out.push(((v0 << 2) | (v1 >> 4)) as u8);
        if let Some(&c2) = chunk.get(2) {
            let v2 = base64url_val(c2)?;
            out.push((((v1 & 0x0F) << 4) | (v2 >> 2)) as u8);
            if let Some(&c3) = chunk.get(3) {
                let v3 = base64url_val(c3)?;
                out.push((((v2 & 0x03) << 6) | v3) as u8);
            }
        }
    }
    Some(out)
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// Encodes `raw` in Qt's `qCompress` wire format (4-byte big-endian length + RFC 1950 zlib
/// stream using uncompressed DEFLATE blocks, valid for any standard zlib / `qUncompress` decoder).
fn qcompress_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    // Zlib header: CMF=0x78 (DEFLATE, 32K window), FLG=0x01 (FCHECK valid).
    out.push(0x78);
    out.push(0x01);
    if raw.is_empty() {
        out.push(0x01); // BFINAL=1, BTYPE=00 (stored)
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(!0u16).to_le_bytes());
    } else {
        let chunks: Vec<&[u8]> = raw.chunks(u16::MAX as usize).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i + 1 == chunks.len();
            out.push(u8::from(is_last));
            let len = chunk.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Decodes a `qCompress` stored-block zlib stream back to raw bytes, verifying length and Adler-32.
fn quncompress_stored(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 11 {
        return None;
    }
    let expected_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if expected_len > 256 * 1024 {
        return None;
    }
    let cmf = data[4];
    let flg = data[5];
    if (cmf & 0x0F) != 8 || ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
        return None;
    }
    let mut pos = 6usize;
    let mut out = Vec::with_capacity(expected_len);
    loop {
        let header = *data.get(pos)?;
        pos += 1;
        let bfinal = (header & 0x01) != 0;
        let btype = (header >> 1) & 0x03;
        if btype != 0 {
            return None;
        }
        let len = u16::from_le_bytes([*data.get(pos)?, *data.get(pos + 1)?]);
        let nlen = u16::from_le_bytes([*data.get(pos + 2)?, *data.get(pos + 3)?]);
        pos += 4;
        if len != !nlen {
            return None;
        }
        let end = pos.checked_add(usize::from(len))?;
        let slice = data.get(pos..end)?;
        out.extend_from_slice(slice);
        pos = end;
        if bfinal {
            break;
        }
    }
    let expected_checksum = u32::from_be_bytes([
        *data.get(pos)?,
        *data.get(pos + 1)?,
        *data.get(pos + 2)?,
        *data.get(pos + 3)?,
    ]);
    if out.len() != expected_len || adler32(&out) != expected_checksum {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_reset_and_core_settings_persistence() {
        let mut core = CoreSettings::new();
        assert!(!any_experimental_changed(&core));
        assert!(!experimental_option_value(&core, "unlimited-message-width"));

        set_experimental_option(&mut core, "unlimited-message-width", true);
        set_experimental_option(&mut core, "dialogs-mute-icon", false);
        assert!(any_experimental_changed(&core));
        assert!(experimental_option_value(&core, "unlimited-message-width"));
        assert!(!experimental_option_value(&core, "dialogs-mute-icon"));

        // Round-trip through CoreSettings binary serialization in a tempdir.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.bin");
        std::fs::write(&path, core.serialize()).expect("write settings");

        let bytes = std::fs::read(&path).expect("read settings");
        let mut reloaded = CoreSettings::new();
        reloaded
            .add_from_serialized(&bytes, tg_core::version::TDESKTOP_COMPAT_VERSION)
            .expect("deserialize settings");
        assert!(experimental_option_value(
            &reloaded,
            "unlimited-message-width"
        ));
        assert!(!experimental_option_value(&reloaded, "dialogs-mute-icon"));

        reset_experimental_options(&mut reloaded);
        assert!(!any_experimental_changed(&reloaded));
    }

    #[test]
    fn export_and_import_tdesktop_flags_round_trip() {
        let mut core = CoreSettings::new();
        set_experimental_option(&mut core, "show-peer-id-below-about", true);
        set_experimental_option(&mut core, "use-small-msg-bubble-radius", true);

        let code = encode_options_to_text(&core);
        assert!(code.starts_with(OPTIONS_CLIPBOARD_PREFIX));

        let mut target = CoreSettings::new();
        assert_eq!(
            import_options_from_text(&mut target, &code),
            ExperimentalImportResult::Done
        );
        assert!(experimental_option_value(
            &target,
            "show-peer-id-below-about"
        ));
        assert!(experimental_option_value(
            &target,
            "use-small-msg-bubble-radius"
        ));
        assert_eq!(
            import_options_from_text(&mut target, "not-a-valid-code"),
            ExperimentalImportResult::Invalid
        );
    }

    #[test]
    fn multi_word_search_filters_options() {
        let words = search_words("peer id profile");
        let hits: Vec<_> = EXPERIMENTAL_OPTIONS
            .iter()
            .filter(|o| matches_experimental_query(o, &words))
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "show-peer-id-below-about");
    }
}
