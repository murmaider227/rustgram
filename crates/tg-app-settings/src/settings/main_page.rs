//! Main Settings landing page: profile cover, settings search (opened from the header search
//! icon), section rows, Default Interface Scale slider, Premium rows, and the help rows.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_main.cpp`,
//! `Telegram/SourceFiles/settings/settings_search.cpp`, and
//! `Telegram/SourceFiles/settings/sections/settings_premium.cpp` (placeholder row only).

use egui::{Align2, Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Ui, Vec2};
use tg_core::{CoreSettings, GlobalConfig};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::resolve_color_ref;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::SettingsSection;
use super::backend::SettingsBackend;
use super::faq_suggestions::FaqSuggestionsState;
use super::recent_searches::RecentSettingsSearches;
use super::scale::{ScaleChange, render_scale_controls};
use super::strings;
use super::widgets::{
    SETTINGS_LIST_SKIP, SettingsIconKind, paint_tinted_icon, settings_card, settings_divider,
    settings_row, settings_row_pad_x, settings_section_divider, subsection_title,
};

/// Indexed settings search entry mirroring `Settings::SearchEntry` (`settings_search.cpp`).
#[derive(Debug, Clone, Copy)]
pub struct SettingsSearchEntry {
    /// Stable entry ID persisted in [`RecentSettingsSearches`].
    pub id: &'static str,
    /// Human-readable setting title.
    pub title: &'static str,
    /// Subtitle / breadcrumb path.
    pub section_name: &'static str,
    /// Space-separated search keywords.
    pub keywords: &'static str,
    /// Target section to open when clicked.
    pub target: SettingsSection,
    /// Category icon.
    pub icon: SettingsIconKind,
}

