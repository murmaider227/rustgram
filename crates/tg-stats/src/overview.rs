//! Channel, supergroup, message, boosts, and earn/credits statistics models and overview cards.
//!
//! Port of:
//! - `Telegram/SourceFiles/data/data_statistics.h`
//! - `Telegram/SourceFiles/data/data_statistics_lists.h`
//! - `Telegram/SourceFiles/data/data_boosts.h`
//! - `Telegram/SourceFiles/data/data_channel_earn.h`
//! - `Telegram/SourceFiles/data/data_credits_earn.h`
//! - `Telegram/SourceFiles/info/statistics/info_statistics_inner_widget.cpp` (`FillOverview`)
//! - `Telegram/SourceFiles/info/channel_statistics/boosts/info_boosts_inner_widget.cpp`
//! - `Telegram/SourceFiles/info/channel_statistics/earn/info_channel_earn_list.cpp`

use crate::chart::{ChartViewType, StatisticalGraph};
use crate::color::{ChartPalette, Color};
use crate::format::{
    APPROXIMATELY_SIGN, CreditsAmount, EM_DASH, MINUS_SIGN, credits_to_usd, format_count_to_short,
    format_credits_amount_decimal, lang_day_month_year, major_part_credits, minor_part_credits,
};

/// Current vs previous period metric with computed growth percentage (`Data::StatisticalValue`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatisticalValue {
    /// Current period metric value.
    pub value: f64,
    /// Previous period metric value.
    pub previous_value: f64,
    /// Absolute percentage growth rate `|(current - previous) / previous * 100|`.
    pub growth_rate_percentage: f64,
}

impl StatisticalValue {
    /// Builds a [`StatisticalValue`] from `current` and `previous`, matching
    /// `StatisticalValueFromTL` in `api/api_statistics.cpp:25-36`.
    #[must_use]
    pub fn from_current_and_previous(current: f64, previous: f64) -> Self {
        let growth_rate_percentage = if previous != 0.0 {
            ((current - previous) / previous * 100.0).abs()
        } else {
            0.0
        };
        Self {
            value: current,
            previous_value: previous,
            growth_rate_percentage,
        }
    }
}

/// Broadcast channel statistics (`Data::ChannelStatistics`).
#[derive(Debug, Clone, Default)]
pub struct ChannelStatistics {
    /// Period start timestamp (Unix seconds).
    pub start_date: i64,
    /// Period end timestamp (Unix seconds).
    pub end_date: i64,

    /// Followers count and delta.
    pub member_count: StatisticalValue,
    /// Average views per post and delta.
    pub mean_view_count: StatisticalValue,
    /// Average shares per post and delta.
    pub mean_share_count: StatisticalValue,
    /// Average reactions per post and delta.
    pub mean_reaction_count: StatisticalValue,
    /// Average views per story and delta.
    pub mean_story_view_count: StatisticalValue,
    /// Average shares per story and delta.
    pub mean_story_share_count: StatisticalValue,
    /// Average reactions per story and delta.
    pub mean_story_reaction_count: StatisticalValue,

    /// Enabled notifications percentage (`0.0..=100.0`).
    pub enabled_notifications_percentage: f64,

    /// Growth chart (`Linear`).
    pub member_count_graph: StatisticalGraph,
    /// Followers join/leave chart (`Linear`).
    pub join_graph: StatisticalGraph,
    /// Notifications mute/unmute chart (`Linear`).
    pub mute_graph: StatisticalGraph,
    /// Views by hour chart (`Linear`).
    pub view_count_by_hour_graph: StatisticalGraph,
    /// Views by source chart (`StackBar`).
    pub view_count_by_source_graph: StatisticalGraph,
    /// New followers by source chart (`StackBar`).
    pub join_by_source_graph: StatisticalGraph,
    /// Languages chart (`StackLinear`).
    pub language_graph: StatisticalGraph,
    /// Post interactions chart (`DoubleLinear`).
    pub message_interaction_graph: StatisticalGraph,
    /// Instant View interactions chart (`DoubleLinear`).
    pub instant_view_interaction_graph: StatisticalGraph,
    /// Post reactions by emotion chart (`Bar`).
    pub reactions_by_emotion_graph: StatisticalGraph,
    /// Story interactions chart (`DoubleLinear`).
    pub story_interactions_graph: StatisticalGraph,
    /// Story reactions by emotion chart (`Bar`).
    pub story_reactions_by_emotion_graph: StatisticalGraph,
}

