//! Live Telegram Statistics, Boosts, and Monetization backend (`StatsBackend` for `LiveSession`).
//!
//! Port origin: `Telegram/SourceFiles/api/api_statistics.cpp`,
//! `Telegram/SourceFiles/api/api_statistics_data_deserialize.cpp`,
//! `Telegram/SourceFiles/api/api_credits.cpp`, and
//! `Telegram/SourceFiles/api/api_credits_history_entry.cpp`.
//!
//! Implements [`StatsBackend`] over real MTProto RPCs:
//! - `stats.getBroadcastStats`
//! - `stats.getMegagroupStats`
//! - `stats.getMessageStats` / `stats.getMessagePublicForwards` / `channels.getMessages`
//! - `stats.getStoryStats` / `stats.getStoryPublicForwards` / `stories.getStoriesByID`
//! - `stats.loadAsyncGraph` (async graph hydration & remote zoom)
//! - `premium.getBoostsStatus` / `premium.getBoostsList`
//! - `payments.getStarsRevenueStats` (TON `ton: true` — Layer 233 successor to
//!   `stats.getBroadcastRevenueStats` — and Stars `ton: false`) + `payments.getStarsTransactions`
//!   + `payments.getStarsRevenueAdsAccountUrl`

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for};
use tg_data::{
    ChannelFlags, FullMsgId as DataFullMsgId, MsgId as DataMsgId, Peer, PeerId as DataPeerId,
    Session, UserId as DataUserId,
};
use tg_stats::{
    AdministratorActionsInfo, Boost, BoostPrepaidGiveaway, BoostStatus, BoostsListSlice,
    BoostsOffsetToken, BoostsOverview, ChannelStatistics, CreditsAmount, CreditsEarnStatistics,
    CreditsType, EarnHistoryEntry, EarnHistorySlice, EarnStatistics, GiftCodeLink, InviterInfo,
    MessageInteractionInfo, MessageSenderInfo, MessageStatistics, StatisticalChart,
    StatisticalGraph, StatisticalValue, StatisticsLists, SupergroupStatistics,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::RwLock;
use tracing::debug;

use crate::convert::{
    avatar_rgb_for_color_index, initials_from_name, known_peer_ids, peer_display_title,
    peer_primary_username, to_data_peer,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::stand_in::types::PeerId;
use tg_app_core::statistics::backend::{
    BoostStatusBundle, BotEarnBundle, ChannelEarnBundle, ChannelStatsBundle, MessageStatsBundle,
    PeerDisplayInfo, PublicForwardRow, RecentPostPreview, StatsBackend, SupergroupStatsBundle,
};

/// Default timeout for synchronous statistics RPCs triggered from UI trait methods.
const STATS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// First slice size matching `kFirstSlice` in `api_statistics.cpp`.
const BOOSTS_FIRST_SLICE: i32 = 10;
/// Subsequent slice size matching `kLimit` in `api_statistics.cpp`.
const BOOSTS_PAGE_LIMIT: i32 = 40;
/// Default transaction history page size for initial load.
const EARN_FIRST_SLICE: i32 = 10;
/// Subsequent transaction history page size.
const EARN_PAGE_LIMIT: i32 = 40;
/// Public forwards page size matching `kLimit` in `api_statistics.cpp`.
const PUBLIC_FORWARDS_LIMIT: i32 = 100;
/// Default boosts granted per Premium subscription in a giveaway.
const DEFAULT_GIVEAWAY_BOOSTS_PER_PREMIUM: i32 = 4;
/// Default minimum channel boost level required to disable sponsored ads.
const DEFAULT_RESTRICT_SPONSORED_LEVEL_MIN: i32 = 50;

/// Per-session mutable statistics routing state (selected peer and stats DC overrides).
#[derive(Default)]
struct LiveStatsState {
    target_peer: Option<DataPeerId>,
    stats_dc_by_peer: HashMap<DataPeerId, i32>,
    last_stats_dc: Option<i32>,
}

struct StatsRegistryEntry {
    session_weak: Weak<RwLock<Session>>,
    state: Arc<Mutex<LiveStatsState>>,
}

fn stats_registry() -> &'static Mutex<HashMap<usize, StatsRegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, StatsRegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stats_state_for(live: &LiveSession) -> Arc<Mutex<LiveStatsState>> {
    let session_arc = live.runtime().session();
    let key = Arc::as_ptr(session_arc) as usize;
    let session_weak = Arc::downgrade(session_arc);

    let mut map = stats_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|_, entry| entry.session_weak.strong_count() > 0);
    if let Some(entry) = map.get(&key)
        && entry.session_weak.ptr_eq(&session_weak)
    {
        return Arc::clone(&entry.state);
    }
    let created = Arc::new(Mutex::new(LiveStatsState::default()));
    map.insert(
        key,
        StatsRegistryEntry {
            session_weak,
            state: Arc::clone(&created),
        },
    );
    created
}

// =============================================================================
// Pure TL -> tg-stats / tg-app conversions
// =============================================================================

/// Converts a TL `StatsAbsValueAndPrev` into `tg_stats::StatisticalValue`
/// (`StatisticalValueFromTL` in `api_statistics.cpp:25-36`).
#[must_use]
pub fn statistical_value_from_tl(tl: &tl_enums::StatsAbsValueAndPrev) -> StatisticalValue {
    let tl_enums::StatsAbsValueAndPrev::StatsAbsValueAndPrev(d) = tl;
    StatisticalValue::from_current_and_previous(d.current, d.previous)
}

/// Converts a TL `StatsGraph` (`statsGraph`, `statsGraphAsync`, or `statsGraphError`)
/// into `tg_stats::StatisticalGraph` (`StatisticalGraphFromTL` in `api_statistics_data_deserialize.cpp`).
#[must_use]
pub fn statistical_graph_from_tl(tl: &tl_enums::StatsGraph) -> StatisticalGraph {
    match tl {
        tl_enums::StatsGraph::StatsGraph(d) => {
            let tl_enums::DataJSON::DataJSON(json) = &d.json;
            let zoom_token = d.zoom_token.clone().unwrap_or_default();
            match StatisticalChart::from_json_str(&json.data) {
                Ok(chart) => StatisticalGraph {
                    chart,
                    zoom_token,
                    error: String::new(),
                },
                Err(err) => StatisticalGraph {
                    chart: StatisticalChart::default(),
                    zoom_token: String::new(),
                    error: err.to_string(),
                },
            }
        }
        tl_enums::StatsGraph::Async(a) => StatisticalGraph {
            chart: StatisticalChart::default(),
            zoom_token: a.token.clone(),
            error: String::new(),
        },
        tl_enums::StatsGraph::Error(e) => StatisticalGraph {
            chart: StatisticalChart::default(),
            zoom_token: String::new(),
            error: e.error.clone(),
        },
    }
}

/// Converts a TL `StarsAmount` (`starsAmount` or `starsTonAmount`) into `tg_stats::CreditsAmount`
/// (`CreditsAmountFromTL`).
#[must_use]
pub fn credits_amount_from_tl(tl: &tl_enums::StarsAmount) -> CreditsAmount {
    match tl {
        tl_enums::StarsAmount::StarsAmount(s) => {
            CreditsAmount::new(s.amount, i64::from(s.nanos), CreditsType::Stars)
        }
        tl_enums::StarsAmount::StarsTonAmount(t) => CreditsAmount::from_nanoton(t.amount),
    }
}

