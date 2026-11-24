//! Rustgram-specific fallback strings for statistics, boosts, and monetization views
//! when no matching phrase exists in `resources/lang.strings`.

/// Section tab label for broadcast channel statistics.
pub const TAB_CHANNEL: &str = "Channel";
/// Section tab label for supergroup statistics.
pub const TAB_SUPERGROUP: &str = "Supergroup";
/// Error prefix when a statistics screen fails to load from the backend.
pub const LOAD_FAILED_PREFIX: &str = "Failed to load statistics: ";
/// Fallback display name prefix when a leaderboard user is missing from the bundle.
pub const MEMBER_FALLBACK_PREFIX: &str = "Member #";
/// Notice when clicking a prepaid giveaway row in read-only statistics mode.
pub const GIVEAWAY_READ_ONLY_NOTICE: &str =
    "Open Channel Boosts & Giveaways to launch prepaid giveaways.";
/// Notice when clicking the "Get Boosts" row in statistics view.
pub const GIVEAWAY_CREATE_READ_ONLY_NOTICE: &str =
    "Open Channel Boosts & Giveaways to create a giveaway for this channel.";
/// Toast message when clicking an unclaimed giveaway boost row.
pub const GIVEAWAY_UNCLAIMED_NOTICE: &str = "This giveaway prize has not been claimed yet.";
/// Prefix for the share-link read-only preview toast.
pub const SHARE_LINK_READ_ONLY_PREFIX: &str = "Share link ready: ";
/// Suffix for the share-link read-only preview toast.
pub const SHARE_LINK_READ_ONLY_SUFFIX: &str = " (read-only preview)";
/// Prefix for the gift-code link toast.
pub const GIFT_LINK_PREFIX: &str = "Gift link: ";
/// Prefix for generic boost row toast.
pub const BOOST_LABEL_PREFIX: &str = "Boost ";
/// Currency unit label for Toncoin balances.
pub const TON_UNIT: &str = "TON";
/// Currency unit label for Telegram Stars balances.
pub const STARS_UNIT: &str = "Stars";
/// Withdrawal button label for channel TON balance.
pub const WITHDRAW_TON_FRAGMENT: &str = "Withdraw TON via Fragment";
/// Withdrawal button label for channel Stars balance.
pub const WITHDRAW_STARS_READ_ONLY: &str = "Withdraw Stars via Fragment";
/// Withdrawal button label for bot Stars balance.
pub const WITHDRAW_STARS_FRAGMENT: &str = "Withdraw Stars via Fragment";
/// Notice when channel TON withdrawal is triggered without a live signed-in session.
pub const TON_WITHDRAWAL_READ_ONLY_NOTICE: &str =
    "Sign in to a live Telegram session to request a Fragment TON withdrawal URL.";
/// Notice when channel Stars withdrawal is triggered without a live signed-in session.
pub const STARS_WITHDRAWAL_READ_ONLY_NOTICE: &str =
    "Sign in to a live Telegram session to request a Fragment Stars withdrawal URL.";
/// Notice when bot Stars withdrawal is triggered without a live signed-in session.
pub const BOT_WITHDRAWAL_READ_ONLY_NOTICE: &str =
    "Sign in to a live Telegram session to withdraw bot Stars via Fragment.";
/// Prefix for the next withdrawal window date line.
pub const NEXT_WITHDRAWAL_PREFIX: &str = "Next withdrawal window: ";
/// Read-only status when channel ads are switched off.
pub const ADS_SWITCHED_OFF: &str = "Ads are switched off for this channel";
/// Read-only status when channel ads are enabled.
pub const ADS_ENABLED_READ_ONLY: &str = "Ads are enabled for this channel (read-only)";
/// Inline read-only transaction receipt header.
pub const RECEIPT_TITLE: &str = "Transaction Receipt (Read-Only)";
/// Receipt transaction ID prefix.
pub const RECEIPT_ID_PREFIX: &str = "ID: ";
/// Receipt transaction amount prefix.
pub const RECEIPT_AMOUNT_PREFIX: &str = "Amount: ";
/// Receipt transaction date prefix.
pub const RECEIPT_DATE_PREFIX: &str = "Date: ";
/// Receipt provider/address prefix.
pub const RECEIPT_PROVIDER_PREFIX: &str = "Provider / Address: ";
/// Error when a chart zoom index is out of bounds.
pub const ZOOM_INDEX_OUT_OF_BOUNDS: &str = "Selected point index out of bounds";
/// Error when attempting to zoom a chart with no zoom token.
pub const ZOOM_NO_TOKEN: &str = "Chart has no zoom token";
/// Error when the backend returns an empty zoomed chart.
pub const ZOOM_EMPTY_CHART: &str = "Loaded zoom chart is empty";
/// Heading used in the zoomed charts documentation screenshot.
#[cfg(test)]
pub const ZOOMED_CHARTS_CAPTURE_TITLE: &str = "Zoomed Statistics Charts";
