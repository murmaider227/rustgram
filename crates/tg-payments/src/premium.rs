//! Telegram Premium subscription options, promo data, Premium/boost limits,
//! gift codes, giveaways, and channel boosts data models and TL conversions.
//!
//! Port of `data/data_premium_subscription_option.h`, `data/data_premium_limits.h/.cpp`,
//! `api/api_premium.h/.cpp`, `api/api_premium_option.h/.cpp`, `data/data_boosts.h`,
//! and `api/api_statistics.cpp` (Boosts section).

use std::collections::BTreeMap;

use tg_data::{ChannelId, DocumentId, FullMsgId, MsgId, PeerId, TimeId, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fn};

use crate::currency::{CREDITS_CURRENCY, fill_amount_and_currency};
use crate::star_gifts::{document_id_from_tl, peer_id_from_tl};

/// Default fallback boosts granted per Premium gift in giveaways (`giveaway_boosts_per_premium`).
pub const GIVEAWAY_BOOSTS_PER_PREMIUM_FALLBACK: i32 = 4;
/// Default max allowed countries in a giveaway (`giveaway_countries_max`).
pub const GIVEAWAY_COUNTRIES_MAX_FALLBACK: i32 = 10;
/// Default max additional channels in a giveaway (`giveaway_add_peers_max`).
pub const GIVEAWAY_ADD_PEERS_MAX_FALLBACK: i32 = 10;
/// Default max giveaway duration in seconds (`giveaway_period_max`, 7 days).
pub const GIVEAWAY_PERIOD_MAX_FALLBACK: i32 = 3600 * 24 * 7;

/// Seconds in one 30-day boost month (`kMonthsDivider` in `api_statistics.cpp`).
const BOOST_MONTH_SECS: i32 = 30 * 86_400;

/// Whole months between a boost's `date` and `expires` (`expiresAfterMonths` in
/// `api_statistics.cpp`). Computed in `i64`: server dates far apart would overflow `i32`.
fn boost_months(date: i32, expires: i32) -> i32 {
    let months = (i64::from(expires) - i64::from(date)) / i64::from(BOOST_MONTH_SECS);
    // |expires - date| < 2^32, so the month count always fits.
    i32::try_from(months).unwrap_or(0)
}

/// Formatted Telegram Premium subscription option (`Data::PremiumSubscriptionOption`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PremiumSubscriptionOption {
    /// Subscription length in months.
    pub months: i32,
    /// Discount percentage (integer `0..=100`, `0` when no discount).
    pub discount_percent: i32,
    /// Formatted discount badge (e.g., `"−33%"` or `""`).
    pub discount: String,
    /// Formatted per-month (or per-user × count for giveaways) cost string.
    pub cost_per_month: String,
    /// Formatted undiscounted baseline total string (`""` if no discount).
    pub cost_no_discount: String,
    /// Formatted annualized cost string.
    pub cost_per_year: String,
    /// Currency code.
    pub currency: String,
    /// Formatted total cost string.
    pub total: String,
    /// Raw total amount in minor currency units.
    pub raw_amount: i64,
    /// Deep link to Premium bot (`bot_url`).
    pub bot_url: String,
    /// Whether this option matches the user's active subscription.
    pub current: bool,
    /// Whether upgrading to this option is permitted.
    pub can_purchase_upgrade: bool,
}