/// Converts a TL `stats.BroadcastStats` into `(ChannelStatistics, Vec<MessageInteractionInfo>)`.
#[must_use]
pub fn broadcast_stats_from_tl(
    data: &tl_types::stats::BroadcastStats,
) -> (ChannelStatistics, Vec<MessageInteractionInfo>) {
    let tl_enums::StatsDateRangeDays::StatsDateRangeDays(period) = &data.period;
    let tl_enums::StatsPercentValue::StatsPercentValue(unmuted) = &data.enabled_notifications;
    let enabled_notifications_percentage =
        ChannelStatistics::compute_enabled_notifications_percentage(unmuted.part, unmuted.total);

    let stats = ChannelStatistics {
        start_date: i64::from(period.min_date),
        end_date: i64::from(period.max_date),
        member_count: statistical_value_from_tl(&data.followers),
        mean_view_count: statistical_value_from_tl(&data.views_per_post),
        mean_share_count: statistical_value_from_tl(&data.shares_per_post),
        mean_reaction_count: statistical_value_from_tl(&data.reactions_per_post),
        mean_story_view_count: statistical_value_from_tl(&data.views_per_story),
        mean_story_share_count: statistical_value_from_tl(&data.shares_per_story),
        mean_story_reaction_count: statistical_value_from_tl(&data.reactions_per_story),
        enabled_notifications_percentage,
        member_count_graph: statistical_graph_from_tl(&data.growth_graph),
        join_graph: statistical_graph_from_tl(&data.followers_graph),
        mute_graph: statistical_graph_from_tl(&data.mute_graph),
        view_count_by_hour_graph: statistical_graph_from_tl(&data.top_hours_graph),
        view_count_by_source_graph: statistical_graph_from_tl(&data.views_by_source_graph),
        join_by_source_graph: statistical_graph_from_tl(&data.new_followers_by_source_graph),
        language_graph: statistical_graph_from_tl(&data.languages_graph),
        message_interaction_graph: statistical_graph_from_tl(&data.interactions_graph),
        instant_view_interaction_graph: statistical_graph_from_tl(&data.iv_interactions_graph),
        reactions_by_emotion_graph: statistical_graph_from_tl(&data.reactions_by_emotion_graph),
        story_interactions_graph: statistical_graph_from_tl(&data.story_interactions_graph),
        story_reactions_by_emotion_graph: statistical_graph_from_tl(
            &data.story_reactions_by_emotion_graph,
        ),
    };

    let recent = data
        .recent_posts_interactions
        .iter()
        .map(|tl| match tl {
            tl_enums::PostInteractionCounters::Message(m) => MessageInteractionInfo {
                message_id: m.msg_id,
                story_id: 0,
                views_count: m.views,
                forwards_count: m.forwards,
                reactions_count: m.reactions,
            },
            tl_enums::PostInteractionCounters::Story(s) => MessageInteractionInfo {
                message_id: 0,
                story_id: s.story_id,
                views_count: s.views,
                forwards_count: s.forwards,
                reactions_count: s.reactions,
            },
        })
        .collect();

    (stats, recent)
}

/// Converts a TL `stats.MegagroupStats` into `(SupergroupStatistics, StatisticsLists)`.
#[must_use]
pub fn megagroup_stats_from_tl(
    data: &tl_types::stats::MegagroupStats,
) -> (SupergroupStatistics, StatisticsLists) {
    let tl_enums::StatsDateRangeDays::StatsDateRangeDays(period) = &data.period;
    let stats = SupergroupStatistics {
        start_date: i64::from(period.min_date),
        end_date: i64::from(period.max_date),
        member_count: statistical_value_from_tl(&data.members),
        message_count: statistical_value_from_tl(&data.messages),
        viewer_count: statistical_value_from_tl(&data.viewers),
        sender_count: statistical_value_from_tl(&data.posters),
        member_count_graph: statistical_graph_from_tl(&data.growth_graph),
        join_graph: statistical_graph_from_tl(&data.members_graph),
        join_by_source_graph: statistical_graph_from_tl(&data.new_members_by_source_graph),
        language_graph: statistical_graph_from_tl(&data.languages_graph),
        message_content_graph: statistical_graph_from_tl(&data.messages_graph),
        action_graph: statistical_graph_from_tl(&data.actions_graph),
        day_graph: statistical_graph_from_tl(&data.top_hours_graph),
        week_graph: statistical_graph_from_tl(&data.weekdays_graph),
    };

    let lists = StatisticsLists {
        recent_message_interactions: Vec::new(),
        top_senders: data
            .top_posters
            .iter()
            .map(|tl| {
                let tl_enums::StatsGroupTopPoster::StatsGroupTopPoster(p) = tl;
                MessageSenderInfo {
                    user_id: p.user_id.max(0) as u64,
                    sent_message_count: p.messages,
                    average_character_count: p.avg_chars,
                }
            })
            .collect(),
        top_administrators: data
            .top_admins
            .iter()
            .map(|tl| {
                let tl_enums::StatsGroupTopAdmin::StatsGroupTopAdmin(a) = tl;
                AdministratorActionsInfo {
                    user_id: a.user_id.max(0) as u64,
                    deleted_message_count: a.deleted,
                    // Faithful to C++ api_statistics.cpp:190-191: kicked -> banned, banned -> restricted.
                    banned_user_count: a.kicked,
                    restricted_user_count: a.banned,
                }
            })
            .collect(),
        top_inviters: data
            .top_inviters
            .iter()
            .map(|tl| {
                let tl_enums::StatsGroupTopInviter::StatsGroupTopInviter(inv) = tl;
                InviterInfo {
                    user_id: inv.user_id.max(0) as u64,
                    added_member_count: inv.invitations,
                }
            })
            .collect(),
    };

    (stats, lists)
}

/// Converts a TL `premium.BoostsStatus` into `tg_stats::BoostStatus` (overview, link,
/// and prepaid giveaways; callers populate `first_slice_boosts` and `first_slice_gifts`).
#[must_use]
pub fn boost_status_from_tl(
    data: &tl_types::premium::BoostsStatus,
    is_megagroup: bool,
) -> BoostStatus {
    let (premium_member_count, premium_member_percentage) =
        if let Some(tl_enums::StatsPercentValue::StatsPercentValue(pa)) = &data.premium_audience {
            let premium_count = (pa.part as i32).max(0);
            let participant_count = (pa.total as i32).max(premium_count);
            let pct = if participant_count > 0 {
                100.0 * f64::from(premium_count) / f64::from(participant_count)
            } else {
                0.0
            };
            (premium_count, pct)
        } else {
            (0, 0.0)
        };

    let overview = BoostsOverview {
        group: is_megagroup,
        mine: data
            .my_boost_slots
            .as_ref()
            .map_or(0, |slots| slots.len() as i32),
        level: data.level.max(0),
        boost_count: data.boosts.max(data.current_level_boosts),
        current_level_boost_count: data.current_level_boosts,
        next_level_boost_count: data.next_level_boosts.unwrap_or(0),
        premium_member_count,
        premium_member_percentage,
    };

    let prepaid_giveaway = data
        .prepaid_giveaways
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|g| match g {
            tl_enums::PrepaidGiveaway::PrepaidGiveaway(pg) => BoostPrepaidGiveaway {
                date: i64::from(pg.date),
                id: pg.id as u64,
                credits: 0,
                months: pg.months,
                quantity: pg.quantity,
                boosts: 0,
            },
            tl_enums::PrepaidGiveaway::PrepaidStarsGiveaway(sg) => BoostPrepaidGiveaway {
                date: i64::from(sg.date),
                id: sg.id as u64,
                credits: sg.stars.max(0) as u64,
                months: 0,
                quantity: sg.quantity,
                boosts: sg.boosts,
            },
        })
        .collect();

    BoostStatus {
        overview,
        first_slice_boosts: BoostsListSlice::default(),
        first_slice_gifts: BoostsListSlice::default(),
        prepaid_giveaway,
        link: data.boost_url.clone(),
    }
}