impl ChannelStatistics {
    /// Returns `true` when `start_date == 0 || end_date == 0`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.start_date == 0 || self.end_date == 0
    }

    /// Computes enabled notifications percentage from `(part, total)` clamped to `0.0..=100.0`.
    ///
    /// Port of `ChannelStatisticsFromTL` (`api_statistics.cpp:41-46`).
    #[must_use]
    pub fn compute_enabled_notifications_percentage(part: f64, total: f64) -> f64 {
        if total == 0.0 {
            0.0
        } else {
            (part / total * 100.0).clamp(0.0, 100.0)
        }
    }
}

/// Supergroup / megagroup statistics (`Data::SupergroupStatistics`).
#[derive(Debug, Clone, Default)]
pub struct SupergroupStatistics {
    /// Period start timestamp (Unix seconds).
    pub start_date: i64,
    /// Period end timestamp (Unix seconds).
    pub end_date: i64,

    /// Members count and delta.
    pub member_count: StatisticalValue,
    /// Messages count and delta.
    pub message_count: StatisticalValue,
    /// Viewing members count and delta.
    pub viewer_count: StatisticalValue,
    /// Posting members count and delta.
    pub sender_count: StatisticalValue,

    /// Growth chart (`Linear`).
    pub member_count_graph: StatisticalGraph,
    /// New/left members chart (`Linear`).
    pub join_graph: StatisticalGraph,
    /// New members by source chart (`StackBar`).
    pub join_by_source_graph: StatisticalGraph,
    /// Languages chart (`StackLinear`).
    pub language_graph: StatisticalGraph,
    /// Messages by content type chart (`StackBar`).
    pub message_content_graph: StatisticalGraph,
    /// Member actions chart (`DoubleLinear`).
    pub action_graph: StatisticalGraph,
    /// Top hours chart (`Linear`).
    pub day_graph: StatisticalGraph,
    /// Top days of week chart (`StackLinear`).
    pub week_graph: StatisticalGraph,
}

impl SupergroupStatistics {
    /// Returns `true` when `start_date == 0 || end_date == 0`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.start_date == 0 || self.end_date == 0
    }
}

/// Individual message or story statistics (`Data::MessageStatistics` / `Data::StoryStatistics`).
#[derive(Debug, Clone, Default)]
pub struct MessageStatistics {
    /// Interactions chart (`DoubleLinear`).
    pub message_interaction_graph: StatisticalGraph,
    /// Reactions by emotion chart (`Bar`).
    pub reactions_by_emotion_graph: StatisticalGraph,
    /// Public forwards count (`< 0` hides card).
    pub public_forwards: i32,
    /// Private forwards count (`< 0` hides card).
    pub private_forwards: i32,
    /// Total views count (`< 0` hides card).
    pub views: i32,
    /// Total reactions count (`< 0` hides card).
    pub reactions: i32,
}

impl MessageStatistics {
    /// Returns `true` when neither graph nor views are populated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message_interaction_graph.chart.is_empty() && self.views == 0
    }
}

/// Alias matching `Data::StoryStatistics = MessageStatistics`.
pub type StoryStatistics = MessageStatistics;

/// Recent post or story interaction counter (`Data::StatisticsMessageInteractionInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageInteractionInfo {
    /// Message ID (`0` if story).
    pub message_id: i32,
    /// Story ID (`0` if message).
    pub story_id: i32,
    /// Views count.
    pub views_count: i32,
    /// Forwards count.
    pub forwards_count: i32,
    /// Reactions count.
    pub reactions_count: i32,
}

/// Top supergroup sender row (`Data::StatisticsMessageSenderInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageSenderInfo {
    /// Bare user ID.
    pub user_id: u64,
    /// Number of messages sent.
    pub sent_message_count: i32,
    /// Average characters per message.
    pub average_character_count: i32,
}