/// Computes a single [`PremiumSubscriptionOption`] with exact discount rounding
/// (`Api::CreateSubscriptionOption` in `api_premium_option.cpp`).
pub fn create_subscription_option(
    months: i32,
    monthly_amount: f64,
    amount: i64,
    currency: &str,
    bot_url: impl Into<String>,
) -> PremiumSubscriptionOption {
    let months_f = months.max(1) as f64;
    let baseline_amount = monthly_amount * months_f;
    let discount_percent = if baseline_amount > 0.0 {
        let percent = 1.0 - (amount as f64) / baseline_amount;
        (percent * 100.0).round() as i32
    } else {
        0
    };
    let has_discount = discount_percent > 0;
    PremiumSubscriptionOption {
        months,
        discount_percent: discount_percent.max(0),
        discount: if has_discount {
            format!("\u{2212}{discount_percent}%")
        } else {
            String::new()
        },
        cost_per_month: fill_amount_and_currency(
            ((amount as f64) / months_f).round() as i64,
            currency,
            false,
        ),
        cost_no_discount: if has_discount {
            fill_amount_and_currency(baseline_amount.round() as i64, currency, false)
        } else {
            String::new()
        },
        cost_per_year: fill_amount_and_currency(
            ((amount as f64) / (months_f / 12.0)).round() as i64,
            currency,
            false,
        ),
        currency: currency.to_string(),
        total: fill_amount_and_currency(amount, currency, false),
        raw_amount: amount,
        bot_url: bot_url.into(),
        current: false,
        can_purchase_upgrade: false,
    }
}

/// Converts TL `Vector<PremiumSubscriptionOption>` into `Vec<PremiumSubscriptionOption>`
/// (`Api::PremiumSubscriptionOptionsFromTL`).
pub fn premium_subscription_options_from_tl(
    tl_opts: &[tl_enums::PremiumSubscriptionOption],
) -> Vec<PremiumSubscriptionOption> {
    if tl_opts.is_empty() {
        return Vec::new();
    }
    let mut monthly_per_currency: BTreeMap<&str, f64> = BTreeMap::new();
    for tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(o) in tl_opts {
        let entry = monthly_per_currency.entry(&o.currency).or_insert(f64::MAX);
        // Find minimum total amount per currency to establish baseline monthly rate,
        // matching C++ `ranges::min_element` by `o.amount`.
        let candidate_monthly = (o.amount as f64) / (o.months.max(1) as f64);
        if *entry == f64::MAX {
            *entry = candidate_monthly;
        }
    }
    // Exact C++ baseline: option with minimum `amount` for each currency.
    monthly_per_currency.clear();
    let mut min_by_currency: BTreeMap<&str, (i64, i32)> = BTreeMap::new();
    for tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(o) in tl_opts {
        min_by_currency
            .entry(&o.currency)
            .and_modify(|cur| {
                if o.amount < cur.0 {
                    *cur = (o.amount, o.months);
                }
            })
            .or_insert((o.amount, o.months));
    }
    for (curr, (amt, months)) in min_by_currency {
        monthly_per_currency.insert(curr, (amt as f64) / (months.max(1) as f64));
    }

    tl_opts
        .iter()
        .map(
            |tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(o)| {
                let monthly = monthly_per_currency
                    .get(o.currency.as_str())
                    .copied()
                    .unwrap_or(0.0);
                let mut opt = create_subscription_option(
                    o.months,
                    monthly,
                    o.amount,
                    &o.currency,
                    &o.bot_url,
                );
                opt.current = o.current;
                opt.can_purchase_upgrade = o.can_purchase_upgrade;
                opt
            },
        )
        .collect()
}

/// Converts TL `Vector<PremiumGiftCodeOption>` into `Vec<PremiumSubscriptionOption>`
/// (`GiftCodesFromTL` in `api_premium.cpp`).
pub fn gift_code_options_from_tl(
    tl_opts: &[tl_enums::PremiumGiftCodeOption],
) -> Vec<PremiumSubscriptionOption> {
    if tl_opts.is_empty() {
        return Vec::new();
    }
    let mut min_by_currency: BTreeMap<&str, (i64, i32)> = BTreeMap::new();
    for tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(o) in tl_opts {
        min_by_currency
            .entry(&o.currency)
            .and_modify(|cur| {
                if o.amount < cur.0 {
                    *cur = (o.amount, o.months);
                }
            })
            .or_insert((o.amount, o.months));
    }
    tl_opts
        .iter()
        .map(
            |tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(o)| {
                let (min_amt, min_months) = min_by_currency
                    .get(o.currency.as_str())
                    .copied()
                    .unwrap_or((o.amount, o.months));
                let monthly = (min_amt as f64) / (min_months.max(1) as f64);
                let mut opt =
                    create_subscription_option(o.months, monthly, o.amount, &o.currency, "");
                let per_user = ((o.amount as f64) / (o.users.max(1) as f64)).round() as i64;
                let per_user_text = fill_amount_and_currency(per_user, &o.currency, false);
                opt.cost_per_month = format!("{per_user_text} \u{00D7} {}", o.users);
                opt.total = fill_amount_and_currency(o.amount, &o.currency, false);
                opt.currency = o.currency.clone();
                opt
            },
        )
        .collect()
}