/// Converts a TL `premium.BoostsList` into `tg_stats::BoostsListSlice`.
#[must_use]
pub fn boosts_list_from_tl(data: &tl_types::premium::BoostsList, gifts: bool) -> BoostsListSlice {
    const MONTHS_DIVIDER: i32 = 30 * 86_400;
    let list: Vec<Boost> = data
        .boosts
        .iter()
        .map(|tl| {
            let tl_enums::Boost::Boost(b) = tl;
            let gift_code_link = match &b.used_gift_slug {
                Some(slug) if !slug.is_empty() => GiftCodeLink {
                    text: format!("t.me/giftcode/{slug}"),
                    link: format!("https://t.me/giftcode/{slug}"),
                    slug: slug.clone(),
                },
                _ => GiftCodeLink::default(),
            };
            Boost {
                id: b.id.clone(),
                user_id: b.user_id.map_or(0, |id| id.max(0) as u64),
                giveaway_message_id: b.giveaway_msg_id.unwrap_or(0),
                date: i64::from(b.date),
                expires_at: i64::from(b.expires),
                expires_after_months: (b.expires - b.date) / MONTHS_DIVIDER,
                gift_code_link,
                multiplier: b.multiplier.unwrap_or(0),
                credits: b.stars.map_or(0, |s| s.max(0) as u64),
                is_gift: b.gift,
                is_giveaway: b.giveaway,
                is_unclaimed: b.unclaimed,
            }
        })
        .collect();

    let next = data.next_offset.clone().unwrap_or_default();
    let all_loaded = next.is_empty() || data.count == list.len() as i32;
    BoostsListSlice {
        list,
        multiplied_total: data.count,
        all_loaded,
        token: BoostsOffsetToken { next, gifts },
    }
}

/// Converts a TL `payments.StarsRevenueStats` (with `ton: true`) into `tg_stats::EarnStatistics`.
#[must_use]
pub fn ton_earn_stats_from_tl(data: &tl_types::payments::StarsRevenueStats) -> EarnStatistics {
    let tl_enums::StarsRevenueStatus::StarsRevenueStatus(status) = &data.status;
    let mut revenue_graph = statistical_graph_from_tl(&data.revenue_graph);
    revenue_graph.chart.currency_rate = data.usd_rate;
    let top_hours_graph = data
        .top_hours_graph
        .as_ref()
        .map(statistical_graph_from_tl)
        .unwrap_or_default();
    EarnStatistics {
        top_hours_graph,
        revenue_graph,
        current_balance: credits_amount_from_tl(&status.current_balance),
        available_balance: credits_amount_from_tl(&status.available_balance),
        overall_revenue: credits_amount_from_tl(&status.overall_revenue),
        usd_rate: data.usd_rate,
        switched_off: false,
        first_history_slice: EarnHistorySlice::default(),
    }
}

/// Converts a TL `payments.StarsRevenueStats` (with `ton: false`) into `tg_stats::CreditsEarnStatistics`.
#[must_use]
pub fn credits_earn_stats_from_tl(
    data: &tl_types::payments::StarsRevenueStats,
    buy_ads_url: String,
) -> CreditsEarnStatistics {
    let tl_enums::StarsRevenueStatus::StarsRevenueStatus(status) = &data.status;
    let mut revenue_graph = statistical_graph_from_tl(&data.revenue_graph);
    revenue_graph.chart.currency_rate = data.usd_rate;
    CreditsEarnStatistics {
        revenue_graph,
        current_balance: credits_amount_from_tl(&status.current_balance),
        available_balance: credits_amount_from_tl(&status.available_balance),
        overall_revenue: credits_amount_from_tl(&status.overall_revenue),
        usd_rate: data.usd_rate,
        is_withdrawal_enabled: status.withdrawal_enabled,
        next_withdrawal_at: i64::from(status.next_withdrawal_at.unwrap_or(0)),
        buy_ads_url,
    }
}

/// Converts a TL `payments.StarsStatus` transaction page into `tg_stats::EarnHistorySlice`.
#[must_use]
pub fn earn_history_slice_from_tl(
    status: &tl_types::payments::StarsStatus,
    session: &Session,
) -> EarnHistorySlice {
    let list: Vec<EarnHistoryEntry> = status
        .history
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|tl| {
            let tl_enums::StarsTransaction::StarsTransaction(tx) = tl;
            let raw_amount = credits_amount_from_tl(&tx.amount);
            let incoming = raw_amount.value() >= 0.0;
            let abs_amount = if raw_amount.value() < 0.0 {
                if raw_amount.is_ton() {
                    CreditsAmount::from_nanoton(
                        (raw_amount.whole() * tg_stats::ONE_STAR_IN_NANO + raw_amount.nano())
                            .saturating_neg(),
                    )
                } else {
                    CreditsAmount::new(
                        raw_amount.whole().saturating_neg(),
                        raw_amount.nano().saturating_neg(),
                        raw_amount.credits_type(),
                    )
                }
            } else {
                raw_amount
            };

            let provider = match &tx.peer {
                tl_enums::StarsTransactionPeer::StarsTransactionPeer(p) => {
                    let pid = tg_stories::peer_id_from_tl(&p.peer);
                    peer_display_title(session, pid)
                }
                tl_enums::StarsTransactionPeer::Fragment => "Fragment".to_owned(),
                tl_enums::StarsTransactionPeer::AppStore => "App Store".to_owned(),
                tl_enums::StarsTransactionPeer::PlayMarket => "Google Play".to_owned(),
                tl_enums::StarsTransactionPeer::PremiumBot => "Telegram Premium".to_owned(),
                tl_enums::StarsTransactionPeer::Ads => "Telegram Ads".to_owned(),
                tl_enums::StarsTransactionPeer::API => "Telegram API".to_owned(),
                tl_enums::StarsTransactionPeer::Unsupported => String::new(),
            };

            let title = tx
                .title
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if tx.ads_proceeds_to_date.is_some() {
                        "Proceeds from Telegram Ads".to_owned()
                    } else if !provider.is_empty() {
                        if incoming {
                            format!("Payment from {provider}")
                        } else {
                            format!("Withdrawal via {provider}")
                        }
                    } else if incoming {
                        "Incoming transaction".to_owned()
                    } else {
                        "Outgoing transaction".to_owned()
                    }
                });

            EarnHistoryEntry {
                id: tx.id.clone(),
                title,
                date: i64::from(tx.ads_proceeds_from_date.unwrap_or(tx.date)),
                ads_proceeds_to_date: i64::from(tx.ads_proceeds_to_date.unwrap_or(0)),
                provider,
                credits: abs_amount,
                incoming,
                pending: tx.pending,
                failed: tx.failed,
            }
        })
        .collect();

    let token = status.next_offset.clone().unwrap_or_default();
    let total = list.len() as i32;
    let all_loaded = token.is_empty();
    EarnHistorySlice {
        list,
        total,
        all_loaded,
        token,
    }
}

fn peer_to_display_info(session: &Session, peer_id: DataPeerId) -> PeerDisplayInfo {
    let Some(peer) = session.peer(peer_id) else {
        let title = format!("Peer {}", peer_id.bare());
        return PeerDisplayInfo {
            id: peer_id.bare(),
            initials: initials_from_name(&title),
            title,
            subtitle: String::new(),
            avatar_rgb: avatar_rgb_for_color_index(tg_data::decide_color_index(peer_id)),
        };
    };
    let title = peer_display_title(session, peer_id);
    let subtitle = peer_primary_username(peer)
        .map(|u| format!("@{u}"))
        .unwrap_or_default();
    PeerDisplayInfo {
        id: peer_id.bare(),
        initials: initials_from_name(&title),
        title,
        subtitle,
        avatar_rgb: avatar_rgb_for_color_index(peer.common().color_index),
    }
}

fn collect_users_map(
    session: &Session,
    users: &[tl_enums::User],
) -> BTreeMap<u64, PeerDisplayInfo> {
    let mut out = BTreeMap::new();
    for u in users {
        let uid = match u {
            tl_enums::User::User(user) => user.id,
            tl_enums::User::Empty(e) => e.id,
        };
        if uid > 0 {
            let pid = DataPeerId::from_user(DataUserId::from_tl(uid));
            out.insert(uid as u64, peer_to_display_info(session, pid));
        }
    }
    out
}

