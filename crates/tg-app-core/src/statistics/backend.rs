//! Statistics, Boosts, and Monetization backend trait and UI bundle DTOs.
//!
//! Port origin: `Telegram/SourceFiles/api/api_statistics.{h,cpp}`,
//! `Telegram/SourceFiles/api/api_earn.{h,cpp}`, and
//! `Telegram/SourceFiles/api/api_credits.{h,cpp}`.

use std::collections::BTreeMap;

use tg_stats::{
    BoostStatus, BoostsListSlice, BoostsOffsetToken, ChannelStatistics, CreditsEarnStatistics,
    EarnHistorySlice, EarnStatistics, MessageInteractionInfo, MessageStatistics, StatisticalGraph,
    StatisticsLists, SupergroupStatistics,
};

/// Display metadata for a peer (channel, group, bot, or member) referenced in statistics lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDisplayInfo {
    /// Bare numeric peer/user identifier.
    pub id: u64,
    /// Display title or name (fictional in mock data).
    pub title: String,
    /// Optional handle or secondary descriptor (e.g. `"@signal_digest"`).
    pub subtitle: String,
    /// 1-2 character avatar monogram.
    pub initials: String,
    /// Avatar background RGB color.
    pub avatar_rgb: [u8; 3],
}

/// Preview metadata for a recent channel post or story row.
///
/// Mirrors the item metadata joined with `Data::StatisticsMessageInteractionInfo` in
/// `info/statistics/info_statistics_recent_message.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPostPreview {
    /// Interaction counters (views, forwards, reactions, message/story ID).
    pub interaction: MessageInteractionInfo,
    /// `true` for a channel story, `false` for a channel post.
    pub is_story: bool,
    /// Short preview snippet text.
    pub snippet: String,
    /// Publication timestamp (Unix seconds).
    pub date_unix: i64,
    /// Author or channel display title.
    pub author_title: String,
    /// Avatar monogram for the thumbnail placeholder.
    pub initials: String,
    /// Thumbnail / avatar fill RGB color.
    pub avatar_rgb: [u8; 3],
}

/// Public forward row shown beneath individual message or story statistics.
///
/// Mirrors `PublicForwardsController` in `info/statistics/info_statistics_list_controllers.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicForwardRow {
    /// Forwarding channel or group metadata.
    pub peer: PeerDisplayInfo,
    /// Forwarded message ID (`0` if story).
    pub message_id: i32,
    /// Forwarded story ID (`0` if message).
    pub story_id: i32,
    /// Subscriber/member count of the forwarding peer.
    pub subscribers: i32,
    /// Views count on the forwarded copy.
    pub views: i32,
}

/// Complete channel statistics screen payload.
#[derive(Debug, Clone)]
pub struct ChannelStatsBundle {
    /// Target channel display info.
    pub peer: PeerDisplayInfo,
    /// Overview metrics and channel charts (`tg_stats::ChannelStatistics`).
    pub stats: ChannelStatistics,
    /// Recent posts and stories with interaction counters.
    pub recent_posts: Vec<RecentPostPreview>,
}

/// Complete supergroup statistics screen payload.
#[derive(Debug, Clone)]
pub struct SupergroupStatsBundle {
    /// Target supergroup display info.
    pub peer: PeerDisplayInfo,
    /// Overview metrics and supergroup charts (`tg_stats::SupergroupStatistics`).
    pub stats: SupergroupStatistics,
    /// Top senders, administrators, and inviters leaderboards.
    pub lists: StatisticsLists,
    /// Display lookup table keyed by bare `user_id`.
    pub users: BTreeMap<u64, PeerDisplayInfo>,
}

/// Complete message or story statistics screen payload.
#[derive(Debug, Clone)]
pub struct MessageStatsBundle {
    /// Owning channel display info.
    pub peer: PeerDisplayInfo,
    /// Post or story preview metadata.
    pub post: RecentPostPreview,
    /// Overview counters and interaction/reaction charts.
    pub stats: MessageStatistics,
    /// Channels that publicly forwarded this post or story.
    pub public_forwards: Vec<PublicForwardRow>,
    /// Pagination offset token for loading more public forwards (empty when all loaded).
    pub public_forwards_next_offset: String,
}

/// Complete boosts screen payload.
#[derive(Debug, Clone)]
pub struct BoostStatusBundle {
    /// Channel or supergroup display info.
    pub peer: PeerDisplayInfo,
    /// Boost overview, slices, prepaid giveaways, and shareable link.
    pub status: BoostStatus,
    /// Display lookup table for boosting users keyed by bare `user_id`.
    pub users: BTreeMap<u64, PeerDisplayInfo>,
    /// Boosts granted per Premium subscription in a giveaway (`giveawayBoostsPerPremium`, default 4).
    pub giveaway_boosts_per_premium: i32,
}

