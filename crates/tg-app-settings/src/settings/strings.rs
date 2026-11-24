//! Rustgram-specific user-facing strings for `settings/` that have no equivalent
//! key in `resources/lang.strings`.
//!
//! All standard Telegram Desktop phrases live in `tg_lang::tr::*`.

/// Footer caption on the Main Settings page.
pub const MAIN_FOOTER_TEMPLATE: &str = "Rustgram Desktop v{version}  ·  GPL-3.0  ·  Type secret codes (e.g. debugmode) anywhere in Settings";

/// Search field placeholder on Main Settings.
pub const SEARCH_SETTINGS_PLACEHOLDER: &str = "Search settings...";
/// Heading above search hits on Main Settings.
pub const SEARCH_RESULTS_HEADING: &str = "Search results";
/// Empty search state on Main Settings.
pub const SEARCH_NO_MATCHES: &str = "No matching settings found.";

/// Main Settings cover & QR helpers without direct `lang.strings` keys.
pub const PHONE_NUMBER_HIDDEN: &str = "Phone number hidden";
pub const PHONE_COPIED_TOAST: &str = "Phone number copied to clipboard";
pub const PROFILE_QR_TOOLTIP: &str = "Profile QR Code";
pub const PROFILE_QR_SCAN_HINT: &str = "Scan this QR code to open your profile link.";
pub const PREMIUM_TITLE: &str = "Telegram Premium";
pub const PREMIUM_SUBTITLE: &str = "Unlock higher limits, faster downloads, and exclusive features";
pub const PREMIUM_OPEN_TOAST: &str = "Opening Telegram Premium";
pub const FAQ_URL_TOAST: &str = "FAQ: https://telegram.org/faq";

/// Search index entries (title, section breadcrumb).
pub const SEARCH_ENTRY_ACCOUNT_TITLE: &str = "Edit Name, Username & Bio";
pub const SEARCH_ENTRY_NOTIFY_TITLE: &str = "Notifications, Sound & Corner Position";
pub const SEARCH_ENTRY_PRIVACY_TITLE: &str = "Privacy Rules, Blocked Users & Auto-Delete";
pub const SEARCH_ENTRY_PASSCODE_TITLE: &str = "Local Passcode & Auto-Lock";
pub const SEARCH_ENTRY_CLOUD_PWD_TITLE: &str = "Two-Step Verification (Cloud Password)";
pub const SEARCH_ENTRY_SESSIONS_TITLE: &str = "Active Sessions & Devices";
pub const SEARCH_ENTRY_WEBSITES_TITLE: &str = "Connected Websites";
pub const SEARCH_ENTRY_CHAT_TITLE: &str = "Themes, Accent Colors, Backgrounds & Emoji";
pub const SEARCH_ENTRY_FOLDERS_TITLE: &str = "Chat Folders & Filters";
pub const SEARCH_ENTRY_ADVANCED_TITLE: &str = "Connection, Proxy, Storage & System Integration";
pub const SEARCH_ENTRY_CALLS_TITLE: &str = "Speakers, Microphone, Camera & Calls";
pub const SEARCH_ENTRY_POWER_TITLE: &str = "Animations & Power Saving";
pub const SEARCH_ENTRY_LANGUAGE_TITLE: &str = "Interface Language & Translation";

/// Information / profile sub-editor Rustgram copy.
pub const CHANGE_PROFILE_PHOTO_TOOLTIP: &str = "Change profile photo";
pub const BIO_ABOUT_EXAMPLE: &str =
    "Any details such as age, occupation or city. Example: 23 y.o. designer.";
