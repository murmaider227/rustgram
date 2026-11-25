//! Privacy and Security settings section, `EditPrivacyBox`, Blocked Users, and Account TTL.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_privacy_security.cpp`,
//! `Telegram/SourceFiles/boxes/edit_privacy_box.cpp`,
//! `Telegram/SourceFiles/settings/sections/settings_blocked_peers.cpp`,
//! `Telegram/SourceFiles/settings/sections/settings_global_ttl.cpp`, and
//! `Telegram/SourceFiles/boxes/self_destruction_box.cpp`.

use egui::{CornerRadius, Stroke, Ui};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;

use super::backend::{
    ACCOUNT_TTL_OPTIONS, GLOBAL_TTL_OPTIONS_SECS, PrivacyKey, PrivacyOption, PrivacyRule,
    SettingsBackend,
};
use super::strings;
use super::widgets::{
    SettingsIconKind, settings_card, settings_checkbox_row, settings_divider, settings_radio_row,
    settings_row, settings_toggle_row, subsection_title,
};

/// Active sub-editor box inside Privacy & Security.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PrivacyEditor {
    /// No sub-editor open.
    #[default]
    None,
    /// Editing a specific [`PrivacyKey`] (`EditPrivacyBox`).
    EditRule {
        key: PrivacyKey,
        draft: PrivacyRule,
        editing_exceptions: Option<ExceptionKind>,
    },
    /// Viewing and managing blocked users.
    BlockedUsers,
    /// Selecting account self-destruction TTL (`SelfDestructionBox`).
    AccountTtl { selected_days: i32 },
    /// Selecting global auto-delete timer (`settings_global_ttl.cpp`).
    GlobalTtl { selected_secs: i32 },
    /// Clearing saved payment & shipping info (`ClearPaymentInfoBox`).
    ClearPaymentInfo {
        clear_shipping: bool,
        clear_credentials: bool,
    },
}

/// Which exception list is currently being edited inside [`PrivacyEditor::EditRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionKind {
    /// Users always allowed regardless of base option.
    AlwaysAllow,
    /// Users never allowed regardless of base option.
    NeverAllow,
}

/// Persistent UI state for the Privacy and Security section.
#[derive(Debug, Clone)]
pub struct PrivacyState {
    /// Active inline modal/card editor.
    pub editor: PrivacyEditor,
    /// Current state of `"Suggest frequent contacts"` (`contacts.toggleTopPeers`).
    pub top_peers_enabled: bool,
}

impl Default for PrivacyState {
    fn default() -> Self {
        Self {
            editor: PrivacyEditor::None,
            top_peers_enabled: true,
        }
    }
}

/// Navigation/event outputs from [`render_privacy_section`].
#[derive(Debug, Clone, Default)]
pub struct PrivacyOutput {
    /// Open Local Passcode settings section.
    pub open_local_passcode: bool,
    /// Open Two-Step Verification (Cloud Password) section.
    pub open_cloud_password: bool,
    /// Open Passkeys settings section (`Settings::Passkeys`).
    pub open_passkeys: bool,
    /// Open Active Sessions section.
    pub open_active_sessions: bool,
    /// Open Connected Websites section (`settings_websites.cpp`).
    pub open_connected_websites: bool,
    /// Emitted when the user toggles `"Suggest frequent contacts"` (`contacts.toggleTopPeers`).
    pub toggled_top_peers: Option<bool>,
    /// Optional toast feedback.
    pub toast: Option<String>,
}

/// Formats an account/session TTL in days into a localized tdesktop label
/// (e.g. `"6 months"`, `"1 year"`).
#[must_use]
pub fn format_ttl_days(days: i32) -> String {
    match days {
        7 => tr::lng_weeks(1),
        30 => tr::lng_months(1),
        90 => tr::lng_months(3),
        180 => tr::lng_months(6),
        365 => tr::lng_years(1),
        548 => tr::lng_months(18),
        720 => tr::lng_years(2),
        other => tr::lng_days(i64::from(other)),
    }
}