// =============================================================================
// LiveSession statistics helpers & StatsBackend implementation
// =============================================================================

impl LiveSession {
    /// Sets the active peer targeted by [`StatsBackend`] methods on this live session.
    pub fn set_stats_target_peer(&self, peer_id: PeerId) {
        let state = stats_state_for(self);
        if let Ok(mut guard) = state.lock() {
            guard.target_peer = (!peer_id.is_zero()).then(|| to_data_peer(peer_id));
        }
    }

    /// Returns the currently selected statistics target peer, if any.
    #[must_use]
    pub fn stats_target_peer(&self) -> Option<PeerId> {
        stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.target_peer.map(|p| PeerId(p.0)))
    }

    /// Overrides the statistics DC ID for `peer_id` (matching Telegram's `channelFull.stats_dc`).
    pub fn set_stats_dc_id(&self, peer_id: PeerId, dc_id: i32) {
        let state = stats_state_for(self);
        if let Ok(mut guard) = state.lock() {
            if dc_id > 0 {
                guard.stats_dc_by_peer.insert(to_data_peer(peer_id), dc_id);
            } else {
                guard.stats_dc_by_peer.remove(&to_data_peer(peer_id));
            }
        }
    }

    fn read_session_stats<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(guard) = session_lock.try_read() {
            return f(&guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let guard = session_arc.read().await;
            f(&guard)
        })
    }

    fn write_session_stats<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(mut guard) = session_lock.try_write() {
            return f(&mut guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let mut guard = session_arc.write().await;
            f(&mut guard)
        })
    }

    fn invoke_stats_rpc<R>(&self, dc_id: Option<i32>, req: R) -> Result<R::Return, String>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime().rpc().clone();
        let target_dc = dc_id.filter(|&d| d > 0).unwrap_or_else(|| rpc.main_dc_id());
        block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(STATS_RPC_TIMEOUT, async move {
                rpc.invoke_in_dc(target_dc, &req).await
            })
            .await
        })
    }

    fn stats_dc_for(&self, peer_id: DataPeerId) -> Option<i32> {
        stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.stats_dc_by_peer.get(&peer_id).copied())
    }

    fn remember_last_stats_dc(&self, dc_id: Option<i32>) {
        if let Ok(mut g) = stats_state_for(self).lock() {
            g.last_stats_dc = dc_id;
        }
    }

    /// Resolves a `StatsGraphAsync` token via `stats.loadAsyncGraph(token, x = None)`.
    ///
    /// If the RPC succeeds, returns the resolved graph (which may be a parsed chart or a
    /// `StatsGraphError`). If the RPC fails (e.g., network error or unmocked token), keeps
    /// the original `StatsGraphAsync` with its `zoom_token` intact.
    fn resolve_async_graph(&self, dc_id: Option<i32>, graph: StatisticalGraph) -> StatisticalGraph {
        if !graph.chart.is_empty() || graph.zoom_token.is_empty() || !graph.error.is_empty() {
            return graph;
        }
        let req = tl_fns::stats::LoadAsyncGraph {
            token: graph.zoom_token.clone(),
            x: None,
        };
        match self.invoke_stats_rpc(dc_id, req) {
            Ok(tl_graph) => statistical_graph_from_tl(&tl_graph),
            Err(err) => {
                debug!(token = %graph.zoom_token, error = %err, "stats.loadAsyncGraph hydration skipped");
                graph
            }
        }
    }

    fn hydrate_channel_graphs(&self, dc_id: Option<i32>, stats: &mut ChannelStatistics) {
        stats.member_count_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.member_count_graph));
        stats.join_graph = self.resolve_async_graph(dc_id, std::mem::take(&mut stats.join_graph));
        stats.mute_graph = self.resolve_async_graph(dc_id, std::mem::take(&mut stats.mute_graph));
        stats.view_count_by_hour_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.view_count_by_hour_graph));
        stats.view_count_by_source_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.view_count_by_source_graph));
        stats.join_by_source_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.join_by_source_graph));
        stats.language_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.language_graph));
        stats.message_interaction_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.message_interaction_graph));
        stats.instant_view_interaction_graph = self.resolve_async_graph(
            dc_id,
            std::mem::take(&mut stats.instant_view_interaction_graph),
        );
        stats.reactions_by_emotion_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.reactions_by_emotion_graph));
        stats.story_interactions_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.story_interactions_graph));
        stats.story_reactions_by_emotion_graph = self.resolve_async_graph(
            dc_id,
            std::mem::take(&mut stats.story_reactions_by_emotion_graph),
        );
    }

    fn hydrate_supergroup_graphs(&self, dc_id: Option<i32>, stats: &mut SupergroupStatistics) {
        stats.member_count_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.member_count_graph));
        stats.join_graph = self.resolve_async_graph(dc_id, std::mem::take(&mut stats.join_graph));
        stats.join_by_source_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.join_by_source_graph));
        stats.language_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.language_graph));
        stats.message_content_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.message_content_graph));
        stats.action_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut stats.action_graph));
        stats.day_graph = self.resolve_async_graph(dc_id, std::mem::take(&mut stats.day_graph));
        stats.week_graph = self.resolve_async_graph(dc_id, std::mem::take(&mut stats.week_graph));
    }

    fn resolve_broadcast_channel(
        &self,
    ) -> Result<(DataPeerId, tl_enums::InputChannel, PeerDisplayInfo), String> {
        let explicit = stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.target_peer);
        self.read_session_stats(|s| {
            let pick = explicit
                .and_then(|pid| s.peer(pid))
                .filter(|p| p.as_channel().is_some_and(|ch| ch.is_broadcast()))
                .or_else(|| {
                    known_peer_ids(s)
                        .into_iter()
                        .filter_map(|pid| s.peer(pid))
                        .find(|p| p.as_channel().is_some_and(|ch| ch.is_broadcast()))
                });
            let Some(peer) = pick else {
                return Err("No broadcast channel selected for statistics.".to_owned());
            };
            let ch = peer
                .as_channel()
                .ok_or_else(|| "Peer is not a channel.".to_owned())?;
            let input_channel = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            });
            Ok((peer.id(), input_channel, peer_to_display_info(s, peer.id())))
        })
    }

    fn resolve_megagroup_channel(
        &self,
    ) -> Result<(DataPeerId, tl_enums::InputChannel, PeerDisplayInfo), String> {
        let explicit = stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.target_peer);
        self.read_session_stats(|s| {
            let pick = explicit
                .and_then(|pid| s.peer(pid))
                .filter(|p| p.as_channel().is_some_and(|ch| ch.is_megagroup()))
                .or_else(|| {
                    known_peer_ids(s)
                        .into_iter()
                        .filter_map(|pid| s.peer(pid))
                        .find(|p| p.as_channel().is_some_and(|ch| ch.is_megagroup()))
                });
            let Some(peer) = pick else {
                return Err("No supergroup selected for statistics.".to_owned());
            };
            let ch = peer
                .as_channel()
                .ok_or_else(|| "Peer is not a supergroup.".to_owned())?;
            let input_channel = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            });
            Ok((peer.id(), input_channel, peer_to_display_info(s, peer.id())))
        })
    }

    fn resolve_any_channel(
        &self,
    ) -> Result<
        (
            DataPeerId,
            tl_enums::InputPeer,
            tl_enums::InputChannel,
            bool,
            PeerDisplayInfo,
        ),
        String,
    > {
        let explicit = stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.target_peer);
        self.read_session_stats(|s| {
            let pick = explicit
                .and_then(|pid| s.peer(pid))
                .filter(|p| p.as_channel().is_some())
                .or_else(|| {
                    known_peer_ids(s)
                        .into_iter()
                        .filter_map(|pid| s.peer(pid))
                        .find(|p| p.as_channel().is_some())
                });
            let Some(peer) = pick else {
                return Err("No channel or supergroup selected for statistics.".to_owned());
            };
            let ch = peer
                .as_channel()
                .ok_or_else(|| "Peer is not a channel.".to_owned())?;
            let input_channel = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            });
            let input_peer = input_peer_for(s, peer.id());
            Ok((
                peer.id(),
                input_peer,
                input_channel,
                ch.is_megagroup(),
                peer_to_display_info(s, peer.id()),
            ))
        })
    }

    fn resolve_bot_peer(
        &self,
    ) -> Result<(DataPeerId, tl_enums::InputPeer, PeerDisplayInfo), String> {
        let explicit = stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.target_peer);
        self.read_session_stats(|s| {
            let pick = explicit
                .and_then(|pid| s.peer(pid))
                .filter(|p| p.as_user().is_some())
                .or_else(|| {
                    known_peer_ids(s)
                        .into_iter()
                        .filter_map(|pid| s.peer(pid))
                        .find(|p| p.as_user().is_some_and(tg_data::User::is_bot))
                });
            let Some(peer) = pick else {
                return Err("No bot selected for monetization statistics.".to_owned());
            };
            let input_peer = input_peer_for(s, peer.id());
            Ok((peer.id(), input_peer, peer_to_display_info(s, peer.id())))
        })
    }

    fn fetch_boosts_slice(
        &self,
        input_peer: tl_enums::InputPeer,
        token: &BoostsOffsetToken,
    ) -> Result<(BoostsListSlice, BTreeMap<u64, PeerDisplayInfo>), String> {
        let limit = if token.next.is_empty() {
            BOOSTS_FIRST_SLICE
        } else {
            BOOSTS_PAGE_LIMIT
        };
        let req = tl_fns::premium::GetBoostsList {
            gifts: token.gifts,
            peer: input_peer,
            offset: token.next.clone(),
            limit,
        };
        let tl_enums::premium::BoostsList::BoostsList(resp) = self.invoke_stats_rpc(None, req)?;
        let users = self.write_session_stats(|s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
            collect_users_map(s, &resp.users)
        });
        let slice = boosts_list_from_tl(&resp, token.gifts);
        Ok((slice, users))
    }

    fn fetch_stars_transactions_slice(
        &self,
        input_peer: tl_enums::InputPeer,
        ton: bool,
        inbound: bool,
        outbound: bool,
        offset: &str,
        limit: i32,
    ) -> Result<EarnHistorySlice, String> {
        let req = tl_fns::payments::GetStarsTransactions {
            inbound,
            outbound,
            ascending: false,
            ton,
            subscription_id: None,
            peer: input_peer,
            offset: offset.to_owned(),
            limit,
        };
        let tl_enums::payments::StarsStatus::StarsStatus(status) =
            self.invoke_stats_rpc(None, req)?;
        let slice = self.write_session_stats(|s| {
            apply_tl_users_and_chats(s, &status.users, &status.chats);
            earn_history_slice_from_tl(&status, s)
        });
        Ok(slice)
    }

    fn parse_public_forwards(
        &self,
        pf: tl_types::stats::PublicForwards,
    ) -> (i32, Vec<PublicForwardRow>, String) {
        let next_offset = pf.next_offset.clone().unwrap_or_default();
        self.write_session_stats(|s| {
            apply_tl_users_and_chats(s, &pf.users, &pf.chats);
            let mut rows = Vec::with_capacity(pf.forwards.len());
            for fwd in &pf.forwards {
                match fwd {
                    tl_enums::PublicForward::Message(m) => {
                        if let tl_enums::Message::Message(msg) = &m.message {
                            let fwd_peer = tg_stories::peer_id_from_tl(&msg.peer_id);
                            let subscribers = s
                                .peer(fwd_peer)
                                .and_then(Peer::as_channel)
                                .map_or(0, |ch| ch.members_count.max(0));
                            rows.push(PublicForwardRow {
                                peer: peer_to_display_info(s, fwd_peer),
                                message_id: msg.id,
                                story_id: 0,
                                subscribers,
                                views: msg.views.unwrap_or(0),
                            });
                        }
                    }
                    tl_enums::PublicForward::Story(st) => {
                        let fwd_peer = tg_stories::peer_id_from_tl(&st.peer);
                        let subscribers = s
                            .peer(fwd_peer)
                            .and_then(Peer::as_channel)
                            .map_or(0, |ch| ch.members_count.max(0));
                        let (sid, views) = match &st.story {
                            tl_enums::StoryItem::StoryItem(si) => {
                                let v = si.views.as_ref().map_or(0, |sv| {
                                    let tl_enums::StoryViews::StoryViews(d) = sv;
                                    d.views_count
                                });
                                (si.id, v)
                            }
                            tl_enums::StoryItem::Skipped(sk) => (sk.id, 0),
                            tl_enums::StoryItem::Deleted(d) => (d.id, 0),
                        };
                        rows.push(PublicForwardRow {
                            peer: peer_to_display_info(s, fwd_peer),
                            message_id: 0,
                            story_id: sid,
                            subscribers,
                            views,
                        });
                    }
                }
            }
            (pf.count, rows, next_offset)
        })
    }
}