pub const BIO_UPDATED_TOAST: &str = "Bio updated";
pub const BIO_RESET_BUTTON: &str = "Reset";
pub const PHONE_HIDDEN_CLICK_REVEAL: &str = "Hidden (click to reveal)";
pub const PHONE_VISIBILITY_SUBTITLE: &str = "Click to toggle visibility on settings cover";
pub const PHONE_HIDDEN_TOAST: &str = "Phone number hidden in settings header";
pub const PHONE_VISIBLE_TOAST: &str = "Phone number visible in settings header";
pub const USERNAME_NONE: &str = "None";
pub const NAME_UPDATED_TOAST: &str = "Name updated";
pub const USERNAME_SAVED_TOAST: &str = "Username saved";
pub const USERNAME_REMOVE_HINT: &str = "Leave empty to remove your public username.";
pub const USERNAME_CURRENT_HINT: &str = "This is your current username.";
pub const USERNAME_TOO_LONG: &str = "Username must be at most 32 characters.";
pub const BIRTHDAY_DAY_PH: &str = "Day (1-31)";
pub const BIRTHDAY_MONTH_PH: &str = "Month (1-12)";
pub const BIRTHDAY_YEAR_PH: &str = "Year (opt)";
pub const BIRTHDAY_INVALID_ERROR: &str = "Enter valid numeric day, month, and optional year.";
pub const BIRTHDAY_REMOVED_TOAST: &str = "Date of birth removed";
pub const PHOTO_AVATAR_EDITOR_TITLE: &str = "Profile Photo / Avatar Color";
pub const PHOTO_LABEL_PH: &str = "Optional photo filename/note";
pub const PHOTO_APPLY_BUTTON: &str = "Apply";
pub const PHOTO_UPDATED_TOAST: &str = "Profile avatar updated";
pub const EDIT_YOUR_NAME_TITLE: &str = "Edit your name";

/// Notifications section Rustgram copy.
pub const NATIVE_NOTIFICATIONS_LABEL: &str = "Use Windows notifications";
pub const NATIVE_NOTIFICATIONS_SUBTITLE: &str =
    "Deliver via native WinRT toast notifications (tg-platform)";
pub const NOTIFICATION_VOLUME_LABEL: &str = "Notification volume";
pub const FLASH_TASKBAR_LABEL: &str = "Flash taskbar icon on new messages";
pub const SKIP_TOASTS_FOCUS_LABEL: &str = "Skip notifications when window is focused";
pub const NOTIFY_PREVIEW_TITLE: &str = "Notification preview";
pub const NOTIFY_SHOW_PREVIEW: &str = "Show sender name and message text";
pub const NOTIFY_SHOW_NAME: &str = "Show sender name only";
pub const NOTIFY_SHOW_NOTHING: &str = "Show nothing (generic notification)";
pub const CORNER_TOP_LEFT: &str = "Top Left";
pub const CORNER_TOP_RIGHT: &str = "Top Right";
pub const CORNER_BOTTOM_LEFT: &str = "Bottom Left";
pub const CORNER_BOTTOM_RIGHT: &str = "Bottom Right";
pub const CORNER_SCREEN_CORNER: &str = "Screen corner";
pub const CORNER_SELECTED_PREFIX: &str = "Selected:";
pub const CORNER_CLICK_HINT: &str =
    "Click any corner on the monitor preview to move custom toasts.";
pub const RESET_ALL_NOTIFICATIONS_TITLE: &str = "Reset All Notifications";
pub const RESET_ALL_NOTIFICATIONS_SUBTITLE: &str =
    "Undo all custom notification settings and exceptions";
pub const RESET_ALL_NOTIFICATIONS_TOAST: &str = "All notification settings reset to defaults";
pub const SCOPE_SHOW_PREVIEW: &str = "Show message preview for this scope";
pub const EXCEPTION_ADDED_TOAST: &str = "Notification exception added";
pub const RINGTONE_REMOVED_TOAST: &str = "Ringtone removed";
pub const RINGTONES_EMPTY_HINT: &str =
    "No custom ringtones saved yet. Upload an MP3/OGG tone (up to 100 KB, 5 seconds).";

/// Connected websites Rustgram copy.
pub const WEBSITE_DISCONNECTED_TOAST: &str = "Disconnected website";
pub const WEBSITES_DISCONNECTED_ALL_TOAST: &str = "Disconnected all websites";

/// Privacy & Security Rustgram copy.
pub const SECURITY_SECTION_TITLE: &str = "Security";
pub const AUTO_DELETE_MESSAGES_TITLE: &str = "Auto-Delete Messages";
pub const AUTO_DELETE_NEW_CHATS_TITLE: &str = "Auto-Delete Messages in New Chats";
pub const NEW_CHATS_UNKNOWN_TITLE: &str = "New Chats from Unknown Users";
pub const ARCHIVE_AND_MUTE_TITLE: &str = "Archive and mute";
pub const ARCHIVE_AND_MUTE_SUBTITLE: &str =
    "Automatically archive and mute new chats from people not in your contacts";