/// Top supergroup administrator actions row (`Data::StatisticsAdministratorActionsInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdministratorActionsInfo {
    /// Bare user ID.
    pub user_id: u64,
    /// Deleted messages count.
    pub deleted_message_count: i32,
    /// Banned/kicked users count.
    pub banned_user_count: i32,
    /// Restricted users count.
    pub restricted_user_count: i32,
}

/// Top supergroup inviter row (`Data::StatisticsInviterInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InviterInfo {
    /// Bare user ID.
    pub user_id: u64,
    /// Added members count.
    pub added_member_count: i32,
}

/// Aggregated statistics leaderboard lists (`Data::StatisticsLists`).
#[derive(Debug, Clone, Default)]
pub struct StatisticsLists {
    /// Recent channel posts/stories interactions.
    pub recent_message_interactions: Vec<MessageInteractionInfo>,
    /// Top supergroup posters.
    pub top_senders: Vec<MessageSenderInfo>,
    /// Top supergroup administrators.
    pub top_administrators: Vec<AdministratorActionsInfo>,
    /// Top supergroup inviters.
    pub top_inviters: Vec<InviterInfo>,
}

/// Direction of a period-over-period delta badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaTrend {
    /// Positive growth (`+...`).
    Positive,
    /// Negative decline (`−...`).
    Negative,
    /// Neutral badge (e.g. boosts premium audience `%`).
    Neutral,
}

/// Formatted secondary delta text and color beside an overview value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewDelta {
    /// Formatted string (e.g. `"+120 (12.5%)"`, `"−45 (5%)"`, `"3.4%"`).
    pub text: String,
    /// Trend polarity.
    pub trend: DeltaTrend,
    /// Resolved palette color.
    pub color: Color,
}

/// One metric card inside an overview 2x2 grid (`FillOverview`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewCard {
    /// Primary formatted value (e.g. `"12.3K"`, `"84.25%"`, `"≈42"`).
    pub primary: String,
    /// Optional delta or percentage badge beside `primary`.
    pub delta: Option<OverviewDelta>,
    /// Caption label below `primary` (e.g. `"Followers"`, `"Views per post"`).
    pub caption: String,
}

/// A 2x2 overview section with optional period date-range subtitle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewBlock {
    /// Section title (e.g. `"Overview"`).
    pub title: String,
    /// Formatted date range `"D Mon YYYY — D Mon YYYY"` when dates are set.
    pub date_range: String,
    /// Up to 4 metric cards in reading order (top-left, top-right, bottom-left, bottom-right).
    pub cards: Vec<OverviewCard>,
}

/// Formats a [`StatisticalValue`]'s previous-period delta badge exactly like `parseSecond`
/// in `info/statistics/info_statistics_inner_widget.cpp:347-366`.
#[must_use]
pub fn format_value_delta(v: &StatisticalValue, palette: &ChartPalette) -> Option<OverviewDelta> {
    let diff = v.value - v.previous_value;
    if diff == 0.0 || v.previous_value == 0.0 {
        return None;
    }
    const TOO_MUCH_DIFF: f64 = 1_000_000.0;
    let diff_abs = diff.abs();
    let diff_text = if diff_abs > TOO_MUCH_DIFF {
        format_count_to_short(diff_abs as i64, false).string
    } else if (diff_abs - diff_abs.round()).abs() < 1e-9 {
        (diff_abs.round() as i64).to_string()
    } else {
        format!("{diff_abs}")
    };

    let percentage = v.growth_rate_percentage.abs();
    let pct_text = if (percentage - (percentage as i64 as f64)).abs() < 1e-9 {
        format!("{}", percentage as i64)
    } else {
        format!("{percentage:.1}")
    };

    let (sign, trend, color) = if diff < 0.0 {
        (MINUS_SIGN, DeltaTrend::Negative, palette.delta_negative)
    } else {
        ('+', DeltaTrend::Positive, palette.delta_positive)
    };

    Some(OverviewDelta {
        text: format!("{sign}{diff_text} ({pct_text}%)"),
        trend,
        color,
    })
}