/// Parsed Telegram Premium promotional payload (`help.premiumPromo`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PremiumPromoData {
    /// Localized status text describing current subscription or promo.
    pub status_text: String,
    /// Video section key -> document ID mapping.
    pub videos: BTreeMap<String, DocumentId>,
    /// Available subscription period options.
    pub period_options: Vec<PremiumSubscriptionOption>,
    /// 1-month baseline amount in minor units (`0` if absent).
    pub monthly_amount: i64,
    /// 1-month currency code.
    pub monthly_currency: String,
}

/// Converts a TL `help.PremiumPromo` into [`PremiumPromoData`] (`Premium::reloadPromo`).
pub fn premium_promo_from_tl(tl: &tl_enums::help::PremiumPromo) -> PremiumPromoData {
    let tl_enums::help::PremiumPromo::PremiumPromo(data) = tl;
    let period_options = premium_subscription_options_from_tl(&data.period_options);
    let mut monthly_amount = 0;
    let mut monthly_currency = String::new();
    for tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(o) in &data.period_options {
        if o.months == 1 {
            monthly_amount = o.amount;
            monthly_currency = o.currency.clone();
        }
    }
    let count = data.video_sections.len().min(data.videos.len());
    let mut videos = BTreeMap::new();
    for i in 0..count {
        videos.insert(
            data.video_sections[i].clone(),
            document_id_from_tl(&data.videos[i]),
        );
    }
    PremiumPromoData {
        status_text: data.status_text.clone(),
        videos,
        period_options,
        monthly_amount,
        monthly_currency,
    }
}

/// Filters out Stars-denominated options from giveaway choices (`skipForStars` in `api_premium.cpp`).
pub fn filter_non_stars_options(
    options: Vec<PremiumSubscriptionOption>,
) -> Vec<PremiumSubscriptionOption> {
    options
        .into_iter()
        .filter(|o| o.currency != CREDITS_CURRENCY)
        .collect()
}

/// Default vs. Telegram Premium account limits (`Data::PremiumLimits` in `data_premium_limits.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumLimits {
    /// Whether the current user has Telegram Premium.
    pub is_premium: bool,
    /// App-config overrides keyed by Telegram `appConfig` limit name.
    pub overrides: BTreeMap<String, i32>,
}

impl PremiumLimits {
    /// Creates a [`PremiumLimits`] view with built-in tdesktop defaults.
    pub fn new(is_premium: bool) -> Self {
        Self {
            is_premium,
            overrides: BTreeMap::new(),
        }
    }

    fn get(&self, key: &str, fallback: i32) -> i32 {
        self.overrides.get(key).copied().unwrap_or(fallback)
    }

    /// Max joined channels/supergroups (default 500, Premium 1000).
    pub fn channels(&self) -> i32 {
        if self.is_premium {
            self.get("channels_limit_premium", 1000)
        } else {
            self.get("channels_limit_default", 500)
        }
    }

    /// Max saved GIFs (default 200, Premium 400).
    pub fn saved_gifs(&self) -> i32 {
        if self.is_premium {
            self.get("saved_gifs_limit_premium", 400)
        } else {
            self.get("saved_gifs_limit_default", 200)
        }
    }