pub const HIDE_READ_TIME_TITLE: &str = "Hide read time";
pub const HIDE_READ_TIME_SUBTITLE: &str = "Hide the exact time you read messages in private chats";
pub const IF_AWAY_FOR_TITLE: &str = "If away for";
pub const PAYMENT_SHIPPING_INFO: &str = "Shipping info";
pub const PAYMENT_CREDENTIALS_INFO: &str = "Payment info";
pub const PAYMENT_CLEAR_BUTTON: &str = "Clear";
pub const PAYMENT_CLEARED_TOAST: &str = "Saved payment and shipping info cleared";
pub const FORWARDS_PREVIEW_EVERYBODY: &str = "Forwarded from Taylor Kim (clickable link)";
pub const FORWARDS_PREVIEW_CONTACTS: &str = "Forwarded from Taylor Kim (link for contacts only)";
pub const FORWARDS_PREVIEW_NOBODY: &str = "Forwarded from Taylor Kim (no profile link)";
pub const FORWARDS_PREVIEW_SAMPLE: &str = "Sample forwarded message preview.";
pub const SELECT_ALWAYS_ALLOW: &str = "Select users to ALWAYS allow:";
pub const SELECT_NEVER_ALLOW: &str = "Select users to NEVER allow:";
pub const BLOCK_A_CONTACT: &str = "Block a contact:";

/// Active sessions Rustgram copy.
pub const RENAME_DEVICE_TITLE: &str = "Rename this device";
pub const RENAME_BUTTON: &str = "Rename";
pub const DEVICE_NAME_UPDATED_TOAST: &str = "Device name updated";
pub const NO_OTHER_SESSIONS: &str = "No other active sessions.";
pub const ACCEPT_CALLS_LABEL: &str = "Accept calls";
pub const ACCEPT_SECRET_CHATS_LABEL: &str = "Accept secret chats";
pub const AUTO_TERMINATE_SESSIONS_TITLE: &str = "Automatically terminate old sessions";
pub const IF_INACTIVE_FOR_LABEL: &str = "If inactive for";

/// Local passcode Rustgram copy.
pub const PASSCODE_STRENGTH_WEAK: &str = "Weak";
pub const PASSCODE_STRENGTH_FAIR: &str = "Fair";
pub const PASSCODE_STRENGTH_GOOD: &str = "Good";
pub const PASSCODE_STRENGTH_STRONG: &str = "Strong";
pub const PASSCODE_TOO_MANY_ATTEMPTS: &str =
    "Too many attempts. Please wait a few seconds before trying again.";
pub const PASSCODE_EMPTY_ERROR: &str = "Passcode cannot be empty.";
pub const PASSCODE_PROTECT_HEADING: &str = "Protect your local chats with a passcode";
pub const PASSCODE_PROTECT_DESC: &str = "Your local passcode encrypts local storage using PBKDF2-HMAC-SHA512 (tg-crypto) and lets you lock Rustgram manually or after inactivity.";
pub const PASSCODE_TURN_ON_BUTTON: &str = "Turn On Local Passcode";
pub const PASSCODE_LOCK_NOW: &str = "Lock Now";
pub const PASSCODE_LOCK_SHORTCUT: &str = "Shortcut: Ctrl+L";
pub const PASSCODE_AUTO_LOCK_TITLE: &str = "Auto-Lock";
pub const PASSCODE_AUTO_LOCK_IF_AWAY: &str = "Auto-lock if away for";
pub const PASSCODE_CUSTOM_HHMM_PH: &str = "Custom HH:MM";
pub const PASSCODE_SET_CUSTOM_BTN: &str = "Set Custom";
pub const PASSCODE_CUSTOM_HINT: &str = "Enter custom timeout as HH:MM (e.g. 0:15)";
pub const PASSCODE_WINHELLO_TITLE: &str = "Unlock with Windows Hello";
pub const PASSCODE_WINHELLO_SUBTITLE: &str =
    "Allow biometric / OS authentication to unlock Rustgram";