/// Formats an enabled-notifications percentage matching `0.01 * std::round(pct * 100.)` + `"%"`.
#[must_use]
pub fn format_notifications_percentage(pct: f64) -> String {
    let rounded = 0.01 * (pct * 100.0).round();
    if (rounded - rounded.round()).abs() < 1e-9 {
        format!("{}%", rounded.round() as i64)
    } else {
        // Trim trailing zero from two-decimal representation (matches QString::arg(double)).
        let s = format!("{rounded:.2}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        format!("{trimmed}%")
    }
}

fn format_period_range(start_date: i64, end_date: i64) -> String {
    if start_date == 0 || end_date == 0 {
        String::new()
    } else {
        format!(
            "{} {EM_DASH} {}",
            lang_day_month_year(start_date),
            lang_day_month_year(end_date)
        )
    }
}

fn primary_short(v: f64) -> String {
    if v >= 0.0 {
        format_count_to_short(v as i64, false).string
    } else {
        String::new()
    }
}

/// Builds the primary and secondary (shares/reactions) channel overview blocks.
///
/// Port of `FillOverview` (`info_statistics_inner_widget.cpp:327-564`).
#[must_use]
pub fn channel_overview_blocks(
    s: &ChannelStatistics,
    palette: &ChartPalette,
) -> (OverviewBlock, OverviewBlock) {
    let primary = OverviewBlock {
        title: "Overview".to_string(),
        date_range: format_period_range(s.start_date, s.end_date),
        cards: vec![
            OverviewCard {
                primary: primary_short(s.member_count.value),
                delta: format_value_delta(&s.member_count, palette),
                caption: "Followers".to_string(),
            },
            OverviewCard {
                primary: format_notifications_percentage(s.enabled_notifications_percentage),
                delta: None,
                caption: "Enabled notifications".to_string(),
            },
            OverviewCard {
                primary: primary_short(s.mean_view_count.value),
                delta: format_value_delta(&s.mean_view_count, palette),
                caption: "Views per post".to_string(),
            },
            OverviewCard {
                primary: primary_short(s.mean_story_view_count.value),
                delta: format_value_delta(&s.mean_story_view_count, palette),
                caption: "Views per story".to_string(),
            },
        ],
    };

    let has_post_reactions =
        s.mean_reaction_count.value != 0.0 || s.mean_reaction_count.previous_value != 0.0;
    let mut secondary_cards = vec![
        OverviewCard {
            primary: primary_short(s.mean_share_count.value),
            delta: format_value_delta(&s.mean_share_count, palette),
            caption: "Shares per post".to_string(),
        },
        OverviewCard {
            primary: primary_short(s.mean_story_share_count.value),
            delta: format_value_delta(&s.mean_story_share_count, palette),
            caption: "Shares per story".to_string(),
        },
    ];
    if has_post_reactions {
        secondary_cards.push(OverviewCard {
            primary: primary_short(s.mean_reaction_count.value),
            delta: format_value_delta(&s.mean_reaction_count, palette),
            caption: "Reactions per post".to_string(),
        });
        secondary_cards.push(OverviewCard {
            primary: primary_short(s.mean_story_reaction_count.value),
            delta: format_value_delta(&s.mean_story_reaction_count, palette),
            caption: "Reactions per story".to_string(),
        });
    } else {
        secondary_cards.push(OverviewCard {
            primary: primary_short(s.mean_story_reaction_count.value),
            delta: format_value_delta(&s.mean_story_reaction_count, palette),
            caption: "Reactions per story".to_string(),
        });
    }

    let secondary = OverviewBlock {
        title: String::new(),
        date_range: String::new(),
        cards: secondary_cards,
    };

    (primary, secondary)
}

/// Builds the supergroup overview block (`FillOverview` supergroup branch).
#[must_use]
pub fn supergroup_overview_block(
    s: &SupergroupStatistics,
    palette: &ChartPalette,
) -> OverviewBlock {
    OverviewBlock {
        title: "Overview".to_string(),
        date_range: format_period_range(s.start_date, s.end_date),
        cards: vec![
            OverviewCard {
                primary: primary_short(s.member_count.value),
                delta: format_value_delta(&s.member_count, palette),
                caption: "Members".to_string(),
            },
            OverviewCard {
                primary: primary_short(s.message_count.value),
                delta: format_value_delta(&s.message_count, palette),
                caption: "Messages".to_string(),
            },
            OverviewCard {
                primary: primary_short(s.viewer_count.value),
                delta: format_value_delta(&s.viewer_count, palette),
                caption: "Viewing members".to_string(),
            },
            OverviewCard {
                primary: primary_short(s.sender_count.value),
                delta: format_value_delta(&s.sender_count, palette),
                caption: "Posting members".to_string(),
            },
        ],
    }
}

/// Builds the message or story overview block (`FillOverview` message/story branch).
#[must_use]
pub fn message_overview_block(s: &MessageStatistics) -> OverviewBlock {
    let mut cards = Vec::with_capacity(4);
    if s.views >= 0 {
        cards.push(OverviewCard {
            primary: primary_short(f64::from(s.views)),
            delta: None,
            caption: "Views".to_string(),
        });
    }
    if s.public_forwards >= 0 {
        cards.push(OverviewCard {
            primary: primary_short(f64::from(s.public_forwards)),
            delta: None,
            caption: "Public shares".to_string(),
        });
    }
    if s.reactions >= 0 {
        cards.push(OverviewCard {
            primary: primary_short(f64::from(s.reactions)),
            delta: None,
            caption: "Reactions".to_string(),
        });
    }
    if s.private_forwards >= 0 {
        cards.push(OverviewCard {
            primary: primary_short(f64::from(s.private_forwards)),
            delta: None,
            caption: "Private shares".to_string(),
        });
    }
    OverviewBlock {
        title: "Overview".to_string(),
        date_range: String::new(),
        cards,
    }
}

/// Channel/group boosts summary (`Data::BoostsOverview`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BoostsOverview {
    /// `true` for supergroups, `false` for broadcast channels.
    pub group: bool,
    /// Number of boost slots owned by the current user.
    pub mine: i32,
    /// Current boost level.
    pub level: i32,
    /// Total active boosts.
    pub boost_count: i32,
    /// Boosts threshold of the current level.
    pub current_level_boost_count: i32,
    /// Boosts threshold of the next level (`0` if max level).
    pub next_level_boost_count: i32,
    /// Approximate count of Premium members/subscribers.
    pub premium_member_count: i32,
    /// Percentage of Premium members/subscribers (`0.0..=100.0`).
    pub premium_member_percentage: f64,
}

