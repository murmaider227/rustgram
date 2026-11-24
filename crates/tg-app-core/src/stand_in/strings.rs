//! Stand-in localization trait replacing `tg-lang` until integration.
//!
//! Port origin: `Telegram/Resources/langs/lang.strings`.

/// Narrow UI string provider trait so `tg-app` can be wired directly to `tg-lang`.
pub trait Strings: Send + Sync {
    /// Application display name.
    fn app_name(&self) -> &str;
    /// Intro welcome title.
    fn intro_title(&self) -> &str;
    /// Intro welcome subtitle.
    fn intro_about(&self) -> &str;
    /// Intro primary start button text.
    fn intro_start_messaging(&self) -> &str;
    /// Intro phone step title.
    fn intro_phone_title(&self) -> &str;
    /// Intro phone step description.
    fn intro_phone_desc(&self) -> &str;
    /// Intro phone input placeholder.
    fn intro_phone_placeholder(&self) -> &str;
    /// Intro code step title.
    fn intro_code_title(&self) -> &str;
    /// Intro code input placeholder.
    fn intro_code_placeholder(&self) -> &str;
    /// Intro 2FA password step title.
    fn intro_password_title(&self) -> &str;
    /// Intro 2FA password description.
    fn intro_password_desc(&self) -> &str;
    /// Intro 2FA password placeholder.
    fn intro_password_placeholder(&self) -> &str;
    /// Intro next button label.
    fn intro_next(&self) -> &str;
    /// Generic back label.
    fn back(&self) -> &str;
    /// Generic cancel label.
    fn cancel(&self) -> &str;
    /// Generic close label.
    fn close(&self) -> &str;
    /// Search placeholder in dialogs column.
    fn search_placeholder(&self) -> &str;
    /// Empty chat placeholder when no dialog is selected.
    fn select_chat_prompt(&self) -> &str;
    /// Compose input placeholder.
    fn write_message_placeholder(&self) -> &str;
    /// Send button label.
    fn send_button(&self) -> &str;
    /// Main menu item: My Profile.
    fn menu_my_profile(&self) -> &str;
    /// Main menu item: New Group.
    fn menu_new_group(&self) -> &str;
    /// Main menu item: New Channel.
    fn menu_new_channel(&self) -> &str;
    /// Main menu item: Contacts.
    fn menu_contacts(&self) -> &str;
    /// Main menu item: Calls.
    fn menu_calls(&self) -> &str;
    /// Main menu item: Saved Messages.
    fn menu_saved_messages(&self) -> &str;
    /// Main menu item: Settings.
    fn menu_settings(&self) -> &str;
    /// Main menu item: Night Mode.
    fn menu_night_mode(&self) -> &str;
    /// Main menu item: Lock Rustgram.
    fn menu_lock(&self) -> &str;
    /// Main menu item: Log Out.
    fn menu_logout(&self) -> &str;
    /// Connection banner text while connecting.
    fn connecting(&self) -> &str;
    /// Connection banner text while connecting through a proxy.
    fn connecting_to_proxy(&self) -> &str {
        "Connecting to proxy..."
    }
    /// Connection banner label when connected through a proxy.
    fn proxy_connected(&self) -> &str {
        "Proxy"
    }
    /// Connection banner text while waiting for retry.
    fn reconnecting_in(&self, seconds: u32) -> String;
    /// Connection retry link text.
    fn try_now(&self) -> &str;
    /// Passcode lock screen heading.
    fn passcode_enter(&self) -> &str;
    /// Passcode input placeholder.
    fn passcode_placeholder(&self) -> &str;
    /// Passcode submit button text.
    fn passcode_submit(&self) -> &str;
    /// Passcode wrong error text.
    fn passcode_wrong(&self) -> &str;
    /// Passcode flood-wait error text.
    fn passcode_flood(&self) -> &str;
    /// Info panel header title.
    fn info_title(&self) -> &str;
    /// Settings section header title.
    fn settings_title(&self) -> &str;
}

/// Default English strings matching `tdesktop`'s built-in English phrases.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnglishStrings;

impl Strings for EnglishStrings {
    fn app_name(&self) -> &str {
        "Rustgram"
    }

    fn intro_title(&self) -> &str {
        "Rustgram Desktop"
    }