pub const PASSCODE_ENABLED_TOAST: &str = "Local passcode enabled";
pub const PASSCODE_CHANGED_TOAST: &str = "Local passcode changed";
pub const PASSCODE_DISABLED_TOAST: &str = "Local passcode disabled";
pub const PASSCODE_DISABLE_CONFIRM: &str = "Turn off local passcode lock?";
pub const PASSCODE_TURN_OFF_BTN: &str = "Turn Off";

/// Cloud password (2FA) Rustgram copy.
pub const CLOUD_PWD_SET_BUTTON: &str = "Set Password";
pub const CLOUD_PWD_IS_ON_TITLE: &str = "Two-Step Verification is On";
pub const CLOUD_PWD_NO_HINT: &str = "No hint set";
pub const CLOUD_PWD_EMAIL_NOT_SET: &str = "Recovery email: not set";
pub const CLOUD_PWD_TURN_OFF_ROW: &str = "Turn Off Password";
pub const CLOUD_PWD_STEP1_CHANGE: &str = "Step 1 of 3 — New Password";
pub const CLOUD_PWD_STEP1_CREATE: &str = "Step 1 of 3 — Create Password";
pub const CLOUD_PWD_STEP2_HINT: &str = "Step 2 of 3 — Password Hint";
pub const CLOUD_PWD_STEP3_EMAIL: &str = "Step 3 of 3 — Recovery Email";
pub const CLOUD_PWD_SKIP_BTN: &str = "Skip";
pub const CLOUD_PWD_SKIP_ANYWAY_BTN: &str = "Skip Anyway";
pub const CLOUD_PWD_SAVE_SEND_CODE: &str = "Save & Send Code";
pub const CLOUD_PWD_ENTER_OR_SKIP: &str = "Enter an email or click Skip.";
pub const CLOUD_PWD_CODE_PH: &str = "6-digit code";
pub const CLOUD_PWD_CONFIRM_BTN: &str = "Confirm";
pub const CLOUD_PWD_ABORT_EMAIL_BTN: &str = "Abort Email";
pub const CLOUD_PWD_EMAIL_CONFIRMED_TOAST: &str = "Recovery email confirmed";
pub const CLOUD_PWD_EMAIL_CANCELLED_TOAST: &str = "Pending recovery email cancelled";
pub const CLOUD_PWD_DISABLE_TITLE: &str = "Disable Two-Step Verification";
pub const CLOUD_PWD_EMPTY_ERROR: &str = "Password cannot be empty.";

/// Chat Settings Rustgram copy.
pub const FOLLOW_SYSTEM_DARK_MODE: &str = "Follow system dark mode";
pub const TEXT_SIZE_AND_FONT: &str = "Text size & font";
pub const MESSAGES_SECTION: &str = "Messages";
pub const DOUBLE_CLICK_MESSAGE: &str = "Double-click on message";
pub const QUICK_ACTION_REPLY: &str = "Reply";
pub const QUICK_ACTION_REACT: &str = "React";
pub const QUICK_ACTION_DISABLED: &str = "Disabled";
pub const VERTICAL_FOLDERS_SIDEBAR: &str = "Show vertical chat folders sidebar";
pub const VERTICAL_FOLDERS_SUBTITLE: &str = "72px left rail vs horizontal folder tabs";
pub const PULL_NEXT_CHANNEL: &str = "Pull at bottom of channel for next unread";
pub const MANAGE_STICKER_SETS: &str = "Manage Sticker & Emoji Sets";
pub const AUTOPLAY_MEDIA_TITLE: &str = "Autoplay media";
pub const AUTOPLAY_VIDEOS: &str = "Autoplay videos";
pub const AUTOPLAY_GIFS: &str = "Autoplay GIFs";
pub const AUTOPLAY_ROUND_VIDEOS: &str = "Autoplay round video messages";
pub const AUTOPLAY_VIDEO_STICKERS: &str = "Autoplay animated stickers";
pub const TRANSLATION_SECTION: &str = "Translation";
pub const SHOW_TRANSLATE_BUTTON: &str = "Show Translate button";
pub const SHOW_TRANSLATE_SUBTITLE: &str = "Add a Translate option to the message context menu";
pub const TRANSLATE_ENTIRE_CHATS: &str = "Translate entire chats";
pub const CUSTOM_FONT_PH: &str = "Or type a font family name";
pub const CUSTOM_ACCENT_TOOLTIP: &str = "Custom hex accent color";
pub const APPLY_HEX_BTN: &str = "Apply Hex";
pub const INVALID_HEX_TOAST: &str = "Enter a valid #RRGGBB hex color";
pub const RESET_ACCENT_TOAST: &str = "Reset to default theme accent";
pub const BLUR_BACKGROUND: &str = "Blur background";
pub const STICKERS_INSTALLED_TAB: &str = "Installed";
pub const STICKERS_ARCHIVED_TAB: &str = "Archived";
pub const STICKERS_EMPTY_TAB: &str = "No sets in this tab.";
pub const STICKERS_ARCHIVE_BTN: &str = "Archive";
pub const STICKERS_RESTORE_BTN: &str = "Restore";