/// Gift code link attached to a boost (`Data::GiftCodeLink`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftCodeLink {
    /// Short display text.
    pub text: String,
    /// Full URL.
    pub link: String,
    /// Gift code slug.
    pub slug: String,
}

/// A single boost entry in the channel/group boosts list (`Data::Boost`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Boost {
    /// Unique boost ID.
    pub id: String,
    /// Boosting user ID (`0` if unclaimed/anonymous).
    pub user_id: u64,
    /// Giveaway message ID (`0` if not from a giveaway).
    pub giveaway_message_id: i32,
    /// Boost creation timestamp (Unix seconds).
    pub date: i64,
    /// Boost expiration timestamp (Unix seconds).
    pub expires_at: i64,
    /// Duration in 30-day months `(expires_at - date) / (30 * 86400)`.
    pub expires_after_months: i32,
    /// Associated gift code link (if gift/giveaway).
    pub gift_code_link: GiftCodeLink,
    /// Boost multiplier (e.g. `4` for giveaway/gift boosts).
    pub multiplier: i32,
    /// Stars amount for Stars giveaways (`0` otherwise).
    pub credits: u64,
    /// Whether this boost came from a Premium gift.
    pub is_gift: bool,
    /// Whether this boost came from a giveaway.
    pub is_giveaway: bool,
    /// Whether the giveaway prize has not yet been claimed.
    pub is_unclaimed: bool,
}

/// Pagination offset for [`BoostsListSlice`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoostsOffsetToken {
    /// Server-provided next page cursor.
    pub next: String,
    /// `true` for the gifts tab, `false` for all boosts.
    pub gifts: bool,
}

/// Paginated slice of channel/group boosts (`Data::BoostsListSlice`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoostsListSlice {
    /// Boost rows in this page.
    pub list: Vec<Boost>,
    /// Total multiplied boost count.
    pub multiplied_total: i32,
    /// `true` once all pages have been loaded.
    pub all_loaded: bool,
    /// Next page token.
    pub token: BoostsOffsetToken,
}