    fn intro_about(&self) -> &str {
        "Fast, modular Rust port of Telegram Desktop.\nRunning in local mock mode — no network credentials required."
    }

    fn intro_start_messaging(&self) -> &str {
        "Start Messaging"
    }

    fn intro_phone_title(&self) -> &str {
        "Your Phone Number"
    }

    fn intro_phone_desc(&self) -> &str {
        "Enter any synthetic test number to continue (e.g. +999 661 0000). Never use a real account here."
    }

    fn intro_phone_placeholder(&self) -> &str {
        "+999 661 0000"
    }

    fn intro_code_title(&self) -> &str {
        "Enter Code"
    }

    fn intro_code_placeholder(&self) -> &str {
        "12345"
    }

    fn intro_password_title(&self) -> &str {
        "Cloud Password"
    }

    fn intro_password_desc(&self) -> &str {
        "Your mock account is protected with a Two-Step Verification password."
    }

    fn intro_password_placeholder(&self) -> &str {
        "Password"
    }

    fn intro_next(&self) -> &str {
        "Next"
    }

    fn back(&self) -> &str {
        "Back"
    }

    fn cancel(&self) -> &str {
        "Cancel"
    }

    fn close(&self) -> &str {
        "Close"
    }

    fn search_placeholder(&self) -> &str {
        "Search"
    }

    fn select_chat_prompt(&self) -> &str {
        "Select a chat to start messaging"
    }

    fn write_message_placeholder(&self) -> &str {
        "Write a message..."
    }

    fn send_button(&self) -> &str {
        "Send"
    }

    fn menu_my_profile(&self) -> &str {
        "My Profile"
    }

    fn menu_new_group(&self) -> &str {
        "New Group"
    }

    fn menu_new_channel(&self) -> &str {
        "New Channel"
    }

    fn menu_contacts(&self) -> &str {
        "Contacts"
    }

    fn menu_calls(&self) -> &str {
        "Calls"
    }

    fn menu_saved_messages(&self) -> &str {
        "Saved Messages"
    }

    fn menu_settings(&self) -> &str {
        "Settings"
    }

    fn menu_night_mode(&self) -> &str {
        "Night Mode"
    }

    fn menu_lock(&self) -> &str {
        "Lock Rustgram"
    }

    fn menu_logout(&self) -> &str {
        "Log Out"
    }

    fn connecting(&self) -> &str {
        "Connecting..."
    }

    fn reconnecting_in(&self, seconds: u32) -> String {
        format!("Reconnecting in {seconds}s...")
    }

    fn try_now(&self) -> &str {
        "Try now"
    }

    fn passcode_enter(&self) -> &str {
        "Enter your local passcode"
    }

    fn passcode_placeholder(&self) -> &str {
        "Your passcode"
    }

    fn passcode_submit(&self) -> &str {
        "Submit"
    }

    fn passcode_wrong(&self) -> &str {
        "Wrong passcode, please try again."
    }

    fn passcode_flood(&self) -> &str {
        "Too many tries, please wait a moment."
    }

    fn info_title(&self) -> &str {
        "Chat Info"
    }

    fn settings_title(&self) -> &str {
        "Settings"
    }
}

/// Immutable phrase snapshot evaluated from `tg_lang::tr::*` for one global language generation.
#[derive(Debug, Clone)]
struct TgLangStringsSnapshot {
    app_name: String,
    intro_title: String,
    intro_about: String,
    intro_start_messaging: String,
    intro_phone_title: String,
    intro_phone_desc: String,
    intro_phone_placeholder: String,
    intro_code_title: String,
    intro_code_placeholder: String,
    intro_password_title: String,
    intro_password_desc: String,
    intro_password_placeholder: String,
    intro_next: String,
    back: String,
    cancel: String,
    close: String,
    search_placeholder: String,
    select_chat_prompt: String,
    write_message_placeholder: String,
    send_button: String,
    menu_my_profile: String,
    menu_new_group: String,
    menu_new_channel: String,
    menu_contacts: String,
    menu_calls: String,
    menu_saved_messages: String,
    menu_settings: String,
    menu_night_mode: String,
    menu_lock: String,
    menu_logout: String,
    connecting: String,
    try_now: String,
    passcode_enter: String,
    passcode_placeholder: String,
    passcode_submit: String,
    passcode_wrong: String,
    passcode_flood: String,
    info_title: String,
    settings_title: String,
}