/// Chat Folders Rustgram copy.
pub const FOLDERS_ORGANIZE_TITLE: &str = "Organize your chats into folders";
pub const FOLDERS_MY_FOLDERS: &str = "My Folders";
pub const FOLDERS_EMPTY: &str = "No custom folders yet.";
pub const FOLDERS_RESTORED_TOAST: &str = "Folder restored";
pub const FOLDERS_TABS_ON_TOP: &str = "Tabs on the top";
pub const FOLDERS_TABS_ON_TOP_SUBTITLE: &str =
    "Switch between vertical left sidebar and horizontal top tabs";
pub const FOLDERS_ICON_LABEL: &str = "Icon";
pub const FOLDERS_INCLUDED_CHATS_SPECIFIC: &str = "Included Specific Chats";

/// Advanced & Power Saving Rustgram copy.
pub const CONNECTION_TYPE_TITLE: &str = "Connection type";
pub const CONNECTION_PROXY_NONE: &str = "Proxy (none selected)";
pub const CONNECTION_SYSTEM_PROXY: &str = "Default (system proxy)";
pub const CONNECTION_TCP_DIRECT: &str = "Default (TCP direct)";
pub const DOWNLOAD_PATH_TITLE: &str = "Download path";
pub const DOWNLOAD_PATH_DEFAULT: &str = "Default folder";
pub const DOWNLOAD_PATH_PH: &str = "Leave empty for default Downloads folder";
pub const ASK_DOWNLOAD_PATH: &str = "Ask download path for each file";
pub const EXPORT_DATA_SUBTITLE: &str = "Export chats and media to HTML or JSON (tg-export)";
pub const AUTO_MEDIA_DOWNLOAD_TITLE: &str = "Automatic media download";
pub const PERF_POWER_SAVING_TITLE: &str = "Performance and power saving";
pub const POWER_REDUCE_SUBTITLE: &str = "Reduce animations, stickers, and effects to save battery";
pub const POWER_ALL_OFF: &str = "All off";
pub const HARDWARE_VIDEO_DECODING: &str = "Hardware accelerated video decoding";
pub const DISABLE_OPENGL: &str = "Disable OpenGL (software fallback)";
pub const MONOCHROME_TRAY_ICON: &str = "Monochrome tray icon";
pub const CLOSE_BEHAVIOR_QUIT: &str = "Quit Rustgram";
pub const CLOSE_BEHAVIOR_BACKGROUND: &str = "Keep running in background (tray)";
pub const AUTOSTART_SUBTITLE: &str = "Managed via Windows Startup shortcut (tg-platform)";
pub const AUTOSTART_CREATED_TOAST: &str = "Autostart shortcut created";
pub const AUTOSTART_REMOVED_TOAST: &str = "Autostart shortcut removed";
pub const SEND_TO_UPDATED_TOAST: &str = "Explorer 'Send to' menu updated";
pub const WINDOW_TITLE_CHAT_NAME: &str = "Show active chat name";
pub const WINDOW_TITLE_ACCOUNT_NAME: &str = "Show active account name";
pub const WINDOW_TITLE_UNREAD: &str = "Show unread messages counter";
pub const USE_SYSTEM_SPELLCHECKER: &str = "Use system spell checker";
pub const SPELLCHECKER_AVAILABLE: &str = "Windows ISpellChecker available";
pub const SPELLCHECKER_UNAVAILABLE: &str = "System spellchecker unavailable on this OS";
pub const VERSION_AND_UPDATES_TITLE: &str = "Version and updates";
pub const INSTALL_BETA_VERSIONS: &str = "Install beta versions";
pub const RESTART_TO_UPDATE: &str = "Restart to update";
pub const EXPERIMENTAL_DEBUG_SUBTITLE: &str =
    "Secret codes, debug logs, adaptive layout diagnostics";