/// Prepaid giveaway awaiting launch (`Data::BoostPrepaidGiveaway`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoostPrepaidGiveaway {
    /// Purchase timestamp (Unix seconds).
    pub date: i64,
    /// Prepaid giveaway ID.
    pub id: u64,
    /// Stars amount (`0` for Telegram Premium subscription giveaway).
    pub credits: u64,
    /// Subscription duration in months (`0` for Stars giveaway).
    pub months: i32,
    /// Number of winners/subscriptions.
    pub quantity: i32,
    /// Number of boosts granted (`0` means `quantity * boosts_per_premium`).
    pub boosts: i32,
}

/// Complete boost status payload (`Data::BoostStatus`).
#[derive(Debug, Clone, Default)]
pub struct BoostStatus {
    /// Overview counters and level progress.
    pub overview: BoostsOverview,
    /// First page of regular boosts.
    pub first_slice_boosts: BoostsListSlice,
    /// First page of gift/giveaway boosts.
    pub first_slice_gifts: BoostsListSlice,
    /// Prepaid giveaways ready to launch.
    pub prepaid_giveaway: Vec<BoostPrepaidGiveaway>,
    /// Shareable boost link (`https://t.me/...`).
    pub link: String,
}

/// Builds the 4 boosts overview cards matching `Info::Boosts::FillOverview`
/// (`info_boosts_inner_widget.cpp:67-172`).
#[must_use]
pub fn boosts_overview_block(overview: &BoostsOverview, palette: &ChartPalette) -> OverviewBlock {
    let approx_prefix = if overview.premium_member_count > 0 {
        APPROXIMATELY_SIGN.to_string()
    } else {
        String::new()
    };
    let premium_pct_delta = if overview.premium_member_percentage != 0.0 {
        let rounded = (overview.premium_member_percentage * 10.0).round().abs() / 10.0;
        let text = if (rounded - rounded.round()).abs() < 1e-9 {
            format!("{}%", rounded.round() as i64)
        } else {
            format!("{rounded:.1}%")
        };
        Some(OverviewDelta {
            text,
            trend: DeltaTrend::Neutral,
            color: palette.window_sub_text_fg,
        })
    } else {
        None
    };

    let boosts_needed = (overview.next_level_boost_count - overview.boost_count).max(0);
    OverviewBlock {
        title: "Overview".to_string(),
        date_range: String::new(),
        cards: vec![
            OverviewCard {
                primary: primary_short(f64::from(overview.level)),
                delta: None,
                caption: "Level".to_string(),
            },
            OverviewCard {
                primary: format!(
                    "{approx_prefix}{}",
                    primary_short(f64::from(overview.premium_member_count))
                ),
                delta: premium_pct_delta,
                caption: if overview.group {
                    "Premium members".to_string()
                } else {
                    "Premium audience".to_string()
                },
            },
            OverviewCard {
                primary: primary_short(f64::from(overview.boost_count)),
                delta: None,
                caption: "Existing boosts".to_string(),
            },
            OverviewCard {
                primary: primary_short(f64::from(boosts_needed)),
                delta: None,
                caption: "Boosts to next level".to_string(),
            },
        ],
    }
}

/// Transaction history entry for TON / Stars monetization (`Data::CreditsHistoryEntry` subset).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EarnHistoryEntry {
    /// Transaction ID.
    pub id: String,
    /// Entry title.
    pub title: String,
    /// Transaction timestamp (Unix seconds).
    pub date: i64,
    /// End date for ad proceeds periods (`0` if N/A).
    pub ads_proceeds_to_date: i64,
    /// Optional recipient/provider identifier.
    pub provider: String,
    /// Currency amount.
    pub credits: CreditsAmount,
    /// `true` for incoming revenue, `false` for withdrawal/spend.
    pub incoming: bool,
    /// Whether the transaction is pending.
    pub pending: bool,
    /// Whether the transaction failed.
    pub failed: bool,
}

/// Paginated slice of monetization transactions (`Data::EarnHistorySlice`).
#[derive(Debug, Clone, Default)]
pub struct EarnHistorySlice {
    /// Transactions in this slice.
    pub list: Vec<EarnHistoryEntry>,
    /// Total reported count.
    pub total: i32,
    /// `true` if no further pages exist.
    pub all_loaded: bool,
    /// Next page offset token.
    pub token: String,
}