/// Complete keyword search index across all Settings sections.
pub const SEARCH_INDEX: &[SettingsSearchEntry] = &[
    SettingsSearchEntry {
        id: "account",
        title: strings::SEARCH_ENTRY_ACCOUNT_TITLE,
        section_name: "My Account",
        keywords: "account profile name username bio phone birthday photo avatar logout",
        target: SettingsSection::Information,
        icon: SettingsIconKind::Account,
    },
    SettingsSearchEntry {
        id: "notifications",
        title: strings::SEARCH_ENTRY_NOTIFY_TITLE,
        section_name: "Notifications and Sounds",
        keywords: "notifications sounds corner position count badge muted preview flash",
        target: SettingsSection::Notifications,
        icon: SettingsIconKind::Notifications,
    },
    SettingsSearchEntry {
        id: "notifications_reactions",
        title: "Reactions",
        section_name: "Notifications and Sounds",
        keywords: "reactions messages poll votes sender preview notifications",
        target: SettingsSection::NotificationsReactions,
        icon: SettingsIconKind::Notifications,
    },
    SettingsSearchEntry {
        id: "privacy",
        title: strings::SEARCH_ENTRY_PRIVACY_TITLE,
        section_name: "Privacy and Security",
        keywords: "privacy phone last seen online forwards calls groups voice bio birthday blocked ttl",
        target: SettingsSection::PrivacySecurity,
        icon: SettingsIconKind::Privacy,
    },
    SettingsSearchEntry {
        id: "local_passcode",
        title: strings::SEARCH_ENTRY_PASSCODE_TITLE,
        section_name: "Privacy and Security",
        keywords: "local passcode lock auto-lock timeout windows hello",
        target: SettingsSection::LocalPasscode,
        icon: SettingsIconKind::Lock,
    },
    SettingsSearchEntry {
        id: "cloud_password",
        title: strings::SEARCH_ENTRY_CLOUD_PWD_TITLE,
        section_name: "Privacy and Security",
        keywords: "2fa two-step cloud password hint recovery email srp",
        target: SettingsSection::CloudPassword,
        icon: SettingsIconKind::Key,
    },
    SettingsSearchEntry {
        id: "passkeys",
        title: "Passkeys",
        section_name: "Privacy and Security",
        keywords: "passkey passkeys webauthn fido2 security key biometrics hello",
        target: SettingsSection::Passkeys,
        icon: SettingsIconKind::Key,
    },
    SettingsSearchEntry {
        id: "sessions",
        title: strings::SEARCH_ENTRY_SESSIONS_TITLE,
        section_name: "Privacy and Security",
        keywords: "sessions devices authorizations terminate logout ip location",
        target: SettingsSection::ActiveSessions,
        icon: SettingsIconKind::Sessions,
    },
    SettingsSearchEntry {
        id: "websites",
        title: strings::SEARCH_ENTRY_WEBSITES_TITLE,
        section_name: "Privacy and Security",
        keywords: "websites connected login widget bots disconnect authorizations",
        target: SettingsSection::ConnectedWebsites,
        icon: SettingsIconKind::Network,
    },
    SettingsSearchEntry {
        id: "chat",
        title: strings::SEARCH_ENTRY_CHAT_TITLE,
        section_name: "Chat Settings",
        keywords: "theme night dark day classic tinted accent color wallpaper background font enter emoji stickers autoplay quick action swipe double click",
        target: SettingsSection::Chat,
        icon: SettingsIconKind::Chat,
    },
    SettingsSearchEntry {
        id: "folders",
        title: strings::SEARCH_ENTRY_FOLDERS_TITLE,
        section_name: "Folders",
        keywords: "folders filters sidebar tabs recommended",
        target: SettingsSection::Folders,
        icon: SettingsIconKind::Folders,
    },
    SettingsSearchEntry {
        id: "advanced",
        title: strings::SEARCH_ENTRY_ADVANCED_TITLE,
        section_name: "Advanced",
        keywords: "proxy socks5 mtproto http connection download path storage cache auto-download export tray autostart spellcheck title updates debug",
        target: SettingsSection::Advanced,
        icon: SettingsIconKind::Advanced,
    },
    SettingsSearchEntry {
        id: "experimental",
        title: "Experimental settings",
        section_name: "Advanced",
        keywords: "experimental flags options beta peer id ipv6 webview devtools reset import export",
        target: SettingsSection::Experimental,
        icon: SettingsIconKind::Debug,
    },
    SettingsSearchEntry {
        id: "calls",
        title: strings::SEARCH_ENTRY_CALLS_TITLE,
        section_name: "Speakers and Camera",
        keywords: "calls audio speakers headphones microphone camera volume ducking push-to-talk noise",
        target: SettingsSection::Calls,
        icon: SettingsIconKind::Calls,
    },
    SettingsSearchEntry {
        id: "power",
        title: strings::SEARCH_ENTRY_POWER_TITLE,
        section_name: "Battery and Animations",
        keywords: "battery power saving performance animations stickers emoji gpu",
        target: SettingsSection::PowerSaving,
        icon: SettingsIconKind::Power,
    },
    SettingsSearchEntry {
        id: "language",
        title: strings::SEARCH_ENTRY_LANGUAGE_TITLE,
        section_name: "Language",
        keywords: "language locale translation translate",
        target: SettingsSection::Language,
        icon: SettingsIconKind::Language,
    },
];

/// Finds a [`SettingsSearchEntry`] by its stable `id`.
#[must_use]
pub fn find_search_entry(id: &str) -> Option<&'static SettingsSearchEntry> {
    SEARCH_INDEX.iter().find(|entry| entry.id == id)
}

/// Searches [`SEARCH_INDEX`] for `query` (case-insensitive multi-term match).
#[must_use]
pub fn search_settings(query: &str) -> Vec<&'static SettingsSearchEntry> {
    let trimmed = query.trim().to_lowercase();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let terms: Vec<&str> = trimmed.split_whitespace().collect();
    SEARCH_INDEX
        .iter()
        .filter(|entry| {
            let haystack = format!(
                "{} {} {}",
                entry.title.to_lowercase(),
                entry.section_name.to_lowercase(),
                entry.keywords.to_lowercase()
            );
            terms.iter().all(|t| haystack.contains(t))
        })
        .collect()
}