/// Complete channel monetization (TON + Stars) screen payload.
#[derive(Debug, Clone)]
pub struct ChannelEarnBundle {
    /// Target channel display info.
    pub peer: PeerDisplayInfo,
    /// TON ad revenue statistics and first history slice.
    pub ton: EarnStatistics,
    /// Telegram Stars revenue statistics.
    pub credits: CreditsEarnStatistics,
    /// First page of channel Stars transaction history.
    pub credits_history: EarnHistorySlice,
    /// Whether TON ad revenue section is visible.
    pub can_view_currency: bool,
    /// Whether Stars revenue section is visible.
    pub can_view_credits: bool,
    /// Whether TON withdrawal is enabled in app config.
    pub withdrawal_enabled: bool,
    /// Minimum boost level required to disable sponsored ads.
    pub restrict_sponsored_level_min: i32,
    /// Current channel boost level hint.
    pub current_level: i32,
}

/// Complete bot Stars monetization screen payload.
#[derive(Debug, Clone)]
pub struct BotEarnBundle {
    /// Target bot display info.
    pub peer: PeerDisplayInfo,
    /// Bot Stars revenue statistics.
    pub credits: CreditsEarnStatistics,
    /// Full Stars transaction history slice.
    pub full_history: EarnHistorySlice,
    /// Incoming-only Stars transaction history slice.
    pub in_history: EarnHistorySlice,
    /// Outgoing-only Stars transaction history slice.
    pub out_history: EarnHistorySlice,
    /// Whether the direct Affiliate / StarRef program button is allowed (`BotStarRef::Join::Allowed`).
    pub affiliate_allowed: bool,
}

/// Data provider abstraction for all statistics, boosts, and monetization views.
///
/// Decouples the `egui` statistics UI from live MTProto / `tg-api` transport so
/// screens can be exercised against [`crate::statistics::mock::MockStatsBackend`]
/// or wired to real Telegram API requests later.
pub trait StatsBackend: Send + Sync {
    /// Loads broadcast channel statistics and recent posts/stories.
    fn channel_statistics(&self) -> Result<ChannelStatsBundle, String>;

    /// Loads supergroup statistics and member/admin/inviter leaderboards.
    fn supergroup_statistics(&self) -> Result<SupergroupStatsBundle, String>;

    /// Loads statistics for an individual channel message by `message_id`.
    fn message_statistics(&self, message_id: i32) -> Result<MessageStatsBundle, String>;

    /// Loads statistics for an individual channel story by `story_id`.
    fn story_statistics(&self, story_id: i32) -> Result<MessageStatsBundle, String>;

    /// Loads an async or zoomed-in detail graph (`stats.loadAsyncGraph(token, x)`).
    fn load_zoom_graph(
        &self,
        zoom_token: &str,
        x_timestamp: f64,
    ) -> Result<StatisticalGraph, String>;

    /// Loads channel/group boosts overview, first boost/gift slices, giveaways, and boost link.
    fn boost_status(&self) -> Result<BoostStatusBundle, String>;

    /// Loads the next paginated slice of boosts or gifts for `token`.
    fn load_more_boosts(&self, token: &BoostsOffsetToken) -> Result<BoostsListSlice, String>;

    /// Loads channel TON + Stars monetization overview, charts, and transaction history.
    fn channel_earn(&self) -> Result<ChannelEarnBundle, String>;

    /// Loads the next paginated slice of channel TON earn history.
    fn load_more_channel_earn_history(&self, token: &str) -> Result<EarnHistorySlice, String>;

    /// Loads bot Stars monetization overview, chart, and filtered history slices.
    fn bot_earn(&self) -> Result<BotEarnBundle, String>;

    /// Loads an un-hydrated async graph token via `stats.loadAsyncGraph(token, x = None)`.
    fn load_async_graph(&self, token: &str) -> Result<StatisticalGraph, String> {
        self.load_zoom_graph(token, 0.0)
    }

    /// Loads the next paginated slice of public forwards for a message or story
    /// (`stats.getMessagePublicForwards` / `stats.getStoryPublicForwards`).
    fn load_more_public_forwards(
        &self,
        _message_id: i32,
        _is_story: bool,
        _offset: &str,
    ) -> Result<(Vec<PublicForwardRow>, String), String> {
        Ok((Vec::new(), String::new()))
    }

    /// Whether this backend supports live 2FA revenue withdrawal URL generation.
    fn supports_live_withdrawal(&self) -> bool {
        false
    }

    /// Requests a Fragment withdrawal URL (`payments.getStarsRevenueWithdrawalUrl` with 2FA SRP).
    fn request_revenue_withdrawal_url(
        &self,
        _is_bot: bool,
        _ton: bool,
        _amount: Option<i64>,
        _password: &str,
    ) -> Result<String, String> {
        Err("Revenue withdrawals are read-only in demo mode.".to_owned())
    }

    /// Toggles sponsored messages restriction on the target channel
    /// (`channels.restrictSponsoredMessages`, `api_earn.cpp`).
    fn restrict_sponsored_messages(&self, _restricted: bool) -> Result<(), String> {
        Err("Sponsored messages toggle is not supported by this backend.".to_owned())
    }
}