    /// Max favorite stickers (default 5, Premium 10).
    pub fn stickers_faved(&self) -> i32 {
        if self.is_premium {
            self.get("stickers_faved_limit_premium", 10)
        } else {
            self.get("stickers_faved_limit_default", 5)
        }
    }

    /// Max chat folders (default 10, Premium 30).
    pub fn dialog_filters(&self) -> i32 {
        if self.is_premium {
            self.get("dialog_filters_limit_premium", 30)
        } else {
            self.get("dialog_filters_limit_default", 10)
        }
    }

    /// Max pinned chats in main list (default 5, Premium 10).
    pub fn dialogs_pinned(&self) -> i32 {
        if self.is_premium {
            self.get("dialogs_pinned_limit_premium", 10)
        } else {
            self.get("dialogs_pinned_limit_default", 5)
        }
    }

    /// Max caption length (default 1024, Premium 2048).
    pub fn caption_length(&self) -> i32 {
        if self.is_premium {
            self.get("caption_length_limit_premium", 2048)
        } else {
            self.get("caption_length_limit_default", 1024)
        }
    }

    /// Max message text length (default 4096, Premium 8192).
    pub fn message_length(&self) -> i32 {
        if self.is_premium {
            self.get("message_length_limit_premium", 8192)
        } else {
            self.get("message_length_limit_default", 4096)
        }
    }

    /// Max upload file parts (default 4000 (~2 GB), Premium 8000 (~4 GB)).
    pub fn upload_max_fileparts(&self) -> i32 {
        if self.is_premium {
            self.get("upload_max_fileparts_premium", 8000)
        } else {
            self.get("upload_max_fileparts_default", 4000)
        }
    }

    /// Max bio/about length (default 70, Premium 140).
    pub fn about_length(&self) -> i32 {
        if self.is_premium {
            self.get("about_length_limit_premium", 140)
        } else {
            self.get("about_length_limit_default", 70)
        }
    }

    /// Max channel boost level (`boosts_channel_level_max`, 9 in test mode, 99 in prod).
    pub fn max_boost_level(&self, is_test_mode: bool) -> i32 {
        self.get(
            "boosts_channel_level_max",
            if is_test_mode { 9 } else { 99 },
        )
    }
}

/// Minimum boost levels required to unlock channel and group perks (`Data::LevelLimits`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelLimits {
    /// Channel background emoji icon min level (4).
    pub channel_bg_icon: i32,
    /// Channel profile background emoji icon min level (7).
    pub channel_profile_bg_icon: i32,
    /// Channel emoji status min level (8).
    pub channel_emoji_status: i32,
    /// Channel wallpaper min level (9).
    pub channel_wallpaper: i32,
    /// Channel custom wallpaper min level (10).
    pub channel_custom_wallpaper: i32,
    /// Channel disable sponsored messages min level (20).
    pub channel_restrict_sponsored: i32,
    /// Channel auto-translation min level (3).
    pub channel_auto_translate: i32,
    /// Group voice transcription min level (6).
    pub group_transcribe: i32,
    /// Group emoji/sticker pack min level (4).
    pub group_emoji_stickers: i32,
    /// Group profile background emoji icon min level (5).
    pub group_profile_bg_icon: i32,
    /// Group emoji status min level (8).
    pub group_emoji_status: i32,
    /// Group wallpaper min level (9).
    pub group_wallpaper: i32,
    /// Group custom wallpaper min level (10).
    pub group_custom_wallpaper: i32,
}

impl Default for LevelLimits {
    fn default() -> Self {
        Self {
            channel_bg_icon: 4,
            channel_profile_bg_icon: 7,
            channel_emoji_status: 8,
            channel_wallpaper: 9,
            channel_custom_wallpaper: 10,
            channel_restrict_sponsored: 20,
            channel_auto_translate: 3,
            group_transcribe: 6,
            group_emoji_stickers: 4,
            group_profile_bg_icon: 5,
            group_emoji_status: 8,
            group_wallpaper: 9,
            group_custom_wallpaper: 10,
        }
    }
}

