//! Rustgram-specific fallback strings for Instant View and Bot Mini App windows
//! when no matching phrase exists in `resources/lang.strings`.

/// Tooltip for Instant View overflow menu button.
pub const INSTANT_VIEW_MENU_TOOLTIP: &str = "Instant View menu";
/// Tooltip for Instant View search toggle button.
pub const FIND_IN_PAGE_TOOLTIP: &str = "Find in page (Ctrl+F)";
/// Instant View menu item for opening in-page search.
pub const FIND_IN_PAGE: &str = "Find in Page";
/// Tooltip for previous search match button.
pub const PREV_MATCH_TOOLTIP: &str = "Previous match (Shift+Enter)";
/// Tooltip for next search match button.
pub const NEXT_MATCH_TOOLTIP: &str = "Next match (Enter)";
/// Tooltip for closing the search bar.
pub const CLOSE_SEARCH_TOOLTIP: &str = "Close search (Esc)";
/// Tooltip for browser-style back navigation button.
pub const BACK_TOOLTIP: &str = "Back";
/// Tooltip for browser-style forward navigation button.
pub const FORWARD_TOOLTIP: &str = "Forward";
/// Tooltip for resetting Instant View zoom to 100%.
pub const RESET_ZOOM_TOOLTIP: &str = "Reset zoom (Ctrl+0)";
/// Instant View menu item for zooming in.
pub const ZOOM_IN: &str = "Zoom In";
/// Instant View menu item for zooming out.
pub const ZOOM_OUT: &str = "Zoom Out";
/// Instant View menu item prefix for resetting zoom.
pub const RESET_ZOOM_PREFIX: &str = "Reset Zoom";
/// Toast after opening an Instant View article externally.
pub const OPENED_IN_BROWSER: &str = "Opened in browser";
/// Toast after copying selected text from Instant View.
pub const SELECTION_COPIED: &str = "Selection copied";
/// Toast after triggering Instant View share.
pub const SHARE_LINK_READY: &str = "Share link ready";
/// Toast after joining a channel from an Instant View embed.
pub const JOINED_CHANNEL: &str = "Joined channel";
/// Toast prefix when opening media from Instant View.
pub const OPENING_MEDIA_PREFIX: &str = "Opening ";
/// Toast suffix when opening media from Instant View.
pub const OPENING_MEDIA_SUFFIX: &str = " in Media Viewer";
/// OS window title suffix for Bot Mini App panels.
pub const MINI_APP_TITLE_SUFFIX: &str = "Mini App";
/// Toast shown while reloading a Bot Mini App.
pub const RELOADING_MINI_APP: &str = "Reloading Mini App…";
/// Toast prefix when sharing a Bot Mini App link.
pub const SHARING_PREFIX: &str = "Sharing @";
/// Menu item to enter fullscreen in a Bot Mini App.
pub const FULLSCREEN: &str = "Fullscreen";
/// Menu item to exit fullscreen in a Bot Mini App.
pub const EXIT_FULLSCREEN: &str = "Exit Fullscreen";
/// Modal title for phone number share confirmation.
pub const SHARE_PHONE_TITLE: &str = "Share Phone Number?";
/// Modal title when QR scanning is unsupported on desktop.
pub const QR_SCANNER_UNAVAILABLE_TITLE: &str = "QR Scanner Unavailable";
/// Modal body prompt for opening an external link.
pub const CONFIRM_EXTERNAL_LINK_BODY: &str = "Do you want to open this external link?";
/// Modal title when WebView2 Runtime is missing.
pub const WEBVIEW2_REQUIRED_TITLE: &str = "WebView2 Runtime Required";
/// Modal title when embedded WebView cannot initialize.
pub const WEBVIEW_UNAVAILABLE_TITLE: &str = "WebView Unavailable";
/// Modal title when embedded WebView crashes.
pub const WEBVIEW_CRASHED_TITLE: &str = "WebView Crashed";
/// Modal body when WebView2 Runtime is missing.
pub const WEBVIEW2_MISSING_BODY: &str = "Microsoft Edge WebView2 Runtime is required to display this page inside Rustgram. Install the Evergreen WebView2 Runtime from Microsoft and try again.";
/// Modal body prefix when WebView2 Runtime is missing with details.
pub const WEBVIEW2_MISSING_BODY_PREFIX: &str =
    "Microsoft Edge WebView2 Runtime is required to display this page inside Rustgram.";
/// Button label to download Microsoft Edge WebView2 Runtime.
pub const DOWNLOAD_WEBVIEW2: &str = "Download WebView2";
/// Caption above the official Microsoft WebView2 download URL.
pub const OFFICIAL_DOWNLOAD_LINK: &str = "Official Microsoft download link:";
/// Prefix above fallback page URL in runtime error modal.
pub const PAGE_URL_PREFIX: &str = "Page URL: ";
/// Error detail when a native parent HWND is unavailable.
pub const PARENT_HANDLE_UNAVAILABLE: &str = "Parent window handle is unavailable on this platform.";
/// Toast when sending data payload to a bot.
pub const SENT_BYTES_PREFIX: &str = "Sent ";
/// Toast middle when sending data payload to a bot.
pub const SENT_BYTES_TO: &str = " bytes to @";