/// Channel TON monetization statistics (`Data::EarnStatistics`).
#[derive(Debug, Clone, Default)]
pub struct EarnStatistics {
    /// Top ad impressions hours graph (`ChartViewType::Bar`).
    pub top_hours_graph: StatisticalGraph,
    /// Ad revenue graph (`ChartViewType::StackBar`, with `currency_rate = usd_rate`).
    pub revenue_graph: StatisticalGraph,
    /// Current reward balance.
    pub current_balance: CreditsAmount,
    /// Available withdrawal balance.
    pub available_balance: CreditsAmount,
    /// Lifetime overall revenue.
    pub overall_revenue: CreditsAmount,
    /// Conversion rate from 1 TON to USD.
    pub usd_rate: f64,
    /// Whether sponsored messages are switched off.
    pub switched_off: bool,
    /// First page of revenue transactions.
    pub first_history_slice: EarnHistorySlice,
}

/// Telegram Stars revenue statistics (`Data::CreditsEarnStatistics`).
#[derive(Debug, Clone, Default)]
pub struct CreditsEarnStatistics {
    /// Stars revenue graph (`ChartViewType::StackBar`, with `currency_rate = usd_rate`).
    pub revenue_graph: StatisticalGraph,
    /// Current Stars balance.
    pub current_balance: CreditsAmount,
    /// Available Stars balance for withdrawal.
    pub available_balance: CreditsAmount,
    /// Lifetime overall Stars revenue.
    pub overall_revenue: CreditsAmount,
    /// Conversion rate from 1 Star to USD.
    pub usd_rate: f64,
    /// Whether Stars withdrawal is enabled.
    pub is_withdrawal_enabled: bool,
    /// Earliest timestamp when next withdrawal is allowed (Unix seconds, `0` if none).
    pub next_withdrawal_at: i64,
    /// URL to spend Stars on Telegram Ads.
    pub buy_ads_url: String,
}

/// Formatted balance row in the Earn / Credits overview section (`info_channel_earn_list.cpp:671-808`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarnOverviewRow {
    /// Row caption (`"Available balance"`, `"Current balance"`, `"Total revenue"`).
    pub caption: String,
    /// TON integer part (`MajorPart`).
    pub ton_major: String,
    /// TON fractional part (`MinorPart`, e.g. `".25"`).
    pub ton_minor: String,
    /// TON USD estimate (3 decimals when non-zero, matching `kMinorLength = 3`).
    pub ton_usd: String,
    /// Stars formatted amount (`FormatCreditsAmountDecimal`).
    pub stars_text: String,
    /// Stars USD estimate (2 decimals when non-zero).
    pub stars_usd: String,
}

/// Builds the 3 overview rows (Available, Current, Total) for Earn + Credits monetization views.
///
/// Port of `addOverview` in `info/channel_statistics/earn/info_channel_earn_list.cpp:671-808`.
#[must_use]
pub fn earn_overview_rows(
    ton: &EarnStatistics,
    credits: &CreditsEarnStatistics,
) -> Vec<EarnOverviewRow> {
    const MINOR_USD_LEN: usize = 3;
    let make_row =
        |caption: &str, ton_val: CreditsAmount, credits_val: CreditsAmount| EarnOverviewRow {
            caption: caption.to_string(),
            ton_major: major_part_credits(ton_val),
            ton_minor: minor_part_credits(ton_val),
            ton_usd: if ton_val.is_empty() {
                String::new()
            } else {
                credits_to_usd(ton_val, ton.usd_rate, MINOR_USD_LEN)
            },
            stars_text: format_credits_amount_decimal(credits_val),
            stars_usd: if credits_val.is_empty() {
                String::new()
            } else {
                credits_to_usd(credits_val, credits.usd_rate, 0)
            },
        };

    vec![
        make_row(
            "Available balance",
            ton.available_balance,
            credits.available_balance,
        ),
        make_row(
            "Current balance",
            ton.current_balance,
            credits.current_balance,
        ),
        make_row(
            "Total revenue",
            ton.overall_revenue,
            credits.overall_revenue,
        ),
    ]
}