impl TgLangStringsSnapshot {
    fn from_global() -> Self {
        use tg_lang::tr;

        Self {
            app_name: "Rustgram".to_owned(),
            intro_title: "Rustgram Desktop".to_owned(),
            intro_about: tr::lng_intro_about().replace("Telegram", "Rustgram"),
            intro_start_messaging: tr::lng_start_msgs(),
            intro_phone_title: tr::lng_phone_title(),
            intro_phone_desc: tr::lng_phone_desc(),
            intro_phone_placeholder: "+999 661 0000".to_owned(),
            intro_code_title: tr::lng_intro_fragment_title(),
            intro_code_placeholder: "12345".to_owned(),
            intro_password_title: tr::lng_signin_password(),
            intro_password_desc: tr::lng_signin_password(),
            intro_password_placeholder: tr::lng_signin_password(),
            intro_next: tr::lng_intro_next(),
            back: tr::lng_go_back(),
            cancel: tr::lng_cancel(),
            close: tr::lng_close(),
            search_placeholder: tr::lng_dlg_filter(),
            select_chat_prompt: tr::lng_willbe_history(),
            write_message_placeholder: tr::lng_message_ph(),
            send_button: tr::lng_send_button(),
            menu_my_profile: tr::lng_menu_my_profile(),
            menu_new_group: tr::lng_create_group_title(),
            menu_new_channel: tr::lng_create_channel_title(),
            menu_contacts: tr::lng_menu_contacts(),
            menu_calls: tr::lng_menu_calls(),
            menu_saved_messages: tr::lng_saved_messages(),
            menu_settings: tr::lng_menu_settings(),
            menu_night_mode: tr::lng_menu_night_mode(),
            menu_lock: "Lock Rustgram".to_owned(),
            menu_logout: tr::lng_settings_logout(),
            connecting: tr::lng_connecting(),
            try_now: tr::lng_reconnecting_try_now(),
            passcode_enter: tr::lng_passcode_enter(),
            passcode_placeholder: tr::lng_passcode_ph(),
            passcode_submit: tr::lng_passcode_submit(),
            passcode_wrong: tr::lng_signin_bad_password(),
            passcode_flood: tr::lng_flood_error(),
            info_title: tr::lng_settings_section_info(),
            settings_title: tr::lng_menu_settings(),
        }
    }
}

static LIVE_STRINGS_CACHE: std::sync::OnceLock<
    std::sync::RwLock<(u64, &'static TgLangStringsSnapshot)>,
> = std::sync::OnceLock::new();

fn live_strings_snapshot(force_refresh: bool) -> &'static TgLangStringsSnapshot {
    let generation = tg_lang::global_generation();
    let lock = LIVE_STRINGS_CACHE.get_or_init(|| {
        let initial: &'static TgLangStringsSnapshot =
            Box::leak(Box::new(TgLangStringsSnapshot::from_global()));
        std::sync::RwLock::new((generation, initial))
    });
    if !force_refresh {
        let guard = lock.read().unwrap_or_else(|e| e.into_inner());
        if guard.0 == generation {
            return guard.1;
        }
    }
    let mut guard = lock.write().unwrap_or_else(|e| e.into_inner());
    if force_refresh || guard.0 != generation {
        let fresh: &'static TgLangStringsSnapshot =
            Box::leak(Box::new(TgLangStringsSnapshot::from_global()));
        *guard = (generation, fresh);
    }
    guard.1
}

/// Live localization provider backed by `tg-lang`'s generated [`tg_lang::tr`] phrase functions.
///
/// Tracks [`tg_lang::global_generation`] automatically so switching the active language pack
/// at runtime immediately updates every UI string on the next render frame without restarting
/// the application or requiring mutable access to `TgLangStrings`.
#[derive(Debug, Clone)]
pub struct TgLangStrings {
    last_generation: u64,
}

impl Default for TgLangStrings {
    fn default() -> Self {
        Self::new()
    }
}

impl TgLangStrings {
    /// Creates a new [`TgLangStrings`] handle synced to the active `tg-lang` pack.
    #[must_use]
    pub fn new() -> Self {
        let _ = live_strings_snapshot(false);
        Self {
            last_generation: tg_lang::global_generation(),
        }
    }

