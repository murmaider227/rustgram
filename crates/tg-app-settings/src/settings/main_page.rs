//! Main Settings landing page: profile cover, live settings search, colored category rows,
//! Default Interface Scale slider, Telegram Premium placeholder row, and version footer.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_main.cpp`,
//! `Telegram/SourceFiles/settings/settings_search.cpp`, and
//! `Telegram/SourceFiles/settings/sections/settings_premium.cpp` (placeholder row only).

use egui::{Color32, CornerRadius, CursorIcon, Sense, Ui, Vec2};
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
use super::scale::render_scale_controls;
use super::strings;
use super::widgets::{
    SettingsIconKind, paint_tinted_icon, settings_card, settings_divider, settings_row,
    settings_row_pad_x, subsection_title,
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
    backend: &dyn SettingsBackend,
    colors: &dyn Colors,
) -> MainPageOutput {
    let mut out = MainPageOutput::default();
    let profile = backend.profile();
    let pad_x = settings_row_pad_x();

    if recent_searches.is_empty() && !core.recent_settings_searches.is_empty() {
        *recent_searches = RecentSettingsSearches::from_core_settings(core);
    }

    // 1. Live Settings Search field (`settings_search.cpp`).
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(180.0);
        InputField::single_line(
            "settings_main_search",
            search_query,
            strings::SEARCH_SETTINGS_PLACEHOLDER,
            InputFieldStyle::rounded_search(colors),
        )
        .width(avail)
        .show(ui);
    });

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

    // 2. Profile Cover (`Cover` in `settings_main.cpp` using `st::settings_photo_*` & `st::settings_cover_*`).
    let photo_top = st::settings_photo_top.scaled().max(12) as f32;
    let photo_left = st::settings_photo_left.scaled().max(pad_x as i32) as f32;
    let photo_size = st::info_profile_cover.photo.size.width.scaled().max(68) as f32;
    ui.add_space(photo_top);
    ui.horizontal(|ui| {
        ui.add_space(photo_left);
        let (av_rect, av_resp) = ui.allocate_exact_size(Vec2::splat(photo_size), Sense::click());
        let userpic =
            EmptyUserpic::for_color_index(profile.photo_color_index, &profile.full_name(), colors);
        userpic.paint_circle(ui.painter(), av_rect);
        if av_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if av_resp.clicked() {
            out.navigate = Some(SettingsSection::Information);
        }

        let name_gap = (st::settings_name_left.scaled() as f32 - photo_left - photo_size).max(14.0);
        ui.add_space(name_gap);
        ui.vertical(|ui| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(profile.full_name())
                    .font(font_id_from_style(st::settings_cover_name.style.font))
                    .color(resolve_color_ref(st::settings_cover_name.text_fg, colors)),
            );
            let phone_text = if profile.phone_hidden {
                strings::PHONE_NUMBER_HIDDEN.to_owned()
            } else {
                profile.phone.clone()
            };
            let phone_resp = ui.add(
                egui::Label::new(
                    egui::RichText::new(phone_text)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                )
                .sense(Sense::click()),
            );
            if phone_resp.clicked() && !profile.phone_hidden {
                ui.ctx().copy_text(profile.phone.clone());
                out.toast = Some(strings::PHONE_COPIED_TOAST.to_owned());
            }

            let uname_label = if profile.username.is_empty() {
                tr::lng_settings_username_add()
            } else {
                format!("@{}", profile.username)
            };
            let uname_resp = ui.add(
                egui::Label::new(
                    egui::RichText::new(uname_label)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowActiveTextFg")),
                )
                .sense(Sense::click()),
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
        });

        // Right-aligned QR code button (`st::menu_icon_qr_code` / `st::info_profile_labeled_button_qr`).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(pad_x);
            let qr_btn_size = st::info_profile_labeled_button_qr.width.scaled().max(36) as f32;
            let (qr_rect, qr_resp) =
                ui.allocate_exact_size(Vec2::splat(qr_btn_size), Sense::click());
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
        });
    });
    ui.add_space(st::settings_photo_bottom.scaled().max(6) as f32);

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

    settings_divider(ui, None, colors);

    // 3. Main Settings Categories (`SetupSections` in `settings_main.cpp`).
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
            tr::lng_settings_section_filters(),
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
            tr::lng_settings_section_call_settings(),
            SettingsIconKind::Calls,
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

    settings_divider(ui, None, colors);

    // Built-in TON Wallet entry (gated by `WalletAppConfig::is_available`).
    crate::wallet::AppWalletHost::render_settings_entry(ui, colors);

    // 4. Default Interface Scale (`SetupDefaultScale` in `settings_main.cpp`).
    render_scale_controls(ui, global, false, colors);

    settings_divider(ui, None, colors);

    // 5. Telegram Premium, Stars, Gifts, Affiliate, and Business entry points (`settings_main.cpp`).
    if settings_row(
        ui,
        strings::PREMIUM_TITLE,
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
        "My Stars",
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
    if settings_row(
        ui,
        "Send a Gift",
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
    if settings_row(
        ui,
        "Stars Affiliate",
        None,
        None,
        Some(SettingsIconKind::Affiliate),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_payments = Some(tg_app_core::core_types::window_types::PaymentsScreen::Affiliate);
    }
    if settings_row(
        ui,
        "Telegram Business",
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

    settings_divider(ui, None, colors);

    // 6. Help / FAQ & Version footer (`SetupHelp` in `settings_main.cpp`).
    let faq_label = tr::lng_settings_faq();
    if settings_row(
        ui,
        &faq_label,
        None,
        None,
        Some(SettingsIconKind::Help),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.toast = Some(strings::FAQ_URL_TOAST.to_owned());
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let footer = strings::MAIN_FOOTER_TEMPLATE.replace("{version}", tg_core::APP_VERSION_STR);
        ui.label(
            egui::RichText::new(footer)
                .font(egui::FontId::new(11.5, tg_ui::fonts::family_regular()))
                .color(colors.color("windowSubTextFg")),
        );
    });
    ui.add_space(18.0);

    out
}