/// Checked Premium gift code (`Api::GiftCode`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftCode {
    /// Issuer channel/user peer ID.
    pub from: PeerId,
    /// Designated recipient user peer ID (or `PeerId::ZERO` if open).
    pub to: PeerId,
    /// Giveaway announcement message ID (`MsgId::ZERO` if not from giveaway).
    pub giveaway_id: MsgId,
    /// Creation timestamp.
    pub date: TimeId,
    /// Redemption timestamp (`0` if unused).
    pub used: TimeId,
    /// Subscription duration in days (`0` if invalid).
    pub days: i32,
    /// Whether this gift code came from a giveaway.
    pub giveaway: bool,
}

impl GiftCode {
    /// Returns `true` if `days > 0`.
    pub const fn is_valid(&self) -> bool {
        self.days != 0
    }

    /// Returns `true` if already redeemed.
    pub const fn is_used(&self) -> bool {
        self.used != 0
    }
}

/// Converts TL `payments.CheckedGiftCode` into [`GiftCode`] (`Parse` in `api_premium.cpp`).
pub fn checked_gift_code_from_tl(tl: &tl_enums::payments::CheckedGiftCode) -> GiftCode {
    let tl_enums::payments::CheckedGiftCode::CheckedGiftCode(c) = tl;
    GiftCode {
        from: c
            .from_id
            .as_ref()
            .map(peer_id_from_tl)
            .unwrap_or(PeerId::ZERO),
        to: c
            .to_id
            .map(|id| PeerId::from_user(UserId::from_tl(id)))
            .unwrap_or(PeerId::ZERO),
        giveaway_id: c.giveaway_msg_id.map(MsgId::from).unwrap_or(MsgId::ZERO),
        date: c.date,
        used: c.used_date.unwrap_or(0),
        days: c.days,
        giveaway: c.via_giveaway,
    }
}

/// Lifecycle state of a Telegram giveaway (`Api::GiveawayState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GiveawayState {
    /// Invalid or unknown giveaway.
    #[default]
    Invalid,
    /// Giveaway is currently accepting participants.
    Running,
    /// Entry period ended; Telegram is selecting winners.
    Preparing,
    /// Winners have been chosen.
    Finished,
    /// Giveaway was cancelled and refunded.
    Refunded,
}

/// Detailed status of a giveaway (`Api::GiveawayInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiveawayInfo {
    /// Won gift code slug, if the current user won.
    pub gift_code: String,
    /// ISO2 country code explaining ineligibility, if any.
    pub disallowed_country: String,
    /// Channel ID where the user is an admin (disallowing participation).
    pub admin_channel_id: ChannelId,
    /// Current giveaway state.
    pub state: GiveawayState,
    /// Timestamp when the user joined too early for `only_new_subscribers`.
    pub too_early_date: TimeId,
    /// Giveaway finish timestamp.
    pub finish_date: TimeId,
    /// Giveaway start timestamp.
    pub start_date: TimeId,
    /// Stars prize won (`0` for Premium giveaway or non-winner).
    pub credits: u64,
    /// Total winners count.
    pub winners_count: i32,
    /// Count of activated gift codes.
    pub activated_count: i32,
    /// Whether the current user is participating.
    pub participating: bool,
}