pub const PROXY_EDITOR_TITLE: &str = "Connection Type & Proxy List";
pub const PROXY_DISABLE_DIRECT: &str = "Disable proxy (direct TCP connection)";
pub const PROXY_USE_SYSTEM: &str = "Use system proxy settings";
pub const PROXY_USE_CUSTOM: &str = "Use custom proxy";
pub const PROXY_USE_FOR_CALLS: &str = "Use proxy for calls (SOCKS5 only)";
pub const PROXY_AUTO_ROTATION: &str = "Automatic proxy rotation on disconnect";
pub const PROXY_ROTATION_TIMEOUT: &str = "Rotation timeout:";
pub const PROXY_NO_CUSTOM: &str = "No custom proxies configured yet.";
pub const PROXY_COPY_LINK: &str = "Copy Link";
pub const PROXY_SHARE_BTN: &str = "Share";
pub const PROXY_EDIT_BTN: &str = "Edit";
pub const PROXY_CHECK_BTN: &str = "Check";
pub const PROXY_CHECK_ALL_BTN: &str = "Check All";
pub const PROXY_REMOVED_TOAST: &str = "Proxy removed";
pub const PROXY_UPDATED_TOAST: &str = "Proxy updated";
pub const PROXY_SHARED_TOAST: &str = "Proxy link copied to clipboard";
pub const PROXY_ADD_BTN: &str = "Add Proxy";
pub const PROXY_EDIT_TITLE: &str = "Edit Proxy";
pub const PROXY_HIDE_ADD_BTN: &str = "Hide Add Proxy Form";
pub const PROXY_CANCEL_EDIT_BTN: &str = "Cancel Edit";
pub const PROXY_IMPORT_HINT: &str = "Import from tg://proxy or https://t.me/socks link:";
pub const PROXY_PARSE_LINK_BTN: &str = "Parse Link";
pub const PROXY_PARSED_TOAST: &str = "Parsed proxy link into fields";
pub const PROXY_SAVE_BTN: &str = "Save Proxy";
pub const PROXY_ADDED_TOAST: &str = "Proxy added and enabled";
pub const PROXY_CONNECTING_LABEL: &str = "Connecting to proxy...";
pub const PROXY_CONNECTED_LABEL: &str = "Proxy";
pub const PROXY_SIMULATOR_LABEL: &str = "Connection state simulator (offline mock)";
pub const CONN_CONNECTED: &str = "Connected";
pub const CONN_CONNECTING: &str = "Connecting";
pub const CONN_WAITING_8S: &str = "Waiting (8s)";
pub const STORAGE_PHOTOS: &str = "Photos & thumbnails";
pub const STORAGE_VIDEOS: &str = "Videos & animations";
pub const STORAGE_FILES: &str = "Files & music";
pub const STORAGE_STICKERS: &str = "Stickers & emoji";
pub const STORAGE_NO_LIMIT: &str = "No limit";
pub const AUTO_DOWNLOAD_RESET_BTN: &str = "Reset to Defaults";
pub const AUTO_DOWNLOAD_ON: &str = "On";
pub const EXPORT_ACCOUNT_INFO: &str = "Account information & contacts";
pub const EXPORT_PERSONAL_CHATS: &str = "Personal chats";
pub const EXPORT_GROUPS_CHANNELS: &str = "Groups and channels";
pub const EXPORT_INCLUDE_MEDIA: &str = "Include media attachments";
pub const EXPORT_FORMAT_LABEL: &str = "Format:";
pub const EXPORT_FORMAT_HTML: &str = "Human-readable HTML";
pub const EXPORT_FORMAT_JSON: &str = "Machine-readable JSON";
pub const EXPORT_FORMAT_BOTH: &str = "Both";
pub const EXPORT_PATH_PH: &str = "Export destination directory";
pub const POWER_DISABLE_ALL_BTN: &str = "Disable All Animations";
pub const POWER_ENABLE_ALL_BTN: &str = "Enable All Animations";
pub const POWER_AC_DESKTOP: &str = "Power: AC Desktop (no system battery detected)";
pub const DEBUG_TOOLS_HEADER: &str = "Experimental & Debug Options (settings_codes.cpp)";
pub const DEBUG_LOGGING: &str = "Debug logging (debugmode)";
pub const DEBUG_MODERATE: &str = "Moderate mode (moderate)";
pub const DEBUG_FAST_BUTTONS: &str = "Fast buttons mode (numberbuttons)";
pub const DEBUG_TEST_CHAT_COLORS: &str = "Test chat theme colors (testchatcolors)";
pub const DEBUG_SMOOTH_SCROLLING: &str = "Smooth scrolling";
pub const DEBUG_MEDIA_OVERLAY: &str = "Media decoder diagnostics overlay";
pub const DEBUG_SECRET_CODE_PH: &str = "Run secret code (e.g. debugmode, folders, sounds_reset)";
pub const DEBUG_RUN_BTN: &str = "Run";
pub const DEBUG_LAYOUT_DIAGNOSTICS: &str = "Adaptive Layout Diagnostics";