/// `InputField` id salt of the Main Settings search field.
const SEARCH_FIELD_SALT: &str = "settings_main_search";

/// Context-data key of the "settings search is open" flag toggled by the header search icon.
fn search_open_id() -> Id {
    Id::unique("tg_settings_main_search_open")
}

/// Returns whether the Main Settings search field is open (`settings_search.cpp`).
#[must_use]
pub fn settings_search_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(search_open_id()))
        .unwrap_or(false)
}

/// Context-data key of the "focus the search field when it is next shown" flag.
fn search_focus_id() -> Id {
    Id::unique("tg_settings_main_search_focus")
}

/// Opens or closes the Main Settings search field (header search icon). Opening focuses the
/// field like `Settings::Search::setInnerFocus`.
pub fn set_settings_search_open(ctx: &egui::Context, open: bool) {
    ctx.data_mut(|d| {
        d.insert_temp(search_open_id(), open);
        d.insert_temp(search_focus_id(), open);
    });
}

/// Output emitted by [`render_main_settings_page`].
#[derive(Debug, Clone, Default)]
pub struct MainPageOutput {
    /// Navigate to a specific settings sub-section.
    pub navigate: Option<SettingsSection>,
    /// Open the Payments / Stars / Gifts / Premium / Affiliate overlay on a specific screen.
    pub open_payments: Option<tg_app_core::core_types::window_types::PaymentsScreen>,
    /// Open the Telegram Business overlay.
    pub open_business: bool,
    /// Toast message to show (e.g. username/phone copied).
    pub toast: Option<String>,
    /// A new interface scale that needs tdesktop's "Restart now?" prompt before it is saved.
    pub scale_restart: Option<i32>,
}

/// Renders the Main Settings root page (`settings_main.cpp`).
#[allow(clippy::too_many_arguments)]
pub fn render_main_settings_page(
    ui: &mut Ui,
    search_query: &mut String,
    qr_open: &mut bool,
    global: &mut GlobalConfig,
    active_language_name: &str,
    backend: &dyn SettingsBackend,
    colors: &dyn Colors,
) -> MainPageOutput {
    let mut core = CoreSettings::new();
    let mut faq = FaqSuggestionsState::new();
    let mut recent = RecentSettingsSearches::new();
    render_main_settings_page_with_extras(
        ui,
        search_query,
        qr_open,
        global,
        &mut core,
        &mut faq,
        &mut recent,
        active_language_name,
        1,
        backend,
        colors,
    )
}