/// Converts TL `payments.GiveawayInfo` into [`GiveawayInfo`] (`Premium::resolveGiveawayInfo`).
pub fn giveaway_info_from_tl(tl: &tl_enums::payments::GiveawayInfo) -> GiveawayInfo {
    match tl {
        tl_enums::payments::GiveawayInfo::GiveawayInfo(d) => GiveawayInfo {
            participating: d.participating,
            state: if d.preparing_results {
                GiveawayState::Preparing
            } else {
                GiveawayState::Running
            },
            admin_channel_id: d
                .admin_disallowed_chat_id
                .map(ChannelId::from_tl)
                .unwrap_or(ChannelId::ZERO),
            disallowed_country: d.disallowed_country.clone().unwrap_or_default(),
            too_early_date: d.joined_too_early_date.unwrap_or(0),
            start_date: d.start_date,
            ..GiveawayInfo::default()
        },
        tl_enums::payments::GiveawayInfo::Results(d) => GiveawayInfo {
            state: if d.refunded {
                GiveawayState::Refunded
            } else {
                GiveawayState::Finished
            },
            gift_code: d.gift_code_slug.clone().unwrap_or_default(),
            activated_count: d.activated_count.unwrap_or(0),
            winners_count: d.winners_count,
            finish_date: d.finish_date,
            start_date: d.start_date,
            credits: d.stars_prize.unwrap_or(0).max(0) as u64,
            ..GiveawayInfo::default()
        },
    }
}

/// Channel/supergroup boost level overview (`Data::BoostsOverview`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoostsOverview {
    /// `true` for megagroup, `false` for broadcast channel.
    pub is_group: bool,
    /// Number of active boosts contributed by the current user.
    pub mine: i32,
    /// Current boost level.
    pub level: i32,
    /// Current total boosts (`max(boosts, current_level_boosts)`).
    pub boost_count: i32,
    /// Boosts required to reach the current level.
    pub current_level_boost_count: i32,
    /// Boosts required for the next level (`0` if max level).
    pub next_level_boost_count: i32,
    /// Estimated Premium subscriber count.
    pub premium_member_count: i32,
    /// Estimated Premium subscriber percentage (`0.0..=100.0`).
    pub premium_member_percentage: f64,
}

/// Gift code link metadata attached to a gift/giveaway boost (`Data::GiftCodeLink`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftCodeLink {
    /// Raw gift code slug.
    pub slug: String,
    /// Shareable `https://t.me/giftcode/<slug>` URL.
    pub link: String,
}

/// Individual boost record (`Data::Boost`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Boost {
    /// Boost ID.
    pub id: String,
    /// Boosting user ID (`UserId::ZERO` if unclaimed).
    pub user_id: UserId,
    /// Source giveaway message ID.
    pub giveaway_message: FullMsgId,
    /// Boost creation timestamp.
    pub date: TimeId,
    /// Boost expiration timestamp.
    pub expires_at: TimeId,
    /// Whole 30-day months between `date` and `expires_at`.
    pub expires_after_months: i32,
    /// Associated gift code link, if any.
    pub gift_code_link: Option<GiftCodeLink>,
    /// Boost multiplier (e.g., 4 for gift/giveaway boosts).
    pub multiplier: i32,
    /// Stars prize for Stars giveaway boosts.
    pub credits: u64,
    /// Boost came from a Premium gift.
    pub is_gift: bool,
    /// Boost came from a giveaway.
    pub is_giveaway: bool,
    /// Giveaway prize is not yet claimed.
    pub is_unclaimed: bool,
}

/// Pagination cursor for `premium.getBoostsList` (`Data::BoostsListSlice::OffsetToken`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoostsOffsetToken {
    /// Server `next_offset` string.
    pub next: String,
    /// `true` to list gift boosts only.
    pub gifts: bool,
}

/// Paginated list of boosts (`Data::BoostsListSlice`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoostsListSlice {
    /// Boost records in this page.
    pub list: Vec<Boost>,
    /// Multiplied total boosts count (`premium.boostsList.count`).
    pub multiplied_total: i32,
    /// `true` when `count == boosts.len()`.
    pub all_loaded: bool,
    /// Cursor for fetching the next page.
    pub token: BoostsOffsetToken,
}