/// Formats global auto-delete message TTL seconds into a localized label.
#[must_use]
pub fn format_global_ttl(secs: i32) -> String {
    match secs {
        0 => tr::lng_settings_cloud_password_off(),
        86_400 => tr::lng_days(1),
        604_800 => tr::lng_weeks(1),
        2_592_000 => tr::lng_months(1),
        other => tr::lng_days(i64::from(other / 86_400)),
    }
}

/// Skip above the Security / Privacy titles (`st::settings_privacy_skip`).
const SECURITY_TOP_SKIP: f32 = 4.0;
/// Skip before a section divider (`Builder::addSkip`).
const SECTION_SKIP: f32 = 8.0;

/// One row of the Privacy list (`BuildPrivacySection`).
#[derive(Debug, Clone, Copy)]
enum PrivacyRow {
    /// A privacy key row that opens `EditPrivacyBox`.
    Key(PrivacyKey),
    /// "Messages" (`EditMessagesPrivacyBox`, backed by the global privacy settings).
    Messages,
}

/// Privacy rows in Telegram Desktop 7.4.6 order.
const PRIVACY_ROWS: [PrivacyRow; 12] = [
    PrivacyRow::Key(PrivacyKey::PhoneNumber),
    PrivacyRow::Key(PrivacyKey::LastSeen),
    PrivacyRow::Key(PrivacyKey::ProfilePhoto),
    PrivacyRow::Key(PrivacyKey::Forwards),
    PrivacyRow::Key(PrivacyKey::Calls),
    PrivacyRow::Key(PrivacyKey::VoiceMessages),
    PrivacyRow::Messages,
    PrivacyRow::Key(PrivacyKey::Birthday),
    PrivacyRow::Key(PrivacyKey::GiftsAutoSave),
    PrivacyRow::Key(PrivacyKey::About),
    PrivacyRow::Key(PrivacyKey::SavedMusic),
    PrivacyRow::Key(PrivacyKey::GroupsInvite),
];

/// Row title on the Privacy and Security page (`lng_settings_*_privacy`, sentence case).
fn privacy_row_title(key: PrivacyKey) -> String {
    match key {
        PrivacyKey::PhoneNumber => tr::lng_settings_phone_number_privacy(),
        PrivacyKey::LastSeen => tr::lng_settings_last_seen(),
        PrivacyKey::ProfilePhoto => tr::lng_settings_profile_photo_privacy(),
        PrivacyKey::Forwards => tr::lng_settings_forwards_privacy(),
        PrivacyKey::Calls => tr::lng_settings_calls(),
        PrivacyKey::VoiceMessages => tr::lng_settings_voices_privacy(),
        PrivacyKey::Birthday => strings::BIRTHDAY_PRIVACY_TITLE.to_owned(),
        PrivacyKey::GiftsAutoSave => tr::lng_settings_gifts_privacy(),
        PrivacyKey::About => tr::lng_settings_bio_privacy(),
        PrivacyKey::SavedMusic => tr::lng_settings_saved_music_privacy(),
        PrivacyKey::GroupsInvite => tr::lng_settings_groups_invite(),
        other => other.localized_title(),
    }
}

/// Row value label: localized option ("Everybody", "My contacts", "Nobody") plus the
/// exception counts tdesktop appends (`"Everybody (-2, +1)"`).
fn privacy_row_summary(rule: &PrivacyRule, key: PrivacyKey) -> String {
    let base = rule.option.localized_label_for(key);
    let mut parts = Vec::new();
    if !rule.never_allow.is_empty() {
        parts.push(format!("-{}", rule.never_allow.len()));
    }
    if !rule.always_allow.is_empty() {
        parts.push(format!("+{}", rule.always_allow.len()));
    }
    if parts.is_empty() {
        base
    } else {
        format!("{base} ({})", parts.join(", "))
    }
}