impl StatsBackend for LiveSession {
    fn channel_statistics(&self) -> Result<ChannelStatsBundle, String> {
        let (peer_id, input_channel, peer_info) = self.resolve_broadcast_channel()?;
        let dc_id = self.stats_dc_for(peer_id);
        self.remember_last_stats_dc(dc_id);

        let req = tl_fns::stats::GetBroadcastStats {
            dark: false,
            channel: input_channel,
        };
        let tl_enums::stats::BroadcastStats::BroadcastStats(resp) =
            self.invoke_stats_rpc(dc_id, req)?;

        let (mut stats, recent_interactions) = broadcast_stats_from_tl(&resp);
        self.hydrate_channel_graphs(dc_id, &mut stats);

        let recent_posts = self.read_session_stats(|s| {
            recent_interactions
                .into_iter()
                .map(|interaction| {
                    let is_story = interaction.story_id != 0;
                    let (snippet, date_unix) = if !is_story && interaction.message_id > 0 {
                        s.message(DataFullMsgId::new(
                            peer_id,
                            DataMsgId(i64::from(interaction.message_id)),
                        ))
                        .map(|m| {
                            let txt = m.text().map(|t| t.text.clone()).unwrap_or_default();
                            let snippet = if txt.is_empty() {
                                format!("Post #{}", interaction.message_id)
                            } else {
                                txt
                            };
                            (snippet, i64::from(m.date))
                        })
                        .unwrap_or_else(|| {
                            (format!("Post #{}", interaction.message_id), stats.end_date)
                        })
                    } else {
                        (format!("Story #{}", interaction.story_id), stats.end_date)
                    };
                    RecentPostPreview {
                        interaction,
                        is_story,
                        snippet,
                        date_unix,
                        author_title: peer_info.title.clone(),
                        initials: peer_info.initials.clone(),
                        avatar_rgb: peer_info.avatar_rgb,
                    }
                })
                .collect()
        });

        Ok(ChannelStatsBundle {
            peer: peer_info,
            stats,
            recent_posts,
        })
    }

    fn supergroup_statistics(&self) -> Result<SupergroupStatsBundle, String> {
        let (peer_id, input_channel, peer_info) = self.resolve_megagroup_channel()?;
        let dc_id = self.stats_dc_for(peer_id);
        self.remember_last_stats_dc(dc_id);

        let req = tl_fns::stats::GetMegagroupStats {
            dark: false,
            channel: input_channel,
        };
        let tl_enums::stats::MegagroupStats::MegagroupStats(resp) =
            self.invoke_stats_rpc(dc_id, req)?;

        let users = self.write_session_stats(|s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
            collect_users_map(s, &resp.users)
        });
        let (mut stats, lists) = megagroup_stats_from_tl(&resp);
        self.hydrate_supergroup_graphs(dc_id, &mut stats);