/// Prepaid giveaway ready to launch in a channel (`Data::BoostPrepaidGiveaway`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoostPrepaidGiveaway {
    /// Purchase timestamp.
    pub date: TimeId,
    /// Prepaid giveaway ID.
    pub id: u64,
    /// Stars prize pool (`0` for Premium giveaway).
    pub credits: u64,
    /// Premium months per winner (`0` for Stars giveaway).
    pub months: i32,
    /// Number of winners/prizes.
    pub quantity: i32,
    /// Boosts granted (for Stars giveaways).
    pub boosts: i32,
}

/// Full channel boost status (`Data::BoostStatus`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoostStatus {
    /// Level & progress overview.
    pub overview: BoostsOverview,
    /// Prepaid giveaways available to launch.
    pub prepaid_giveaways: Vec<BoostPrepaidGiveaway>,
    /// Shareable boost URL (`boost_url`).
    pub link: String,
    /// User's boost slot IDs applied to this peer.
    pub my_boost_slots: Vec<i32>,
}

/// One of the current user's Premium boost slots (`myBoost`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MyBoostSlot {
    /// Slot number.
    pub slot: i32,
    /// Currently boosted peer (`None` if free).
    pub peer_id: Option<PeerId>,
    /// Applied timestamp.
    pub date: TimeId,
    /// Expiration timestamp.
    pub expires: TimeId,
    /// Cooldown end timestamp before this slot can be reassigned (`0` if ready).
    pub cooldown_until_date: TimeId,
}

impl MyBoostSlot {
    /// Returns `true` if this slot can be reassigned at timestamp `now`.
    pub const fn can_reassign(&self, now: TimeId) -> bool {
        self.peer_id.is_none() || self.cooldown_until_date <= now
    }
}

/// Converts TL `premium.BoostsStatus` into [`BoostStatus`] (`Boosts::request` in `api_statistics.cpp`).
pub fn boosts_status_from_tl(tl: &tl_enums::premium::BoostsStatus, is_group: bool) -> BoostStatus {
    let tl_enums::premium::BoostsStatus::BoostsStatus(data) = tl;
    let (premium_member_count, premium_member_percentage) = match &data.premium_audience {
        Some(tl_enums::StatsPercentValue::StatsPercentValue(aud)) => {
            let part = (aud.part as i32).max(0);
            let total = (aud.total as i32).max(part);
            let pct = if total > 0 {
                100.0 * (part as f64) / (total as f64)
            } else {
                0.0
            };
            (part, pct)
        }
        None => (0, 0.0),
    };
    let my_boost_slots = data.my_boost_slots.clone().unwrap_or_default();
    let overview = BoostsOverview {
        is_group,
        mine: my_boost_slots.len() as i32,
        level: data.level.max(0),
        boost_count: data.boosts.max(data.current_level_boosts),
        current_level_boost_count: data.current_level_boosts,
        next_level_boost_count: data.next_level_boosts.unwrap_or(0),
        premium_member_count,
        premium_member_percentage,
    };
    let prepaid_giveaways = data
        .prepaid_giveaways
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|p| match p {
            tl_enums::PrepaidGiveaway::PrepaidGiveaway(g) => BoostPrepaidGiveaway {
                date: g.date,
                id: g.id as u64,
                credits: 0,
                months: g.months,
                quantity: g.quantity,
                boosts: 0,
            },
            tl_enums::PrepaidGiveaway::PrepaidStarsGiveaway(g) => BoostPrepaidGiveaway {
                date: g.date,
                id: g.id as u64,
                credits: g.stars.max(0) as u64,
                months: 0,
                quantity: g.quantity,
                boosts: g.boosts,
            },
        })
        .collect();

    BoostStatus {
        overview,
        prepaid_giveaways,
        link: data.boost_url.clone(),
        my_boost_slots,
    }
}