/// Calls / Speakers & Camera Rustgram copy.
pub const CALLS_TEST_MIC: &str = "Test Microphone";
pub const CALLS_STOP_MIC_TEST: &str = "Stop Microphone Test";
pub const CALLS_REFRESH_DEVICES: &str = "Refresh Devices";
pub const CALLS_VOICE_VIDEO_TITLE: &str = "Voice and video calls";
pub const CALLS_SAME_AS_ABOVE: &str = "Same devices as in settings above";
pub const CALLS_OUTPUT_DEVICE: &str = "Call output device";
pub const CALLS_INPUT_DEVICE: &str = "Call input device";
pub const CALLS_SPEAKER_VOLUME: &str = "Call speaker volume";
pub const CALLS_MIC_GAIN: &str = "Call microphone gain";
pub const CALLS_AUDIO_DUCKING: &str = "Mute other apps during calls (audio ducking)";
pub const CALLS_VIDEO_INPUT: &str = "Video input device";
pub const CALLS_DEFAULT_CAMERA: &str = "Default camera";
pub const CALLS_GROUP_CALLS_TITLE: &str = "Group calls & voice chats";
pub const CALLS_NOISE_SUPPRESSION: &str = "Noise suppression";
pub const CALLS_NOISE_SUBTITLE: &str = "Filter background noise during voice chats";
pub const CALLS_PUSH_TO_TALK: &str = "Push-to-Talk";
pub const CALLS_PTT_SUBTITLE: &str = "Hold a shortcut key to unmute your microphone";
pub const CALLS_PTT_DELAY: &str = "Release delay:";

/// Language section Rustgram copy.
pub const LANG_TRANSLATE_MESSAGES: &str = "Translate Messages";
pub const LANG_INTERFACE_LANGUAGE: &str = "Interface Language";
pub const LANG_RECENT_LANGUAGES: &str = "Recent / Custom Languages";
pub const LANG_SEARCH_PH: &str = "Search language...";
pub const LANG_LINK_PH: &str = "tg://setlanguage?lang=... or https://t.me/setlanguage/...";
pub const LANG_OPEN_LINK_BTN: &str = "Open Link";
pub const LANG_COPY_LINK_BTN: &str = "Copy Link";
pub const LANG_REMOVE_RECENT_BTN: &str = "Remove";
pub const LANG_SUGGESTED_CHANGE_BTN: &str = "Change";
pub const LANG_TEST_PSEUDO_BTN: &str = "Test Pseudo-Locale (#TEST_X)";
pub const LANG_TEST_PSEUDO_TOAST: &str = "Switched tg-lang to #TEST_X pseudo-locale";

/// Scale controls Rustgram copy.
pub const SCALE_DEFAULT_TITLE: &str = "Default interface scale";
pub const SCALE_PREVIEW_SENDER: &str = "Alex Rivera";