        Ok(SupergroupStatsBundle {
            peer: peer_info,
            stats,
            lists,
            users,
        })
    }

    fn message_statistics(&self, message_id: i32) -> Result<MessageStatsBundle, String> {
        let (peer_id, input_channel, peer_info) = self.resolve_broadcast_channel()?;
        let dc_id = self.stats_dc_for(peer_id);
        self.remember_last_stats_dc(dc_id);

        let req = tl_fns::stats::GetMessageStats {
            dark: false,
            channel: input_channel.clone(),
            msg_id: message_id,
        };
        let tl_enums::stats::MessageStats::MessageStats(resp) =
            self.invoke_stats_rpc(dc_id, req)?;
        let message_interaction_graph =
            self.resolve_async_graph(dc_id, statistical_graph_from_tl(&resp.views_graph));
        let reactions_by_emotion_graph = self.resolve_async_graph(
            dc_id,
            statistical_graph_from_tl(&resp.reactions_by_emotion_graph),
        );

        // Fetch views/forwards/reactions via channels.getMessages (matching C++ requestPrivateForwards).
        let (mut views, mut forwards, mut reactions, mut snippet, mut date_unix) =
            (0i32, 0i32, 0i32, format!("Post #{message_id}"), 0i64);
        let msg_req = tl_fns::channels::GetMessages {
            channel: input_channel.clone(),
            id: vec![tl_enums::InputMessage::ID(tl_types::InputMessageID {
                id: message_id,
            })],
        };
        if let Ok(msgs_resp) = self.invoke_stats_rpc(None, msg_req) {
            let (messages_vec, channel_pts) = match msgs_resp {
                tl_enums::messages::Messages::Messages(m) => (Some(m.messages), None),
                tl_enums::messages::Messages::Slice(m) => (Some(m.messages), None),
                tl_enums::messages::Messages::ChannelMessages(m) => {
                    (Some(m.messages), (m.pts > 0).then_some(m.pts))
                }
                tl_enums::messages::Messages::NotModified(_) => (None, None),
            };
            if let Some(pts) = channel_pts
                && let Some(ch_id) = peer_id.to_channel()
            {
                let _ = self.apply_channel_pts(ch_id, pts, 0);
            }
            if let Some(first) = messages_vec.and_then(|v| v.into_iter().next())
                && let tl_enums::Message::Message(m) = first
            {
                views = m.views.unwrap_or(0);
                forwards = m.forwards.unwrap_or(0);
                reactions = m.reactions.as_ref().map_or(0, |r| {
                    let tl_enums::MessageReactions::MessageReactions(mr) = r;
                    mr.results
                        .iter()
                        .map(|rc| {
                            let tl_enums::ReactionCount::ReactionCount(c) = rc;
                            c.count
                        })
                        .sum()
                });
                if !m.message.is_empty() {
                    snippet = m.message;
                }
                date_unix = i64::from(m.date);
            }
        } else {
            self.read_session_stats(|s| {
                if let Some(m) = s.message(DataFullMsgId::new(
                    peer_id,
                    DataMsgId(i64::from(message_id)),
                )) {
                    if let Some(t) = m.text()
                        && !t.text.is_empty()
                    {
                        snippet = t.text.clone();
                    }
                    date_unix = i64::from(m.date);
                }
            });
        }

        // Fetch public forwards (matching C++ PublicForwards::request).
        let pf_req = tl_fns::stats::GetMessagePublicForwards {
            channel: input_channel,
            msg_id: message_id,
            offset: String::new(),
            limit: PUBLIC_FORWARDS_LIMIT,
        };
        let (public_forwards_total, public_forwards, public_forwards_next_offset) =
            match self.invoke_stats_rpc(dc_id, pf_req) {
                Ok(tl_enums::stats::PublicForwards::PublicForwards(pf)) => {
                    self.parse_public_forwards(pf)
                }
                Err(err) => {
                    debug!(message_id, error = %err, "stats.getMessagePublicForwards failed");
                    (0, Vec::new(), String::new())
                }
            };

        let private_forwards = (forwards - public_forwards_total).max(0);
        let post = RecentPostPreview {
            interaction: MessageInteractionInfo {
                message_id,
                story_id: 0,
                views_count: views,
                forwards_count: forwards,
                reactions_count: reactions,
            },
            is_story: false,
            snippet,
            date_unix,
            author_title: peer_info.title.clone(),
            initials: peer_info.initials.clone(),
            avatar_rgb: peer_info.avatar_rgb,
        };

        Ok(MessageStatsBundle {
            peer: peer_info,
            post,
            stats: MessageStatistics {
                message_interaction_graph,
                reactions_by_emotion_graph,
                public_forwards: public_forwards_total,
                private_forwards,
                views,
                reactions,
            },
            public_forwards,
            public_forwards_next_offset,
        })
    }

    fn story_statistics(&self, story_id: i32) -> Result<MessageStatsBundle, String> {
        let (peer_id, input_peer, _input_channel, _is_mg, peer_info) =
            self.resolve_any_channel()?;
        let dc_id = self.stats_dc_for(peer_id);
        self.remember_last_stats_dc(dc_id);

        let req = tl_fns::stats::GetStoryStats {
            dark: false,
            peer: input_peer.clone(),
            id: story_id,
        };
        let tl_enums::stats::StoryStats::StoryStats(resp) = self.invoke_stats_rpc(dc_id, req)?;
        let message_interaction_graph =
            self.resolve_async_graph(dc_id, statistical_graph_from_tl(&resp.views_graph));
        let reactions_by_emotion_graph = self.resolve_async_graph(
            dc_id,
            statistical_graph_from_tl(&resp.reactions_by_emotion_graph),
        );

        let (mut views, mut forwards, mut reactions, mut snippet, mut date_unix) =
            (0i32, 0i32, 0i32, format!("Story #{story_id}"), 0i64);
        let story_req = tl_fns::stories::GetStoriesByID {
            peer: input_peer.clone(),
            id: vec![story_id],
        };
        if let Ok(tl_enums::stories::Stories::Stories(stories)) =
            self.invoke_stats_rpc(None, story_req)
            && let Some(tl_enums::StoryItem::StoryItem(item)) = stories.stories.first()
        {
            if let Some(tl_enums::StoryViews::StoryViews(sv)) = &item.views {
                views = sv.views_count;
                forwards = sv.forwards_count.unwrap_or(0);
                reactions = sv.reactions_count.unwrap_or(0);
            }
            if let Some(cap) = &item.caption
                && !cap.is_empty()
            {
                snippet = cap.clone();
            }
            date_unix = i64::from(item.date);
        }

        let pf_req = tl_fns::stats::GetStoryPublicForwards {
            peer: input_peer,
            id: story_id,
            offset: String::new(),
            limit: PUBLIC_FORWARDS_LIMIT,
        };
        let (public_forwards_total, public_forwards, public_forwards_next_offset) =
            match self.invoke_stats_rpc(dc_id, pf_req) {
                Ok(tl_enums::stats::PublicForwards::PublicForwards(pf)) => {
                    self.parse_public_forwards(pf)
                }
                Err(err) => {
                    debug!(story_id, error = %err, "stats.getStoryPublicForwards failed");
                    (0, Vec::new(), String::new())
                }
            };

        let private_forwards = (forwards - public_forwards_total).max(0);
        let post = RecentPostPreview {
            interaction: MessageInteractionInfo {
                message_id: 0,
                story_id,
                views_count: views,
                forwards_count: forwards,
                reactions_count: reactions,
            },
            is_story: true,
            snippet,
            date_unix,
            author_title: peer_info.title.clone(),
            initials: peer_info.initials.clone(),
            avatar_rgb: peer_info.avatar_rgb,
        };

        Ok(MessageStatsBundle {
            peer: peer_info,
            post,
            stats: MessageStatistics {
                message_interaction_graph,
                reactions_by_emotion_graph,
                public_forwards: public_forwards_total,
                private_forwards,
                views,
                reactions,
            },
            public_forwards,
            public_forwards_next_offset,
        })
    }

    fn load_zoom_graph(
        &self,
        zoom_token: &str,
        x_timestamp: f64,
    ) -> Result<StatisticalGraph, String> {
        if zoom_token.is_empty() {
            return Err("Chart does not support remote zoom".to_owned());
        }
        let dc_id = stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.last_stats_dc);
        let x = if x_timestamp > 0.0 && x_timestamp.is_finite() {
            Some(x_timestamp.round() as i64)
        } else {
            None
        };
        let req = tl_fns::stats::LoadAsyncGraph {
            token: zoom_token.to_owned(),
            x,
        };
        let resp = self.invoke_stats_rpc(dc_id, req)?;
        Ok(statistical_graph_from_tl(&resp))
    }

    fn boost_status(&self) -> Result<BoostStatusBundle, String> {
        let (peer_id, input_peer, _input_channel, is_megagroup, peer_info) =
            self.resolve_any_channel()?;
        let req = tl_fns::premium::GetBoostsStatus {
            peer: input_peer.clone(),
        };
        let tl_enums::premium::BoostsStatus::BoostsStatus(resp) =
            self.invoke_stats_rpc(None, req)?;

        self.write_session_stats(|s| {
            if let Some(Peer::Channel(ch)) = s.peer_mut(peer_id) {
                ch.level_hint = resp.level.max(0);
            }
        });

        let mut status = boost_status_from_tl(&resp, is_megagroup);
        let mut users = BTreeMap::new();

        match self.fetch_boosts_slice(
            input_peer.clone(),
            &BoostsOffsetToken {
                next: String::new(),
                gifts: false,
            },
        ) {
            Ok((slice, slice_users)) => {
                status.first_slice_boosts = slice;
                users.extend(slice_users);
            }
            Err(err) => {
                debug!(peer = peer_id.raw(), error = %err, "premium.getBoostsList(boosts) failed");
            }
        }

        match self.fetch_boosts_slice(
            input_peer,
            &BoostsOffsetToken {
                next: String::new(),
                gifts: true,
            },
        ) {
            Ok((slice, slice_users)) => {
                status.first_slice_gifts = slice;
                users.extend(slice_users);
            }
            Err(err) => {
                debug!(peer = peer_id.raw(), error = %err, "premium.getBoostsList(gifts) failed");
            }
        }

        Ok(BoostStatusBundle {
            peer: peer_info,
            status,
            users,
            giveaway_boosts_per_premium: DEFAULT_GIVEAWAY_BOOSTS_PER_PREMIUM,
        })
    }

    fn load_more_boosts(&self, token: &BoostsOffsetToken) -> Result<BoostsListSlice, String> {
        let (_peer_id, input_peer, _, _, _) = self.resolve_any_channel()?;
        let (slice, _) = self.fetch_boosts_slice(input_peer, token)?;
        Ok(slice)
    }

    fn channel_earn(&self) -> Result<ChannelEarnBundle, String> {
        let (peer_id, input_peer, input_channel, _is_mg, peer_info) = self.resolve_any_channel()?;
        let dc_id = self.stats_dc_for(peer_id);
        self.remember_last_stats_dc(dc_id);

        // TON ad revenue (`payments.getStarsRevenueStats(ton = true)`, Layer 233 successor to
        // `stats.getBroadcastRevenueStats`).
        let ton_res = self.invoke_stats_rpc(
            dc_id,
            tl_fns::payments::GetStarsRevenueStats {
                dark: false,
                ton: true,
                peer: input_peer.clone(),
            },
        );
        // Stars revenue (`payments.getStarsRevenueStats(ton = false)`).
        let stars_res = self.invoke_stats_rpc(
            dc_id,
            tl_fns::payments::GetStarsRevenueStats {
                dark: false,
                ton: false,
                peer: input_peer.clone(),
            },
        );

        let (mut ton, can_view_currency, ton_withdrawal_enabled) = match ton_res {
            Ok(tl_enums::payments::StarsRevenueStats::StarsRevenueStats(d)) => {
                let tl_enums::StarsRevenueStatus::StarsRevenueStatus(ref st) = d.status;
                let withdrawal = st.withdrawal_enabled;
                let mut parsed = ton_earn_stats_from_tl(&d);
                parsed.top_hours_graph =
                    self.resolve_async_graph(dc_id, std::mem::take(&mut parsed.top_hours_graph));
                parsed.revenue_graph =
                    self.resolve_async_graph(dc_id, std::mem::take(&mut parsed.revenue_graph));
                parsed.revenue_graph.chart.currency_rate = parsed.usd_rate;
                (parsed, true, withdrawal)
            }
            Err(err) => {
                if let Err(stars_err) = &stars_res {
                    return Err(format!(
                        "Failed to load channel revenue stats (TON: {err}; Stars: {stars_err})"
                    ));
                }
                debug!(peer = peer_id.raw(), error = %err, "TON payments.getStarsRevenueStats failed");
                (EarnStatistics::default(), false, false)
            }
        };

        if can_view_currency {
            match self.fetch_stars_transactions_slice(
                input_peer.clone(),
                true,
                false,
                false,
                "",
                EARN_FIRST_SLICE,
            ) {
                Ok(slice) => ton.first_history_slice = slice,
                Err(err) => {
                    debug!(peer = peer_id.raw(), error = %err, "TON payments.getStarsTransactions failed");
                }
            }
        }

        let (credits, can_view_credits) = match stars_res {
            Ok(tl_enums::payments::StarsRevenueStats::StarsRevenueStats(d)) => {
                let buy_ads_url = match self.invoke_stats_rpc(
                    None,
                    tl_fns::payments::GetStarsRevenueAdsAccountUrl {
                        peer: input_peer.clone(),
                    },
                ) {
                    Ok(
                        tl_enums::payments::StarsRevenueAdsAccountUrl::StarsRevenueAdsAccountUrl(u),
                    ) => u.url,
                    Err(_) => String::new(),
                };
                let mut parsed = credits_earn_stats_from_tl(&d, buy_ads_url);
                parsed.revenue_graph =
                    self.resolve_async_graph(dc_id, std::mem::take(&mut parsed.revenue_graph));
                parsed.revenue_graph.chart.currency_rate = parsed.usd_rate;
                (parsed, true)
            }
            Err(err) => {
                debug!(peer = peer_id.raw(), error = %err, "Stars payments.getStarsRevenueStats failed");
                (CreditsEarnStatistics::default(), false)
            }
        };

        let credits_history = if can_view_credits {
            self.fetch_stars_transactions_slice(
                input_peer,
                false,
                false,
                false,
                "",
                EARN_FIRST_SLICE,
            )
            .unwrap_or_default()
        } else {
            EarnHistorySlice::default()
        };

        // Best-effort refresh of channelFull flags (`restricted_sponsored`, `stats_dc`, `level`).
        let mut current_level = self.read_session_stats(|s| {
            s.peer(peer_id)
                .and_then(Peer::as_channel)
                .map_or(0, |ch| ch.level_hint)
        });
        if let Ok(tl_enums::messages::ChatFull::ChatFull(full)) = self.invoke_stats_rpc(
            None,
            tl_fns::channels::GetFullChannel {
                channel: input_channel,
            },
        ) && let tl_enums::ChatFull::ChannelFull(cf) = full.full_chat
        {
            ton.switched_off = cf.restricted_sponsored;
            if let Some(dc) = cf.stats_dc
                && dc > 0
            {
                self.set_stats_dc_id(PeerId(peer_id.0), dc);
            }
            self.write_session_stats(|s| {
                if let Some(Peer::Channel(ch)) = s.peer_mut(peer_id) {
                    ch.flags
                        .set(ChannelFlags::CAN_VIEW_REVENUE, cf.can_view_revenue);
                    current_level = ch.level_hint;
                }
            });
        }

        Ok(ChannelEarnBundle {
            peer: peer_info,
            ton,
            credits,
            credits_history,
            can_view_currency,
            can_view_credits,
            withdrawal_enabled: ton_withdrawal_enabled,
            restrict_sponsored_level_min: DEFAULT_RESTRICT_SPONSORED_LEVEL_MIN,
            current_level,
        })
    }

    fn load_more_channel_earn_history(&self, token: &str) -> Result<EarnHistorySlice, String> {
        let (_peer_id, input_peer, _, _, _) = self.resolve_any_channel()?;
        let limit = if token.is_empty() {
            EARN_FIRST_SLICE
        } else {
            EARN_PAGE_LIMIT
        };
        self.fetch_stars_transactions_slice(input_peer, true, false, false, token, limit)
    }

    fn bot_earn(&self) -> Result<BotEarnBundle, String> {
        let (peer_id, input_peer, peer_info) = self.resolve_bot_peer()?;
        let dc_id = self.stats_dc_for(peer_id);
        self.remember_last_stats_dc(dc_id);

        let tl_enums::payments::StarsRevenueStats::StarsRevenueStats(stats_resp) = self
            .invoke_stats_rpc(
                dc_id,
                tl_fns::payments::GetStarsRevenueStats {
                    dark: false,
                    ton: false,
                    peer: input_peer.clone(),
                },
            )?;

        let buy_ads_url = match self.invoke_stats_rpc(
            None,
            tl_fns::payments::GetStarsRevenueAdsAccountUrl {
                peer: input_peer.clone(),
            },
        ) {
            Ok(tl_enums::payments::StarsRevenueAdsAccountUrl::StarsRevenueAdsAccountUrl(u)) => {
                u.url
            }
            Err(_) => String::new(),
        };

        let mut credits = credits_earn_stats_from_tl(&stats_resp, buy_ads_url);
        credits.revenue_graph =
            self.resolve_async_graph(dc_id, std::mem::take(&mut credits.revenue_graph));
        credits.revenue_graph.chart.currency_rate = credits.usd_rate;

        let full_history = self
            .fetch_stars_transactions_slice(
                input_peer.clone(),
                false,
                false,
                false,
                "",
                EARN_FIRST_SLICE,
            )
            .unwrap_or_default();

        let in_history = self
            .fetch_stars_transactions_slice(
                input_peer.clone(),
                false,
                true,
                false,
                "",
                EARN_FIRST_SLICE,
            )
            .unwrap_or_else(|_| {
                let list: Vec<_> = full_history
                    .list
                    .iter()
                    .filter(|e| e.incoming)
                    .cloned()
                    .collect();
                EarnHistorySlice {
                    total: list.len() as i32,
                    list,
                    all_loaded: full_history.all_loaded,
                    token: String::new(),
                }
            });

        let out_history = self
            .fetch_stars_transactions_slice(input_peer, false, false, true, "", EARN_FIRST_SLICE)
            .unwrap_or_else(|_| {
                let list: Vec<_> = full_history
                    .list
                    .iter()
                    .filter(|e| !e.incoming)
                    .cloned()
                    .collect();
                EarnHistorySlice {
                    total: list.len() as i32,
                    list,
                    all_loaded: full_history.all_loaded,
                    token: String::new(),
                }
            });

        Ok(BotEarnBundle {
            peer: peer_info,
            credits,
            full_history,
            in_history,
            out_history,
            affiliate_allowed: true,
        })
    }

    fn load_async_graph(&self, token: &str) -> Result<StatisticalGraph, String> {
        if token.is_empty() {
            return Err("Async graph token is empty.".to_owned());
        }
        let dc_id = stats_state_for(self)
            .lock()
            .ok()
            .and_then(|g| g.last_stats_dc);
        let req = tl_fns::stats::LoadAsyncGraph {
            token: token.to_owned(),
            x: None,
        };
        let resp = self.invoke_stats_rpc(dc_id, req)?;
        Ok(statistical_graph_from_tl(&resp))
    }

    fn load_more_public_forwards(
        &self,
        message_id: i32,
        is_story: bool,
        offset: &str,
    ) -> Result<(Vec<PublicForwardRow>, String), String> {
        if offset.is_empty() {
            return Ok((Vec::new(), String::new()));
        }
        if is_story {
            let (peer_id, input_peer, _, _, _) = self.resolve_any_channel()?;
            let dc_id = self.stats_dc_for(peer_id);
            let req = tl_fns::stats::GetStoryPublicForwards {
                peer: input_peer,
                id: message_id,
                offset: offset.to_owned(),
                limit: PUBLIC_FORWARDS_LIMIT,
            };
            let tl_enums::stats::PublicForwards::PublicForwards(pf) =
                self.invoke_stats_rpc(dc_id, req)?;
            let (_count, rows, next_offset) = self.parse_public_forwards(pf);
            Ok((rows, next_offset))
        } else {
            let (peer_id, input_channel, _) = self.resolve_broadcast_channel()?;
            let dc_id = self.stats_dc_for(peer_id);
            let req = tl_fns::stats::GetMessagePublicForwards {
                channel: input_channel,
                msg_id: message_id,
                offset: offset.to_owned(),
                limit: PUBLIC_FORWARDS_LIMIT,
            };
            let tl_enums::stats::PublicForwards::PublicForwards(pf) =
                self.invoke_stats_rpc(dc_id, req)?;
            let (_count, rows, next_offset) = self.parse_public_forwards(pf);
            Ok((rows, next_offset))
        }
    }

    fn supports_live_withdrawal(&self) -> bool {
        true
    }

    fn request_revenue_withdrawal_url(
        &self,
        is_bot: bool,
        ton: bool,
        amount: Option<i64>,
        password: &str,
    ) -> Result<String, String> {
        let peer_id = if is_bot {
            let (pid, _, _) = self.resolve_bot_peer()?;
            PeerId(pid.0)
        } else {
            let (pid, _, _, _, _) = self.resolve_any_channel()?;
            PeerId(pid.0)
        };
        self.get_stars_revenue_withdrawal_url(peer_id, ton, amount, password)
    }

    fn restrict_sponsored_messages(&self, restricted: bool) -> Result<(), String> {
        let (_peer_id, _input_peer, input_channel, _is_mg, _peer_info) =
            self.resolve_any_channel()?;
        let updates = self.invoke_stats_rpc(
            None,
            tl_fns::channels::RestrictSponsoredMessages {
                channel: input_channel,
                restricted,
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        Ok(())
    }
}

impl LiveSession {
    /// Toggles sponsored messages restriction for a specific channel peer via
    /// `channels.restrictSponsoredMessages` (`Api::RestrictSponsored` in `api_earn.cpp:23-35`).
    pub fn restrict_channel_sponsored_messages(
        &self,
        peer_id: PeerId,
        restricted: bool,
    ) -> Result<(), String> {
        let data_peer = to_data_peer(peer_id);
        let input_channel = self.read_session_stats(|s| {
            let ch = s
                .peer(data_peer)
                .and_then(Peer::as_channel)
                .ok_or_else(|| "Channel peer not found in session.".to_owned())?;
            Ok::<_, String>(tl_enums::InputChannel::InputChannel(
                tl_types::InputChannel {
                    channel_id: ch.channel_id().0 as i64,
                    access_hash: ch.access_hash as i64,
                },
            ))
        })?;
        let updates = self.invoke_stats_rpc(
            None,
            tl_fns::channels::RestrictSponsoredMessages {
                channel: input_channel,
                restricted,
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        Ok(())
    }
}