    /// Returns the active global `tg-lang` generation counter.
    #[must_use]
    pub fn generation(&self) -> u64 {
        tg_lang::global_generation()
    }

    /// Re-evaluates all phrases from the active `tg-lang` language instance immediately.
    pub fn refresh(&mut self) {
        let _ = live_strings_snapshot(true);
        self.last_generation = tg_lang::global_generation();
    }

    #[inline]
    fn snap(&self) -> &'static TgLangStringsSnapshot {
        let _ = self.last_generation;
        live_strings_snapshot(false)
    }
}

impl Strings for TgLangStrings {
    fn app_name(&self) -> &str {
        &self.snap().app_name
    }

    fn intro_title(&self) -> &str {
        &self.snap().intro_title
    }

    fn intro_about(&self) -> &str {
        &self.snap().intro_about
    }

    fn intro_start_messaging(&self) -> &str {
        &self.snap().intro_start_messaging
    }

    fn intro_phone_title(&self) -> &str {
        &self.snap().intro_phone_title
    }

    fn intro_phone_desc(&self) -> &str {
        &self.snap().intro_phone_desc
    }

    fn intro_phone_placeholder(&self) -> &str {
        &self.snap().intro_phone_placeholder
    }

    fn intro_code_title(&self) -> &str {
        &self.snap().intro_code_title
    }

    fn intro_code_placeholder(&self) -> &str {
        &self.snap().intro_code_placeholder
    }

    fn intro_password_title(&self) -> &str {
        &self.snap().intro_password_title
    }

    fn intro_password_desc(&self) -> &str {
        &self.snap().intro_password_desc
    }

    fn intro_password_placeholder(&self) -> &str {
        &self.snap().intro_password_placeholder
    }

    fn intro_next(&self) -> &str {
        &self.snap().intro_next
    }

    fn back(&self) -> &str {
        &self.snap().back
    }

    fn cancel(&self) -> &str {
        &self.snap().cancel
    }

    fn close(&self) -> &str {
        &self.snap().close
    }

    fn search_placeholder(&self) -> &str {
        &self.snap().search_placeholder
    }

    fn select_chat_prompt(&self) -> &str {
        &self.snap().select_chat_prompt
    }

    fn write_message_placeholder(&self) -> &str {
        &self.snap().write_message_placeholder
    }

    fn send_button(&self) -> &str {
        &self.snap().send_button
    }

    fn menu_my_profile(&self) -> &str {
        &self.snap().menu_my_profile
    }

    fn menu_new_group(&self) -> &str {
        &self.snap().menu_new_group
    }

    fn menu_new_channel(&self) -> &str {
        &self.snap().menu_new_channel
    }

    fn menu_contacts(&self) -> &str {
        &self.snap().menu_contacts
    }

    fn menu_calls(&self) -> &str {
        &self.snap().menu_calls
    }

    fn menu_saved_messages(&self) -> &str {
        &self.snap().menu_saved_messages
    }

    fn menu_settings(&self) -> &str {
        &self.snap().menu_settings
    }

    fn menu_night_mode(&self) -> &str {
        &self.snap().menu_night_mode
    }

    fn menu_lock(&self) -> &str {
        &self.snap().menu_lock
    }

    fn menu_logout(&self) -> &str {
        &self.snap().menu_logout
    }

    fn connecting(&self) -> &str {
        &self.snap().connecting
    }

    fn reconnecting_in(&self, seconds: u32) -> String {
        tg_lang::tr::lng_reconnecting(i64::from(seconds))
    }

    fn try_now(&self) -> &str {
        &self.snap().try_now
    }

    fn passcode_enter(&self) -> &str {
        &self.snap().passcode_enter
    }

    fn passcode_placeholder(&self) -> &str {
        &self.snap().passcode_placeholder
    }

    fn passcode_submit(&self) -> &str {
        &self.snap().passcode_submit
    }

    fn passcode_wrong(&self) -> &str {
        &self.snap().passcode_wrong
    }

    fn passcode_flood(&self) -> &str {
        &self.snap().passcode_flood
    }

    fn info_title(&self) -> &str {
        &self.snap().info_title
    }

    fn settings_title(&self) -> &str {
        &self.snap().settings_title
    }
}