/// Converts TL `premium.BoostsList` into [`BoostsListSlice`] (`Boosts::requestBoosts`).
pub fn boosts_list_from_tl(
    tl: &tl_enums::premium::BoostsList,
    peer_id: PeerId,
    gifts: bool,
) -> BoostsListSlice {
    let tl_enums::premium::BoostsList::BoostsList(data) = tl;
    let list = data
        .boosts
        .iter()
        .map(|tl_enums::Boost::Boost(b)| {
            let gift_code_link = b.used_gift_slug.as_ref().map(|slug| GiftCodeLink {
                slug: slug.clone(),
                link: format!("https://t.me/giftcode/{slug}"),
            });
            let giveaway_message = b
                .giveaway_msg_id
                .map(|mid| FullMsgId::new(peer_id, MsgId::from(mid)))
                .unwrap_or(FullMsgId::ZERO);
            Boost {
                id: b.id.clone(),
                user_id: b.user_id.map(UserId::from_tl).unwrap_or(UserId::ZERO),
                giveaway_message,
                date: b.date,
                expires_at: b.expires,
                expires_after_months: boost_months(b.date, b.expires),
                gift_code_link,
                multiplier: b.multiplier.unwrap_or(0),
                credits: b.stars.unwrap_or(0).max(0) as u64,
                is_gift: b.gift,
                is_giveaway: b.giveaway,
                is_unclaimed: b.unclaimed,
            }
        })
        .collect();

    BoostsListSlice {
        list,
        multiplied_total: data.count,
        all_loaded: data.count as usize == data.boosts.len(),
        token: BoostsOffsetToken {
            next: data.next_offset.clone().unwrap_or_default(),
            gifts,
        },
    }
}

/// Converts TL `premium.MyBoosts` into `Vec<MyBoostSlot>`.
pub fn my_boosts_from_tl(tl: &tl_enums::premium::MyBoosts) -> Vec<MyBoostSlot> {
    let tl_enums::premium::MyBoosts::MyBoosts(data) = tl;
    data.my_boosts
        .iter()
        .map(|tl_enums::MyBoost::MyBoost(m)| MyBoostSlot {
            slot: m.slot,
            peer_id: m.peer.as_ref().map(peer_id_from_tl),
            date: m.date,
            expires: m.expires,
            cooldown_until_date: m.cooldown_until_date.unwrap_or(0),
        })
        .collect()
}

// ============================================================================
// TL Request Builders
// ============================================================================

/// Builds `premium.getBoostsStatus`.
pub fn build_get_boosts_status(peer: tl_enums::InputPeer) -> tl_fn::premium::GetBoostsStatus {
    tl_fn::premium::GetBoostsStatus { peer }
}

/// Builds `premium.getBoostsList`.
pub fn build_get_boosts_list(
    peer: tl_enums::InputPeer,
    token: &BoostsOffsetToken,
    limit: i32,
) -> tl_fn::premium::GetBoostsList {
    tl_fn::premium::GetBoostsList {
        gifts: token.gifts,
        peer,
        offset: token.next.clone(),
        limit,
    }
}

/// Builds `premium.applyBoost`.
pub fn build_apply_boost(
    peer: tl_enums::InputPeer,
    slots: Option<Vec<i32>>,
) -> tl_fn::premium::ApplyBoost {
    tl_fn::premium::ApplyBoost { slots, peer }
}

/// Builds `payments.getGiveawayInfo`.
pub fn build_get_giveaway_info(
    peer: tl_enums::InputPeer,
    msg_id: MsgId,
) -> tl_fn::payments::GetGiveawayInfo {
    tl_fn::payments::GetGiveawayInfo {
        peer,
        msg_id: msg_id.as_i32().unwrap_or(0),
    }
}

/// Builds `payments.checkGiftCode`.
pub fn build_check_gift_code(slug: impl Into<String>) -> tl_fn::payments::CheckGiftCode {
    tl_fn::payments::CheckGiftCode { slug: slug.into() }
}

/// Builds `payments.applyGiftCode`.
pub fn build_apply_gift_code(slug: impl Into<String>) -> tl_fn::payments::ApplyGiftCode {
    tl_fn::payments::ApplyGiftCode { slug: slug.into() }
}