/// Renders the Main Settings root page with live FAQ suggestions (`settings_faq_suggestions.cpp`)
/// and persisted recent Settings searches (`settings_recent_searches.cpp`).
#[allow(clippy::too_many_arguments)]
pub fn render_main_settings_page_with_extras(
    ui: &mut Ui,
    search_query: &mut String,
    qr_open: &mut bool,
    global: &mut GlobalConfig,
    core: &mut CoreSettings,
    faq_suggestions: &mut FaqSuggestionsState,
    recent_searches: &mut RecentSettingsSearches,
    active_language_name: &str,
    device_pixel_ratio: i32,
    backend: &dyn SettingsBackend,
    colors: &dyn Colors,
) -> MainPageOutput {
    let mut out = MainPageOutput::default();
    let profile = backend.profile();
    let pad_x = settings_row_pad_x();
    ui.spacing_mut().item_spacing.y = 0.0;

    if recent_searches.is_empty() && !core.recent_settings_searches.is_empty() {
        *recent_searches = RecentSettingsSearches::from_core_settings(core);
    }

    // 1. Settings search field (`settings_search.cpp`), shown once the header search icon opens it.
    let search_open = settings_search_open(ui.ctx()) || !search_query.is_empty();
    if search_open {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            let avail = (ui.available_width() - pad_x).max(180.0);
            if ui
                .ctx()
                .data_mut(|d| d.remove_temp::<bool>(search_focus_id()))
                .unwrap_or(false)
            {
                let edit_id = ui.make_persistent_id(SEARCH_FIELD_SALT).with("_edit");
                ui.memory_mut(|m| m.request_focus(edit_id));
            }
            InputField::single_line(
                SEARCH_FIELD_SALT,
                search_query,
                strings::SEARCH_SETTINGS_PLACEHOLDER,
                InputFieldStyle::rounded_search(colors),
            )
            .width(avail)
            .show(ui);
        });
    }

    if !search_query.trim().is_empty() {
        faq_suggestions.ensure_loaded(backend);
        ui.add_space(6.0);
        let hits = search_settings(search_query);
        let faq_hits = faq_suggestions.search(search_query);

        if hits.is_empty() && faq_hits.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.label(
                    egui::RichText::new(strings::SEARCH_NO_MATCHES)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
        } else {
            if !hits.is_empty() {
                subsection_title(ui, strings::SEARCH_RESULTS_HEADING, colors);
                for hit in &hits {
                    let localized_section = hit.target.localized_title();
                    if settings_row(
                        ui,
                        hit.title,
                        Some(&localized_section),
                        None,
                        Some(hit.icon),
                        false,
                        true,
                        colors,
                    )
                    .clicked()
                    {
                        recent_searches.bump(hit.id);
                        recent_searches.sync_to_core_settings(core);
                        out.navigate = Some(hit.target);
                    }
                }
            }

            if !faq_hits.is_empty() {
                if !hits.is_empty() {
                    settings_divider(ui, None, colors);
                }
                let faq_heading = tr::lng_settings_faq();
                subsection_title(ui, &faq_heading, colors);
                let title_budget = (ui.available_width() - pad_x * 2.0 - 76.0).max(120.0);
                let max_chars = ((title_budget / 7.2) as usize).max(20);
                for faq in faq_hits.into_iter().take(6) {
                    let display_title = if faq.title.chars().count() > max_chars {
                        let truncated: String = faq
                            .title
                            .chars()
                            .take(max_chars.saturating_sub(1))
                            .collect();
                        format!("{}…", truncated.trim_end())
                    } else {
                        faq.title.clone()
                    };
                    let subtitle = format!("{} · {}", tr::lng_settings_faq_subtitle(), faq.section);
                    if settings_row(
                        ui,
                        &display_title,
                        Some(&subtitle),
                        None,
                        Some(SettingsIconKind::Help),
                        false,
                        true,
                        colors,
                    )
                    .clicked()
                    {
                        out.toast = Some(format!("FAQ: {}", faq.url));
                    }
                }
            }
        }
        ui.add_space(16.0);
        return out;
    }

    // Recent Settings searches strip (shown above Profile cover when non-empty).
    if !recent_searches.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new("Recent")
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(pad_x);
                let clear_resp = ui.add(
                    egui::Label::new(
                        egui::RichText::new("Clear")
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowActiveTextFg")),
                    )
                    .sense(Sense::click()),
                );
                if clear_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if clear_resp.clicked() {
                    recent_searches.clear();
                    recent_searches.sync_to_core_settings(core);
                }
            });
        });

        let recent_ids: Vec<String> = recent_searches.list().iter().take(5).cloned().collect();
        let mut to_remove: Option<String> = None;
        for id in recent_ids {
            let Some(entry) = find_search_entry(&id) else {
                continue;
            };
            ui.horizontal(|ui| {
                let row_w = (ui.available_width() - 36.0).max(160.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(row_w, 46.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let localized_section = entry.target.localized_title();
                        if settings_row(
                            ui,
                            entry.title,
                            Some(&localized_section),
                            None,
                            Some(entry.icon),
                            false,
                            false,
                            colors,
                        )
                        .clicked()
                        {
                            recent_searches.bump(entry.id);
                            recent_searches.sync_to_core_settings(core);
                            out.navigate = Some(entry.target);
                        }
                    },
                );
                let (x_rect, x_resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
                if x_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().text(
                    x_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "×",
                    font_id_from_style(st::semibold_font),
                    colors.color("windowSubTextFg"),
                );
                if x_resp.clicked() {
                    to_remove = Some(id.clone());
                }
            });
        }
        if let Some(rem) = to_remove {
            recent_searches.remove(&rem);
            recent_searches.sync_to_core_settings(core);
        }
        settings_divider(ui, None, colors);
    }

    // 2. Profile cover (`Cover` in `settings_main.cpp`): 72 px photo at `settingsPhotoLeft/Top`,
    // name / phone / username at `settingsNameLeft` and `settingsName/Phone/UsernameTop`.
    let photo_top = st::settings_photo_top.scaled().max(8) as f32;
    let photo_left = st::settings_photo_left.scaled().max(pad_x as i32) as f32;
    let photo_size = 72.0_f32;
    let cover_h = photo_top + photo_size + st::settings_photo_bottom.scaled().max(16) as f32;
    let name_left = 112.0_f32;
    let (cover_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), cover_h), Sense::hover());
    {
        let av_rect = Rect::from_min_size(
            Pos2::new(cover_rect.left() + photo_left, cover_rect.top() + photo_top),
            Vec2::splat(photo_size),
        );
        let av_resp = ui.interact(av_rect, ui.scope_id().with("cover_photo"), Sense::click());
        let userpic =
            EmptyUserpic::for_color_index(profile.photo_color_index, &profile.full_name(), colors);
        // A user's full peer id is its bare id (`peerFromUser`).
        userpic.paint_circle_or_photo(ui.painter(), av_rect, profile.user_id as i64);
        if av_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if av_resp.clicked() {
            out.navigate = Some(SettingsSection::Information);
        }

        // Right-aligned QR code button (`st::info_profile_labeled_button_qr`).
        let qr_btn_size = 36.0_f32;
        let qr_rect = Rect::from_center_size(
            Pos2::new(
                cover_rect.right() - pad_x - qr_btn_size * 0.5 + 8.0,
                av_rect.center().y,
            ),
            Vec2::splat(qr_btn_size),
        );
        let qr_resp = ui.interact(qr_rect, ui.scope_id().with("cover_qr"), Sense::click());
        if qr_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter().circle_filled(
                qr_rect.center(),
                qr_btn_size * 0.5,
                colors.color("windowBgOver"),
            );
        }
        paint_tinted_icon(
            ui.painter(),
            qr_rect,
            st::menu_icon_qr_code,
            colors,
            Some(colors.color("windowActiveTextFg")),
        );
        if qr_resp.on_hover_text(strings::PROFILE_QR_TOOLTIP).clicked() {
            *qr_open = !*qr_open;
        }

        let text_left = cover_rect.left() + name_left;
        let text_w = (qr_rect.left() - 8.0 - text_left).max(40.0);
        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
            Pos2::new(text_left, cover_rect.top()),
            Pos2::new(text_left + text_w, cover_rect.bottom()),
        ));
        painter.text(
            Pos2::new(text_left, cover_rect.top() + 12.0),
            Align2::LEFT_TOP,
            profile.full_name(),
            egui::FontId::new(16.0, tg_ui::fonts::family_semibold()),
            resolve_color_ref(st::settings_cover_name.text_fg, colors),
        );

        let phone_text = if profile.phone_hidden {
            strings::PHONE_NUMBER_HIDDEN.to_owned()
        } else {
            format_cover_phone(&profile.phone)
        };
        let small = font_id_from_style(st::normal_font);
        let phone_rect = painter.text(
            Pos2::new(text_left, cover_rect.top() + 37.0),
            Align2::LEFT_TOP,
            &phone_text,
            small.clone(),
            colors.color("windowFg"),
        );
        let phone_resp = ui.interact(
            phone_rect,
            ui.scope_id().with("cover_phone"),
            Sense::click(),
        );
        if phone_resp.clicked() && !profile.phone_hidden {
            ui.ctx().copy_text(phone_text.clone());
            out.toast = Some(strings::PHONE_COPIED_TOAST.to_owned());
        }

        let uname_label = if profile.username.is_empty() {
            tr::lng_settings_username_add()
        } else {
            format!("@{}", profile.username)
        };
        let uname_rect = painter.text(
            Pos2::new(text_left, cover_rect.top() + 58.0),
            Align2::LEFT_TOP,
            uname_label,
            small,
            colors.color("windowSubTextFg"),
        );
        let uname_resp = ui.interact(
            uname_rect,
            ui.scope_id().with("cover_username"),
            Sense::click(),
        );
        if uname_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if uname_resp.clicked() {
            if profile.username.is_empty() {
                out.navigate = Some(SettingsSection::Information);
            } else {
                let link = format!("https://t.me/{}", profile.username);
                ui.ctx().copy_text(link);
                out.toast = Some(tr::lng_username_copied());
            }
        }
    }

    if *qr_open {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            ui.horizontal(|ui| {
                let (box_rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
                ui.painter().rect_filled(
                    box_rect,
                    CornerRadius::same(st::settings_icon_radius.scaled().clamp(2, 16) as u8),
                    Color32::WHITE,
                );
                paint_tinted_icon(
                    ui.painter(),
                    box_rect.shrink(6.0),
                    st::menu_icon_qr_code,
                    colors,
                    Some(Color32::BLACK),
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(format!("@{}", profile.username))
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    ui.label(
                        egui::RichText::new(strings::PROFILE_QR_SCAN_HINT)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(4.0);
                    if RoundButton::new(&tr::lng_close(), RoundButtonStyle::default_light(colors))
                        .show(ui)
                        .clicked()
                    {
                        *qr_open = false;
                    }
                });
            });
        });
    }

    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 3. Main Settings sections (`BuildSectionButtons` in `settings_main.cpp`).
    let sections: [(String, SettingsIconKind, SettingsSection, Option<&str>); 9] = [
        (
            tr::lng_settings_my_account(),
            SettingsIconKind::Account,
            SettingsSection::Information,
            None,
        ),
        (
            tr::lng_settings_section_notify(),
            SettingsIconKind::Notifications,
            SettingsSection::Notifications,
            None,
        ),
        (
            tr::lng_settings_section_privacy(),
            SettingsIconKind::Privacy,
            SettingsSection::PrivacySecurity,
            None,
        ),
        (
            tr::lng_settings_section_chat_settings(),
            SettingsIconKind::Chat,
            SettingsSection::Chat,
            None,
        ),
        (
            strings::CHAT_FOLDERS_TITLE.to_owned(),
            SettingsIconKind::Folders,
            SettingsSection::Folders,
            None,
        ),
        (
            tr::lng_settings_advanced(),
            SettingsIconKind::Advanced,
            SettingsSection::Advanced,
            None,
        ),
        (
            tr::lng_settings_section_devices(),
            SettingsIconKind::Mic,
            SettingsSection::Calls,
            None,
        ),
        (
            tr::lng_settings_power_menu(),
            SettingsIconKind::Power,
            SettingsSection::PowerSaving,
            None,
        ),
        (
            tr::lng_settings_language(),
            SettingsIconKind::Language,
            SettingsSection::Language,
            Some(active_language_name),
        ),
    ];

    for (title, icon, target, right_label) in &sections {
        if settings_row(
            ui,
            title,
            None,
            *right_label,
            Some(*icon),
            false,
            false,
            colors,
        )
        .clicked()
        {
            out.navigate = Some(*target);
        }
    }

    section_break(ui, colors);

    // 4. Default Interface Scale (`BuildInterfaceScale` in `settings_main.cpp`).
    if let ScaleChange::NeedsRestart(scale) =
        render_scale_controls(ui, global, device_pixel_ratio, false, colors)
    {
        out.scale_restart = Some(scale);
    }

    section_break(ui, colors);

    // 5. Premium section (`BuildPremiumSection`): Premium, Stars, TON, Business, Send a Gift.
    if settings_row(
        ui,
        &tr::lng_premium_summary_title(),
        None,
        None,
        Some(SettingsIconKind::Premium),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_payments = Some(tg_app_core::core_types::window_types::PaymentsScreen::Premium);
    }
    if settings_row(
        ui,
        &tr::lng_settings_credits(),
        None,
        None,
        Some(SettingsIconKind::Stars),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_payments = Some(tg_app_core::core_types::window_types::PaymentsScreen::Stars);
    }

    // Built-in TON Wallet entry (tdesktop's `main/currency` row, gated by `WalletAppConfig`).
    crate::wallet::AppWalletHost::render_settings_entry(ui, colors);

    if settings_row(
        ui,
        &tr::lng_business_title(),
        None,
        None,
        Some(SettingsIconKind::Business),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_business = true;
    }
    if settings_row(
        ui,
        strings::SEND_A_GIFT_TITLE,
        None,
        None,
        Some(SettingsIconKind::Gift),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_payments = Some(tg_app_core::core_types::window_types::PaymentsScreen::StarGifts);
    }

    section_break(ui, colors);

    // 6. Help section (`BuildHelpSection`): FAQ (with the keys legend button), Features, Ask.
    let faq_label = tr::lng_settings_faq();
    let faq_resp = settings_row(
        ui,
        &faq_label,
        None,
        None,
        Some(SettingsIconKind::Help),
        false,
        false,
        colors,
    );
    let legend_rect = Rect::from_center_size(
        Pos2::new(
            faq_resp.rect.right() - pad_x - 8.0,
            faq_resp.rect.center().y,
        ),
        Vec2::splat(28.0),
    );
    let legend_resp = ui.interact(
        legend_rect,
        ui.scope_id().with("faq_keys_legend"),
        Sense::click(),
    );
    paint_tinted_icon(
        ui.painter(),
        legend_rect,
        st::settings_faq_keys_legend_icon,
        colors,
        None,
    );
    if legend_resp
        .on_hover_text(tr::lng_keys_legend_title())
        .clicked()
    {
        out.toast = Some(tr::lng_keys_legend_title());
    } else if faq_resp.clicked() {
        out.toast = Some(strings::FAQ_URL_TOAST.to_owned());
    }
    if settings_row(
        ui,
        &tr::lng_settings_features(),
        None,
        None,
        Some(SettingsIconKind::Features),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.toast = Some(strings::FEATURES_URL_TOAST.to_owned());
    }
    if settings_row(
        ui,
        &tr::lng_settings_ask_question(),
        None,
        None,
        Some(SettingsIconKind::Discussion),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.toast = Some(tr::lng_settings_ask_sure());
    }
    ui.add_space(SETTINGS_LIST_SKIP);

    out
}

/// `addSkip()` + `addDivider()` + `addSkip()` between two `settings_main.cpp` sections.
fn section_break(ui: &mut Ui, colors: &dyn Colors) {
    ui.add_space(SETTINGS_LIST_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);
}

/// Formats the cover phone in international format (`Ui::FormatPhone`, e.g. `+999 66 1 2345`).
fn format_cover_phone(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains(' ') {
        return trimmed.to_owned();
    }
    let formatted = tg_lang::countries::format_phone(trimmed);
    if formatted.starts_with('+') {
        formatted
    } else {
        format!("+{formatted}")
    }
}