/// Standard chart view types assigned to channel, supergroup, message, and earn charts.
#[must_use]
pub fn earn_chart_view_type(is_top_hours: bool) -> ChartViewType {
    if is_top_hours {
        ChartViewType::Bar
    } else {
        ChartViewType::StackBar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::CreditsType;

    #[test]
    fn channel_overview_formats_deltas_and_notifications_percentage() {
        let stats = ChannelStatistics {
            start_date: 1_700_006_400, // 15 Nov 2023
            end_date: 1_700_611_200,   // 22 Nov 2023
            member_count: StatisticalValue::from_current_and_previous(15_200.0, 14_000.0),
            mean_view_count: StatisticalValue::from_current_and_previous(4_500.0, 5_000.0),
            mean_share_count: StatisticalValue::from_current_and_previous(120.0, 100.0),
            mean_reaction_count: StatisticalValue::from_current_and_previous(85.0, 85.0),
            mean_story_view_count: StatisticalValue::from_current_and_previous(2_100.0, 1_500.0),
            mean_story_share_count: StatisticalValue::from_current_and_previous(30.0, 20.0),
            mean_story_reaction_count: StatisticalValue::from_current_and_previous(50.0, 40.0),
            enabled_notifications_percentage:
                ChannelStatistics::compute_enabled_notifications_percentage(842.0, 1000.0),
            ..Default::default()
        };

        let palette = ChartPalette::light();
        let (primary, secondary) = channel_overview_blocks(&stats, &palette);
        assert_eq!(primary.date_range, "15 Nov 2023 \u{2014} 22 Nov 2023");
        assert_eq!(primary.cards[0].primary, "15.2K");
        assert_eq!(
            primary.cards[0].delta.as_ref().unwrap().text,
            "+1200 (8.6%)"
        );
        assert_eq!(
            primary.cards[0].delta.as_ref().unwrap().trend,
            DeltaTrend::Positive
        );

        assert_eq!(primary.cards[1].primary, "84.2%");
        assert!(primary.cards[1].delta.is_none());

        assert_eq!(primary.cards[2].primary, "4500");
        assert_eq!(
            primary.cards[2].delta.as_ref().unwrap().text,
            "\u{2212}500 (10%)"
        );
        assert_eq!(
            primary.cards[2].delta.as_ref().unwrap().trend,
            DeltaTrend::Negative
        );

        assert_eq!(secondary.cards.len(), 4);
        assert_eq!(secondary.cards[0].caption, "Shares per post");
        assert!(secondary.cards[2].delta.is_none()); // 85 vs 85 -> zero diff
    }

    #[test]
    fn boosts_and_earn_overviews_format_accurately() {
        let palette = ChartPalette::light();
        let boosts = BoostsOverview {
            group: false,
            mine: 1,
            level: 4,
            boost_count: 17,
            current_level_boost_count: 15,
            next_level_boost_count: 25,
            premium_member_count: 340,
            premium_member_percentage: 12.34,
        };
        let block = boosts_overview_block(&boosts, &palette);
        assert_eq!(block.cards[0].primary, "4");
        assert_eq!(block.cards[1].primary, "\u{2248}340");
        assert_eq!(block.cards[1].delta.as_ref().unwrap().text, "12.3%");
        assert_eq!(block.cards[2].primary, "17");
        assert_eq!(block.cards[3].primary, "8");

        let ton = EarnStatistics {
            available_balance: CreditsAmount::from_nanoton(12_500_000_000),
            current_balance: CreditsAmount::from_nanoton(15_000_000_000),
            overall_revenue: CreditsAmount::from_nanoton(100_000_000_000),
            usd_rate: 5.0,
            ..Default::default()
        };
        let credits = CreditsEarnStatistics {
            available_balance: CreditsAmount::from_whole(1_000, CreditsType::Stars),
            current_balance: CreditsAmount::from_whole(1_500, CreditsType::Stars),
            overall_revenue: CreditsAmount::from_whole(5_000, CreditsType::Stars),
            usd_rate: 0.013,
            ..Default::default()
        };
        let rows = earn_overview_rows(&ton, &credits);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].ton_major, "12");
        assert_eq!(rows[0].ton_minor, ".50");
        assert_eq!(rows[0].ton_usd, "\u{2248}$62.500");
        assert_eq!(rows[0].stars_text, "1,000");
        assert_eq!(rows[0].stars_usd, "\u{2248}$13.00");
    }
}