/// Renders the Privacy and Security settings section (`settings_privacy_security.cpp`).
pub fn render_privacy_section(
    ui: &mut Ui,
    state: &mut PrivacyState,
    has_local_passcode: bool,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> PrivacyOutput {
    let mut out = PrivacyOutput::default();
    ui.spacing_mut().item_spacing.y = 0.0;

    // 1. Security (`BuildSecuritySection` in `settings_privacy_security.cpp`): Two-Step
    // Verification, Auto-Delete Messages, Local passcode, Passkeys, Blocked users, Connected
    // websites (only when there are any), Active sessions, then the sessions footer.
    ui.add_space(SECURITY_TOP_SKIP);
    subsection_title(ui, &tr::lng_settings_security(), colors);

    let cloud_state = backend.cloud_password_state();
    let cloud_label = if cloud_state.has_password {
        tr::lng_settings_cloud_password_on()
    } else {
        tr::lng_settings_cloud_password_off()
    };
    let cloud_title = tr::lng_settings_cloud_password_start_title();
    if settings_row(
        ui,
        &cloud_title,
        None,
        Some(&cloud_label),
        Some(SettingsIconKind::TwoStep),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_cloud_password = true;
    }

    let mut global_priv = backend.global_privacy();
    let ttl_label = format_global_ttl(global_priv.global_ttl_seconds);
    if settings_row(
        ui,
        &tr::lng_settings_ttl_title(),
        None,
        Some(&ttl_label),
        Some(SettingsIconKind::Timer),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editor = PrivacyEditor::GlobalTtl {
            selected_secs: global_priv.global_ttl_seconds,
        };
    }

    let passcode_label = if has_local_passcode {
        tr::lng_settings_cloud_password_on()
    } else {
        tr::lng_settings_cloud_password_off()
    };
    let passcode_title = tr::lng_settings_passcode_title();
    if settings_row(
        ui,
        &passcode_title,
        None,
        Some(&passcode_label),
        Some(SettingsIconKind::Lock),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_local_passcode = true;
    }

    let passkeys_count = backend.passkeys().len();
    let passkeys_label = if passkeys_count == 0 {
        tr::lng_settings_cloud_password_off()
    } else {
        passkeys_count.to_string()
    };
    let passkeys_title = tr::lng_settings_passkeys_title();
    if settings_row(
        ui,
        &passkeys_title,
        None,
        Some(&passkeys_label),
        Some(SettingsIconKind::Passkeys),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_passkeys = true;
    }

    let blocked_count = backend.blocked_users().len();
    let blocked_label = if blocked_count == 0 {
        tr::lng_settings_no_blocked_users()
    } else {
        blocked_count.to_string()
    };
    let blocked_title = tr::lng_settings_blocked_users();
    if settings_row(
        ui,
        &blocked_title,
        None,
        Some(&blocked_label),
        Some(SettingsIconKind::Blocked),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editor = PrivacyEditor::BlockedUsers;
    }

    let websites_count = backend.web_authorizations().len();
    if websites_count > 0 {
        let websites_title = tr::lng_settings_logged_in();
        if settings_row(
            ui,
            &websites_title,
            None,
            Some(&websites_count.to_string()),
            Some(SettingsIconKind::IpAddress),
            false,
            false,
            colors,
        )
        .clicked()
        {
            out.open_connected_websites = true;
        }
    }

    let sessions_count = backend.active_sessions().len();
    let sessions_label = if sessions_count == 0 {
        String::new()
    } else {
        sessions_count.to_string()
    };
    if settings_row(
        ui,
        &tr::lng_settings_show_sessions(),
        None,
        Some(&sessions_label),
        Some(SettingsIconKind::Sessions),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_active_sessions = true;
    }

    // Render active editor immediately under Security if it's a Security editor.
    if matches!(
        state.editor,
        PrivacyEditor::BlockedUsers | PrivacyEditor::GlobalTtl { .. }
    ) {
        ui.add_space(6.0);
        render_privacy_editor(ui, state, backend, colors, &mut out);
    }

    ui.add_space(SECTION_SKIP);
    settings_divider(ui, Some(&tr::lng_settings_sessions_about()), colors);

    // 2. Privacy (`BuildPrivacySection`): rows without icons, value label on the right.
    ui.add_space(SECURITY_TOP_SKIP);
    subsection_title(ui, &tr::lng_settings_privacy_title(), colors);
    let rules = backend.privacy_rules();
    let messages_label = if global_priv.noncontact_peers_paid_stars.unwrap_or(0) > 0 {
        tr::lng_edit_privacy_paid()
    } else if global_priv.new_noncontact_peers_require_premium {
        tr::lng_edit_privacy_contacts_and_premium()
    } else {
        tr::lng_edit_privacy_everyone()
    };
    for row in PRIVACY_ROWS {
        let (key_title, summary, key) = match row {
            PrivacyRow::Key(key) => {
                let rule = rules.get(&key).cloned().unwrap_or_default();
                (
                    privacy_row_title(key),
                    privacy_row_summary(&rule, key),
                    Some(key),
                )
            }
            PrivacyRow::Messages => (
                tr::lng_settings_messages_privacy(),
                messages_label.clone(),
                None,
            ),
        };
        let clicked = settings_row(
            ui,
            &key_title,
            None,
            Some(&summary),
            None,
            false,
            false,
            colors,
        )
        .clicked();
        if let (true, Some(key)) = (clicked, key) {
            state.editor = PrivacyEditor::EditRule {
                key,
                draft: rules.get(&key).cloned().unwrap_or_default(),
                editing_exceptions: None,
            };
        }
    }

    if matches!(state.editor, PrivacyEditor::EditRule { .. }) {
        ui.add_space(6.0);
        render_privacy_editor(ui, state, backend, colors, &mut out);
    }

    ui.add_space(SECTION_SKIP);
    settings_divider(ui, None, colors);

    // 3. New Chats / Archive & Sensitive Content.
    subsection_title(ui, strings::NEW_CHATS_UNKNOWN_TITLE, colors);
    let mut global_changed = false;
    if settings_toggle_row(
        ui,
        strings::ARCHIVE_AND_MUTE_TITLE,
        Some(strings::ARCHIVE_AND_MUTE_SUBTITLE),
        None,
        &mut global_priv.archive_and_mute_new,
        true,
        colors,
    ) {
        global_changed = true;
    }
    if settings_toggle_row(
        ui,
        strings::HIDE_READ_TIME_TITLE,
        Some(strings::HIDE_READ_TIME_SUBTITLE),
        None,
        &mut global_priv.hide_read_time,
        true,
        colors,
    ) {
        global_changed = true;
    }
    if settings_toggle_row(
        ui,
        "Require Premium for new non-contact chats",
        Some("Only contacts and Telegram Premium users can start new private chats with you."),
        None,
        &mut global_priv.new_noncontact_peers_require_premium,
        true,
        colors,
    ) {
        global_changed = true;
    }
    let mut paid_enabled = global_priv.noncontact_peers_paid_stars.unwrap_or(0) > 0;
    let paid_subtitle = match global_priv.noncontact_peers_paid_stars {
        Some(stars) if stars > 0 => {
            format!("Charging {stars} Stars per message from non-contacts.")
        }
        _ => "Require Telegram Stars for messages from users who are not your contacts.".to_owned(),
    };
    if settings_toggle_row(
        ui,
        "Charge Stars for messages from non-contacts",
        Some(&paid_subtitle),
        None,
        &mut paid_enabled,
        true,
        colors,
    ) {
        global_priv.noncontact_peers_paid_stars = if paid_enabled { Some(10) } else { Some(0) };
        global_changed = true;
    }

    settings_divider(ui, None, colors);

    subsection_title(ui, &tr::lng_settings_sensitive_title(), colors);
    let sensitive_lbl = tr::lng_settings_sensitive_disable_filtering();
    let sensitive_sub = tr::lng_settings_sensitive_about();
    if settings_toggle_row(
        ui,
        &sensitive_lbl,
        Some(&sensitive_sub),
        None,
        &mut global_priv.disable_sensitive_content_filtering,
        global_priv.sensitive_can_change,
        colors,
    ) {
        global_changed = true;
    }

    if global_changed {
        backend.set_global_privacy(global_priv);
    }

    settings_divider(ui, None, colors);

    // 3b. Frequent contacts (`BuildTopPeersSection` in `settings_privacy_security.cpp:1029-1064`).
    subsection_title(ui, &tr::lng_settings_top_peers_title(), colors);
    let top_peers_lbl = tr::lng_settings_top_peers_suggest();
    let top_peers_about = tr::lng_settings_top_peers_about();
    if settings_toggle_row(
        ui,
        &top_peers_lbl,
        None,
        None,
        &mut state.top_peers_enabled,
        true,
        colors,
    ) {
        out.toggled_top_peers = Some(state.top_peers_enabled);
    }

    settings_divider(ui, Some(&top_peers_about), colors);

    // 4. Delete my account (Self-Destruction TTL) & Payments.
    subsection_title(ui, &tr::lng_self_destruct_title(), colors);
    let account_ttl = backend.account_ttl_days();
    let account_ttl_str = format_ttl_days(account_ttl);
    let self_destruct_desc = tr::lng_self_destruct_description();
    if settings_row(
        ui,
        strings::IF_AWAY_FOR_TITLE,
        Some(&self_destruct_desc),
        Some(&account_ttl_str),
        None,
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editor = PrivacyEditor::AccountTtl {
            selected_days: account_ttl,
        };
    }

    let clear_payment_lbl = tr::lng_settings_clear_payment_info();
    if settings_row(
        ui,
        &clear_payment_lbl,
        None,
        None,
        None,
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editor = PrivacyEditor::ClearPaymentInfo {
            clear_shipping: true,
            clear_credentials: true,
        };
    }

    if matches!(
        state.editor,
        PrivacyEditor::AccountTtl { .. } | PrivacyEditor::ClearPaymentInfo { .. }
    ) {
        ui.add_space(6.0);
        render_privacy_editor(ui, state, backend, colors, &mut out);
    }

    ui.add_space(16.0);
    out
}

fn render_privacy_editor(
    ui: &mut Ui,
    state: &mut PrivacyState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut PrivacyOutput,
) {
    let mut close = false;
    let contacts = backend.contacts();

    settings_card(ui, colors, |ui| match &mut state.editor {
        PrivacyEditor::None => {}
        PrivacyEditor::EditRule {
            key,
            draft,
            editing_exceptions,
        } => {
            ui.label(
                egui::RichText::new(key.localized_title())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(key.localized_question())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.add_space(4.0);

            // Forwarded Messages live preview card matching `ForwardsPrivacyController::setupAboveWidget`.
            if *key == PrivacyKey::Forwards {
                let sender_line = match draft.option {
                    PrivacyOption::Everybody => strings::FORWARDS_PREVIEW_EVERYBODY,
                    PrivacyOption::Contacts => strings::FORWARDS_PREVIEW_CONTACTS,
                    PrivacyOption::Nobody => strings::FORWARDS_PREVIEW_NOBODY,
                };
                egui::Frame::NONE
                    .fill(colors.color("msgOutBg"))
                    .stroke(Stroke::new(1.0, colors.color("shadowFg")))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(sender_line)
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowActiveTextFg")),
                        );
                        ui.label(
                            egui::RichText::new(strings::FORWARDS_PREVIEW_SAMPLE)
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                    });
                ui.add_space(6.0);
            }

            settings_radio_row(
                ui,
                &mut draft.option,
                PrivacyOption::Everybody,
                &PrivacyOption::Everybody.localized_label_for(*key),
                true,
                colors,
            );
            settings_radio_row(
                ui,
                &mut draft.option,
                PrivacyOption::Contacts,
                &PrivacyOption::Contacts.localized_label_for(*key),
                true,
                colors,
            );
            if key.allows_nobody() {
                settings_radio_row(
                    ui,
                    &mut draft.option,
                    PrivacyOption::Nobody,
                    &PrivacyOption::Nobody.localized_label_for(*key),
                    true,
                    colors,
                );
            }

            // Exceptions rows (`Always Allow` / `Never Allow`).
            if key.supports_exceptions() {
                ui.add_space(6.0);
                if draft.option != PrivacyOption::Everybody {
                    let count_str = if draft.always_allow.is_empty() {
                        tr::lng_edit_privacy_exceptions_add()
                    } else {
                        tr::lng_edit_privacy_exceptions_count(draft.always_allow.len() as i64)
                    };
                    let always_lbl = tr::lng_edit_privacy_groups_always_empty();
                    if settings_row(
                        ui,
                        &always_lbl,
                        None,
                        Some(&count_str),
                        None,
                        false,
                        true,
                        colors,
                    )
                    .clicked()
                    {
                        *editing_exceptions =
                            if *editing_exceptions == Some(ExceptionKind::AlwaysAllow) {
                                None
                            } else {
                                Some(ExceptionKind::AlwaysAllow)
                            };
                    }
                }
                if draft.option != PrivacyOption::Nobody {
                    let count_str = if draft.never_allow.is_empty() {
                        tr::lng_edit_privacy_exceptions_add()
                    } else {
                        tr::lng_edit_privacy_exceptions_count(draft.never_allow.len() as i64)
                    };
                    let never_lbl = tr::lng_edit_privacy_groups_never_empty();
                    if settings_row(
                        ui,
                        &never_lbl,
                        None,
                        Some(&count_str),
                        None,
                        false,
                        true,
                        colors,
                    )
                    .clicked()
                    {
                        *editing_exceptions =
                            if *editing_exceptions == Some(ExceptionKind::NeverAllow) {
                                None
                            } else {
                                Some(ExceptionKind::NeverAllow)
                            };
                    }
                }

                if let Some(exc_kind) = *editing_exceptions {
                    ui.add_space(6.0);
                    let heading = match exc_kind {
                        ExceptionKind::AlwaysAllow => strings::SELECT_ALWAYS_ALLOW,
                        ExceptionKind::NeverAllow => strings::SELECT_NEVER_ALLOW,
                    };
                    ui.label(
                        egui::RichText::new(heading)
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    for peer in contacts.iter().filter(|c| !c.is_group_or_channel) {
                        let target_list = match exc_kind {
                            ExceptionKind::AlwaysAllow => &mut draft.always_allow,
                            ExceptionKind::NeverAllow => &mut draft.never_allow,
                        };
                        let mut present = target_list.contains(&peer.id);
                        if ui
                            .checkbox(&mut present, format!("{} (@{})", peer.name, peer.username))
                            .changed()
                        {
                            if present {
                                target_list.push(peer.id);
                                // Remove from opposing exception list.
                                let opposite = match exc_kind {
                                    ExceptionKind::AlwaysAllow => &mut draft.never_allow,
                                    ExceptionKind::NeverAllow => &mut draft.always_allow,
                                };
                                opposite.retain(|&id| id != peer.id);
                            } else {
                                target_list.retain(|&id| id != peer.id);
                            }
                        }
                    }
                }
            }

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(key.localized_description())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    backend.set_privacy_rule(*key, draft.clone());
                    out.toast = Some(format!("{} updated", key.localized_title()));
                    close = true;
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
        PrivacyEditor::BlockedUsers => {
            ui.label(
                egui::RichText::new(tr::lng_blocked_list_title())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            let blocked = backend.blocked_users();
            if blocked.is_empty() {
                ui.label(
                    egui::RichText::new(tr::lng_blocked_list_empty_title())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            } else {
                let unblock_lbl = tr::lng_blocked_list_unblock();
                for peer in &blocked {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} (@{})", peer.name, peer.username))
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(&unblock_lbl).clicked() {
                                backend.unblock_peer(peer.id);
                                out.toast = Some(format!("{unblock_lbl}: {}", peer.name));
                            }
                        });
                    });
                }
            }

            // Quick-block from unblocked user contacts.
            let unblocked: Vec<_> = contacts
                .iter()
                .filter(|c| !c.is_group_or_channel && !blocked.iter().any(|b| b.id == c.id))
                .collect();
            if !unblocked.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(strings::BLOCK_A_CONTACT)
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowSubTextFg")),
                );
                let block_verb = tr::lng_blocked_list_confirm_ok();
                ui.horizontal_wrapped(|ui| {
                    for peer in unblocked {
                        if ui
                            .small_button(format!("{block_verb} {}", peer.name))
                            .clicked()
                        {
                            backend.block_peer(peer.id);
                            out.toast = Some(format!("{block_verb}: {}", peer.name));
                        }
                    }
                });
            }

            ui.add_space(10.0);
            if RoundButton::new(&tr::lng_close(), RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                close = true;
            }
        }
        PrivacyEditor::AccountTtl { selected_days } => {
            ui.label(
                egui::RichText::new(tr::lng_self_destruct_title())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            for &days in &ACCOUNT_TTL_OPTIONS {
                settings_radio_row(
                    ui,
                    selected_days,
                    days,
                    &format_ttl_days(days),
                    true,
                    colors,
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    backend.set_account_ttl_days(*selected_days);
                    out.toast = Some(format!(
                        "{}: {}",
                        tr::lng_self_destruct_title(),
                        format_ttl_days(*selected_days)
                    ));
                    close = true;
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
        PrivacyEditor::GlobalTtl { selected_secs } => {
            ui.label(
                egui::RichText::new(strings::AUTO_DELETE_NEW_CHATS_TITLE)
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            for &secs in &GLOBAL_TTL_OPTIONS_SECS {
                settings_radio_row(
                    ui,
                    selected_secs,
                    secs,
                    &format_global_ttl(secs),
                    true,
                    colors,
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    let mut gp = backend.global_privacy();
                    gp.global_ttl_seconds = *selected_secs;
                    backend.set_global_privacy(gp);
                    out.toast = Some(format!(
                        "{}: {}",
                        strings::AUTO_DELETE_MESSAGES_TITLE,
                        format_global_ttl(*selected_secs)
                    ));
                    close = true;
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
        PrivacyEditor::ClearPaymentInfo {
            clear_shipping,
            clear_credentials,
        } => {
            ui.label(
                egui::RichText::new(tr::lng_settings_clear_payment_info())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            settings_checkbox_row(
                ui,
                clear_shipping,
                strings::PAYMENT_SHIPPING_INFO,
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                clear_credentials,
                strings::PAYMENT_CREDENTIALS_INFO,
                true,
                colors,
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let any_selected = *clear_shipping || *clear_credentials;
                if RoundButton::new(
                    strings::PAYMENT_CLEAR_BUTTON,
                    RoundButtonStyle::attention_box(colors),
                )
                .enabled(any_selected)
                .show(ui)
                .clicked()
                {
                    backend.clear_payment_info(*clear_shipping, *clear_credentials);
                    out.toast = Some(strings::PAYMENT_CLEARED_TOAST.to_owned());
                    close = true;
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
    });

    if close {
        state.editor = PrivacyEditor::None;
    }
}
