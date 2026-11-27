//! Telegram Stars (credits) balance, transaction history pagination, top-up options,
//! paid media/reactions/messages costs, subscriptions, and TL conversions.
//!
//! Port of `core/credits_amount.h`, `data/components/credits.h/.cpp`, `data/data_credits.h`,
//! `data/data_credits_earn.h`, `data/data_subscriptions.h`, `api/api_credits.h/.cpp`,
//! `api/api_credits_history_entry.cpp`, and `payments/payments_reaction_process.cpp`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use tg_data::{DocumentId, PeerId, PhotoId, TimeId};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};

use crate::star_gifts::{StarGift, UniqueGift, peer_id_from_tl, star_gift_from_tl};

/// One whole Star or TON expressed in nanos (`kOneStarInNano = 1_000_000_000`).
pub const ONE_STAR_IN_NANO: i64 = 1_000_000_000;
/// Default page size for `payments.getStarsTransactions` (`kTransactionsLimit`).
pub const TRANSACTIONS_LIMIT: i32 = 100;
/// Default max Stars per single paid reaction (`kMaxPerReactionFallback`).
pub const MAX_STARS_PER_REACTION_FALLBACK: i32 = 10_000;
/// Default initial paid reaction slider value (`kDefaultPerReaction`).
pub const DEFAULT_STARS_PER_REACTION: i32 = 50;

/// Credits unit kind (`CreditsType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum CreditsType {
    /// Telegram Stars (`XTR`).
    #[default]
    Stars,
    /// Toncoin (`TON`, stored in nanoton).
    Ton,
}

/// Fixed-point Stars or TON amount (`CreditsAmount` in `core/credits_amount.h`).
///
/// Normalized so that `0 <= nano < ONE_STAR_IN_NANO`, meaning negative values carry a
/// negative `whole` part and non-negative `nano` remainder (identical to C++ `normalize()`).
#[derive(Debug, Clone, Copy, Default)]
pub struct CreditsAmount {
    credits_type: CreditsType,
    whole: i64,
    nano: i64,
}

impl CreditsAmount {
    /// Zero Stars amount.
    pub const ZERO_STARS: Self = Self {
        credits_type: CreditsType::Stars,
        whole: 0,
        nano: 0,
    };

    /// Zero TON amount.
    pub const ZERO_TON: Self = Self {
        credits_type: CreditsType::Ton,
        whole: 0,
        nano: 0,
    };

    /// Creates a whole-unit Stars amount.
    pub const fn stars(whole: i64) -> Self {
        Self {
            credits_type: CreditsType::Stars,
            whole,
            nano: 0,
        }
    }

    /// Creates a whole-unit TON amount.
    pub const fn ton(whole: i64) -> Self {
        Self {
            credits_type: CreditsType::Ton,
            whole,
            nano: 0,
        }
    }

    /// Creates a normalized [`CreditsAmount`] from `(whole, nano, credits_type)`.
    pub fn new(whole: i64, nano: i64, credits_type: CreditsType) -> Self {
        let mut amount = Self {
            credits_type,
            whole,
            nano,
        };
        amount.normalize();
        amount
    }

    /// Creates a TON amount from signed nanoton.
    pub fn from_nanoton(nanoton: i64) -> Self {
        let is_negative = nanoton < 0;
        let abs = nanoton.unsigned_abs();
        let res = Self::new(
            (abs / (ONE_STAR_IN_NANO as u64)) as i64,
            (abs % (ONE_STAR_IN_NANO as u64)) as i64,
            CreditsType::Ton,
        );
        if is_negative {
            Self::ZERO_TON - res
        } else {
            res
        }
    }

    /// Returns the integer whole part.
    pub const fn whole(self) -> i64 {
        self.whole
    }

    /// Returns the fractional nanos part (`0..ONE_STAR_IN_NANO`).
    pub const fn nano(self) -> i64 {
        self.nano
    }

    /// Returns the floating-point value (`whole + nano / 1e9`).
    pub fn value(self) -> f64 {
        (self.whole as f64) + (self.nano as f64) / (ONE_STAR_IN_NANO as f64)
    }

    /// Returns `true` if this amount is denominated in TON.
    pub const fn is_ton(self) -> bool {
        matches!(self.credits_type, CreditsType::Ton)
    }

    /// Returns `true` if this amount is denominated in Stars.
    pub const fn is_stars(self) -> bool {
        matches!(self.credits_type, CreditsType::Stars)
    }

    /// Returns the [`CreditsType`].
    pub const fn credits_type(self) -> CreditsType {
        self.credits_type
    }

    /// Returns `true` if both `whole` and `nano` are zero.
    pub const fn is_empty(self) -> bool {
        self.whole == 0 && self.nano == 0
    }

    /// Returns the absolute value (`CreditsAmount::abs`).
    pub fn abs(self) -> Self {
        if self.whole < 0 {
            Self::new(self.whole.saturating_neg(), -self.nano, self.credits_type)
        } else {
            self
        }
    }

    /// Multiplies this amount by a floating-point rate (`CreditsAmount::multiplied`).
    pub fn multiplied(self, rate: f64) -> Self {
        let result = self.value() * rate;
        let abs = result.abs();
        let whole = abs.floor() as i64;
        let nano = ((abs - (whole as f64)) * (ONE_STAR_IN_NANO as f64)).round() as i64;
        if result < 0.0 {
            Self::new(-whole, -nano, self.credits_type)
        } else {
            Self::new(whole, nano, self.credits_type)
        }
    }

    /// Converts from a TL [`tl_enums::StarsAmount`] (`CreditsAmountFromTL`).
    pub fn from_tl(tl: &tl_enums::StarsAmount) -> Self {
        match tl {
            tl_enums::StarsAmount::StarsAmount(s) => {
                Self::new(s.amount, i64::from(s.nanos), CreditsType::Stars)
            }
            tl_enums::StarsAmount::StarsTonAmount(t) => Self::from_nanoton(t.amount),
        }
    }

    /// Converts to a TL [`tl_enums::StarsAmount`] (`StarsAmountToTL`).
    pub fn to_tl(self) -> tl_enums::StarsAmount {
        if self.is_ton() {
            tl_enums::StarsAmount::StarsTonAmount(tl_types::StarsTonAmount {
                amount: self
                    .whole
                    .wrapping_mul(ONE_STAR_IN_NANO)
                    .wrapping_add(self.nano),
            })
        } else {
            tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                amount: self.whole,
                nanos: self.nano as i32,
            })
        }
    }

    /// Moves whole stars out of `nano` so that `0 <= nano < ONE_STAR_IN_NANO`.
    ///
    /// `div_euclid` / `rem_euclid` give the same result as tdesktop's shift loop without the
    /// `-nano` overflow, and `whole` saturates at the ends of the `i64` range (server
    /// `starsAmount` values are untrusted), like tg-lang's copy.
    fn normalize(&mut self) {
        self.whole = self
            .whole
            .saturating_add(self.nano.div_euclid(ONE_STAR_IN_NANO));
        self.nano = self.nano.rem_euclid(ONE_STAR_IN_NANO);
    }
}

impl PartialEq for CreditsAmount {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for CreditsAmount {}

impl PartialOrd for CreditsAmount {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CreditsAmount {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.whole.cmp(&other.whole) {
            Ordering::Equal => match self.nano.cmp(&other.nano) {
                Ordering::Equal => {
                    if !self.is_empty() {
                        self.credits_type.cmp(&other.credits_type)
                    } else {
                        Ordering::Equal
                    }
                }
                ord => ord,
            },
            ord => ord,
        }
    }
}

impl Add for CreditsAmount {
    type Output = Self;
    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
}

impl AddAssign for CreditsAmount {
    fn add_assign(&mut self, rhs: Self) {
        self.whole = self.whole.saturating_add(rhs.whole);
        self.nano = self.nano.saturating_add(rhs.nano);
        self.normalize();
    }
}

impl Sub for CreditsAmount {
    type Output = Self;
    fn sub(mut self, rhs: Self) -> Self {
        self -= rhs;
        self
    }
}

impl SubAssign for CreditsAmount {
    fn sub_assign(&mut self, rhs: Self) {
        self.whole = self.whole.saturating_sub(rhs.whole);
        self.nano = self.nano.saturating_sub(rhs.nano);
        self.normalize();
    }
}

impl Mul<i64> for CreditsAmount {
    type Output = Self;
    fn mul(mut self, rhs: i64) -> Self {
        self *= rhs;
        self
    }
}

impl MulAssign<i64> for CreditsAmount {
    fn mul_assign(&mut self, rhs: i64) {
        self.whole = self.whole.saturating_mul(rhs);
        self.nano = self.nano.saturating_mul(rhs);
        self.normalize();
    }
}

impl Neg for CreditsAmount {
    type Output = Self;
    fn neg(self) -> Self {
        self * -1
    }
}

/// Balance & lock tracker for Stars and TON (`Data::Credits` in `data/components/credits.cpp`).
#[derive(Debug, Clone, Default)]
pub struct CreditsBalanceTracker {
    balance: CreditsAmount,
    locked: CreditsAmount,
    ton_balance: CreditsAmount,
    loaded: bool,
    ton_loaded: bool,
    stats_enabled: bool,
    peer_balances: BTreeMap<PeerId, CreditsAmount>,
    peer_currency_balances: BTreeMap<PeerId, CreditsAmount>,
}

impl CreditsBalanceTracker {
    /// Creates an empty tracker with zero balances.
    pub fn new() -> Self {
        Self {
            ton_balance: CreditsAmount::ZERO_TON,
            ..Self::default()
        }
    }

    /// Returns whether the self Stars balance has been loaded.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Returns whether the self TON balance has been loaded.
    pub fn ton_loaded(&self) -> bool {
        self.ton_loaded
    }

    /// Returns whether Stars revenue statistics are enabled.
    pub fn stats_enabled(&self) -> bool {
        self.stats_enabled
    }

    /// Sets whether Stars revenue statistics are enabled.
    pub fn set_stats_enabled(&mut self, enabled: bool) {
        self.stats_enabled = enabled;
    }

    /// Returns available (non-locked) Stars balance (`Credits::balance()`).
    pub fn available_balance(&self) -> CreditsAmount {
        if self.balance >= self.locked {
            self.balance - self.locked
        } else {
            CreditsAmount::ZERO_STARS
        }
    }

    /// Returns total Stars balance including locked amount.
    pub fn total_balance(&self) -> CreditsAmount {
        self.balance
    }

    /// Returns currently locked Stars amount.
    pub fn locked_balance(&self) -> CreditsAmount {
        self.locked
    }

    /// Returns self TON balance.
    pub fn ton_balance(&self) -> CreditsAmount {
        self.ton_balance
    }

    /// Returns cached Stars balance for `peer_id`.
    pub fn peer_balance(&self, peer_id: PeerId) -> CreditsAmount {
        self.peer_balances
            .get(&peer_id)
            .copied()
            .unwrap_or(CreditsAmount::ZERO_STARS)
    }

    /// Returns cached TON currency balance for `peer_id`.
    pub fn peer_currency_balance(&self, peer_id: PeerId) -> CreditsAmount {
        self.peer_currency_balances
            .get(&peer_id)
            .copied()
            .unwrap_or(CreditsAmount::ZERO_TON)
    }

    /// Applies a new self Stars or TON balance (`Credits::apply(CreditsAmount)`).
    pub fn apply_balance(&mut self, amount: CreditsAmount) {
        if amount.is_ton() {
            self.ton_balance = amount;
            self.ton_loaded = true;
        } else {
            self.balance = amount;
            self.loaded = true;
        }
    }

    /// Applies an `updateStarsBalance` TL update.
    pub fn apply_update_stars_balance(&mut self, update: &tl_types::UpdateStarsBalance) {
        self.apply_balance(CreditsAmount::from_tl(&update.balance));
    }

    /// Caches a peer's Stars balance (`Credits::apply(PeerId, CreditsAmount)`).
    pub fn apply_peer_balance(&mut self, peer_id: PeerId, balance: CreditsAmount) {
        self.peer_balances.insert(peer_id, balance);
    }

    /// Caches a peer's TON currency balance (`Credits::applyCurrency`).
    pub fn apply_peer_currency_balance(&mut self, peer_id: PeerId, balance: CreditsAmount) {
        self.peer_currency_balances.insert(peer_id, balance);
    }

    /// Locks `count` Stars for an in-flight purchase/reaction (`Credits::lock`).
    ///
    /// Returns `false` if not loaded, `count < 0`, or insufficient available balance.
    pub fn lock(&mut self, count: CreditsAmount) -> bool {
        if !self.loaded || count < CreditsAmount::ZERO_STARS || self.locked + count > self.balance {
            return false;
        }
        self.locked += count;
        true
    }

    /// Releases `count` previously locked Stars (`Credits::unlock`).
    pub fn unlock(&mut self, count: CreditsAmount) -> bool {
        if count < CreditsAmount::ZERO_STARS || self.locked < count {
            return false;
        }
        self.locked -= count;
        true
    }

    /// Consumes `count` locked Stars after a successful spend (`Credits::withdrawLocked`).
    pub fn withdraw_locked(&mut self, count: CreditsAmount) -> bool {
        if count < CreditsAmount::ZERO_STARS || self.locked < count {
            return false;
        }
        self.locked -= count;
        let next = if self.balance >= count {
            self.balance - count
        } else {
            CreditsAmount::ZERO_STARS
        };
        self.apply_balance(next);
        true
    }
}

/// Top-up or gift Stars pack option (`Data::CreditTopupOption`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreditTopupOption {
    /// Number of Stars.
    pub credits: u64,
    /// Optional store product ID.
    pub product: String,
    /// ISO-4217 currency code.
    pub currency: String,
    /// Price in minor currency units.
    pub amount: u64,
    /// Whether this option belongs to the expanded list.
    pub extended: bool,
    /// Non-zero recipient bare peer ID if gifting Stars.
    pub gift_bare_peer_id: u64,
}

/// Stars giveaway winner distribution preset (`Data::CreditsGiveawayOption::Winner`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreditsGiveawayWinnerOption {
    /// Number of winners.
    pub users: i32,
    /// Stars awarded per winner.
    pub per_user_stars: u64,
    /// Whether this preset is the default.
    pub is_default: bool,
}

/// Stars giveaway package option (`Data::CreditsGiveawayOption`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreditsGiveawayOption {
    /// Available winner splits.
    pub winners: Vec<CreditsGiveawayWinnerOption>,
    /// Optional store product ID.
    pub store_product: String,
    /// ISO-4217 currency code.
    pub currency: String,
    /// Price in minor currency units.
    pub amount: u64,
    /// Total Stars pool.
    pub credits: u64,
    /// Yearly boosts granted to the channel.
    pub yearly_boosts: i32,
    /// Whether this option is extended.
    pub is_extended: bool,
    /// Whether this option is the default selection.
    pub is_default: bool,
}

/// Recurring Stars subscription pricing (`Data::PeerSubscription`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerSubscription {
    /// Stars charged per billing period.
    pub credits: u64,
    /// Billing period duration in seconds.
    pub period: i32,
}

impl PeerSubscription {
    /// Returns `true` if both `credits` and `period` are positive.
    pub const fn is_valid(self) -> bool {
        self.credits > 0 && self.period > 0
    }
}

/// Active or past Stars subscription (`Data::SubscriptionEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscriptionEntry {
    /// Subscription ID.
    pub id: String,
    /// Channel invite hash if this is a paid invite-link subscription.
    pub invite_hash: String,
    /// Subscription title.
    pub title: String,
    /// Invoice slug for re-fulfilling.
    pub slug: String,
    /// Next renewal / expiration Unix timestamp.
    pub until: TimeId,
    /// Pricing terms.
    pub subscription: PeerSubscription,
    /// Target peer ID.
    pub peer_id: PeerId,
    /// Whether cancelled by user.
    pub cancelled: bool,
    /// Whether cancelled by bot.
    pub cancelled_by_bot: bool,
    /// Whether `now > until`.
    pub expired: bool,
    /// Whether the subscription can be re-fulfilled.
    pub can_refulfill: bool,
}

/// Extended paid media kind inside a Stars transaction (`Data::CreditsHistoryMediaType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditsHistoryMediaType {
    /// Paid photo.
    Photo,
    /// Paid video / animation.
    Video,
}

/// Extended paid media item inside a Stars transaction (`Data::CreditsHistoryMedia`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditsHistoryMedia {
    /// Photo or video.
    pub media_type: CreditsHistoryMediaType,
    /// Photo ID or document ID.
    pub id: u64,
}

/// Counterparty category for a Stars transaction (`Data::CreditsHistoryEntry::PeerType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreditsHistoryPeerType {
    /// Regular Telegram peer (user/channel/bot).
    #[default]
    Peer,
    /// Apple App Store.
    AppStore,
    /// Google Play Market.
    PlayMarket,
    /// Fragment platform.
    Fragment,
    /// Unsupported/unknown source.
    Unsupported,
    /// PremiumBot.
    PremiumBot,
    /// Telegram Ads.
    Ads,
    /// Telegram API / paid floodskip.
    Api,
}

/// One entry in the Stars/TON transaction ledger (`Data::CreditsHistoryEntry`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreditsHistoryEntry {
    /// Transaction ID.
    pub id: String,
    /// Optional title.
    pub title: String,
    /// Optional description.
    pub description: String,
    /// Transaction Unix timestamp (or `ads_proceeds_from_date` if present).
    pub date: TimeId,
    /// Paid media items attached to this transaction.
    pub extended: Vec<CreditsHistoryMedia>,
    /// Signed transaction amount.
    pub credits: CreditsAmount,
    /// Associated message ID (`0` if none).
    pub bare_msg_id: u64,
    /// Associated peer ID.
    pub peer_id: PeerId,
    /// Giveaway post message ID (`0` if none).
    pub bare_giveaway_msg_id: u64,
    /// Gift sticker document ID (`0` if none).
    pub bare_gift_sticker_id: DocumentId,
    /// Sender/actor peer ID for incoming reactions / paid media / paid messages.
    pub actor_id: PeerId,
    /// Parsed unique gift metadata, if this transaction involved a collectible gift.
    pub unique_gift: Option<UniqueGift>,
    /// Affiliate program payout amount.
    pub starref_amount: CreditsAmount,
    /// Affiliate commission in permille.
    pub starref_commission: i32,
    /// Affiliate recipient peer ID.
    pub starref_recipient_id: PeerId,
    /// Counterparty category.
    pub peer_type: CreditsHistoryPeerType,
    /// Subscription period in seconds (`0` if none).
    pub subscription_period: i32,
    /// Ads proceeds end timestamp (`0` if none).
    pub ads_proceeds_to_date: TimeId,
    /// External blockchain/settlement timestamp (`0` if none).
    pub success_date: TimeId,
    /// External transaction URL.
    pub success_link: String,
    /// Number of paid messages covered by this transaction.
    pub paid_messages_count: i32,
    /// Commission/net amount for paid messages.
    pub paid_messages_amount: CreditsAmount,
    /// Commission permille for paid messages.
    pub paid_messages_commission: i32,
    /// Gift limited supply total.
    pub limited_count: i32,
    /// Gift limited supply remaining.
    pub limited_left: i32,
    /// Stars received when converting a non-unique gift.
    pub stars_converted: i64,
    /// Gifted Telegram Premium months paid with Stars.
    pub premium_months_for_stars: i32,
    /// Paid API floodskip broadcast count.
    pub flood_skip: i32,
    /// Gift was converted into Stars.
    pub converted: bool,
    /// Transaction involves a Star Gift.
    pub stargift: bool,
    /// Paid global posts search.
    pub posts_search: bool,
    /// Star gift upgrade payment.
    pub gift_upgraded: bool,
    /// Star gift resale payment.
    pub gift_resale: bool,
    /// Star gift purchase offer.
    pub gift_offer: bool,
    /// Paid reaction.
    pub reaction: bool,
    /// Refunded transaction.
    pub refunded: bool,
    /// Pending settlement.
    pub pending: bool,
    /// Failed settlement.
    pub failed: bool,
    /// Incoming (`amount >= 0`).
    pub incoming: bool,
    /// Gift flag.
    pub gift: bool,
}

impl CreditsHistoryEntry {
    /// Returns `true` for live-story paid reactions (`isLiveStoryReaction`).
    pub const fn is_live_story_reaction(&self) -> bool {
        self.paid_messages_count > 0 && self.reaction && self.bare_msg_id == 0
    }
}

/// Paginated slice returned by `payments.getStarsStatus` / `getStarsTransactions` /
/// `getStarsSubscriptions` (`Data::CreditsStatusSlice`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreditsStatusSlice {
    /// Transactions in this page.
    pub list: Vec<CreditsHistoryEntry>,
    /// Subscriptions in this page.
    pub subscriptions: Vec<SubscriptionEntry>,
    /// Current balance.
    pub balance: CreditsAmount,
    /// Missing balance needed for upcoming renewals.
    pub subscriptions_missing_balance: u64,
    /// `true` when neither `next_offset` nor `subscriptions_next_offset` is present.
    pub all_loaded: bool,
    /// Next pagination offset for transactions.
    pub token: String,
    /// Next pagination offset for subscriptions.
    pub token_subscriptions: String,
}

/// Stars/TON channel or bot earnings statistics (`Data::CreditsEarnStatistics`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreditsEarnStatistics {
    /// Current balance.
    pub current_balance: CreditsAmount,
    /// Balance available for withdrawal.
    pub available_balance: CreditsAmount,
    /// All-time revenue.
    pub overall_revenue: CreditsAmount,
    /// USD conversion rate.
    pub usd_rate: f64,
    /// Whether withdrawal is enabled.
    pub is_withdrawal_enabled: bool,
    /// Next withdrawal availability Unix timestamp (`0` if immediate or unavailable).
    pub next_withdrawal_at: TimeId,
    /// Optional Telegram Ads purchase URL.
    pub buy_ads_url: String,
}

/// Paid reaction configuration & clamping helpers (`payments/payments_reaction_process.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaidReactionConfig {
    /// Maximum Stars allowed per single reaction (`stars_paid_reaction_amount_max`).
    pub max_per_reaction: i32,
}

impl Default for PaidReactionConfig {
    fn default() -> Self {
        Self {
            max_per_reaction: MAX_STARS_PER_REACTION_FALLBACK,
        }
    }
}

impl PaidReactionConfig {
    /// Creates a config with `max_per_reaction` clamped to at least 2 (matching C++ `std::max(..., 2)`).
    pub fn new(app_config_max: i32) -> Self {
        Self {
            max_per_reaction: app_config_max.max(2),
        }
    }

    /// Default initial chosen count clamped into `1..=max_per_reaction`.
    pub fn default_chosen(self) -> i32 {
        DEFAULT_STARS_PER_REACTION.clamp(1, self.max_per_reaction)
    }

    /// Clamps a user-entered reaction amount into `1..=max_per_reaction`.
    pub fn clamp_amount(self, count: i32) -> i32 {
        count.clamp(1, self.max_per_reaction)
    }
}

/// Per-peer messaging cost / Premium gate (`Api::MessageMoneyRestriction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageMoneyRestriction {
    /// Stars charged per message (`0` if free).
    pub stars_per_message: i32,
    /// Whether Telegram Premium is required to write to this user.
    pub premium_required: bool,
    /// Whether restriction state has been resolved from server/history.
    pub known: bool,
}

impl MessageMoneyRestriction {
    /// Returns `true` if either Stars payment or Premium is required.
    pub const fn is_restricted(self) -> bool {
        self.stars_per_message != 0 || self.premium_required
    }

    /// Computes total Stars cost for sending `message_count` messages (e.g., an album).
    pub const fn cost_for_messages(self, message_count: i32) -> i64 {
        if self.stars_per_message <= 0 || message_count <= 0 {
            0
        } else {
            (self.stars_per_message as i64) * (message_count as i64)
        }
    }
}

/// Cost summary for paid media posts (`inputMediaPaidMedia` / `messageMediaPaidMedia`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaidMediaCost {
    /// Stars required to unlock the media bundle.
    pub stars_amount: i64,
    /// Number of media attachments included.
    pub media_count: usize,
    /// Whether the current user has already unlocked it.
    pub unlocked: bool,
}

/// Filter for `payments.getStarsTransactions` (`Api::CreditsHistory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CreditsHistoryFilter {
    /// Include incoming transactions (`inbound`).
    pub inbound: bool,
    /// Include outgoing transactions (`outbound`).
    pub outbound: bool,
    /// Query TON ledger instead of Stars (`ton`).
    pub ton: bool,
    /// Sort oldest-first (`ascending`).
    pub ascending: bool,
}

/// Stateful paginator accumulating [`CreditsStatusSlice`] pages without duplicate IDs.
#[derive(Debug, Clone, Default)]
pub struct CreditsHistoryPaginator {
    entries: Vec<CreditsHistoryEntry>,
    seen_ids: HashSet<String>,
    next_offset: String,
    all_loaded: bool,
    balance: CreditsAmount,
}

impl CreditsHistoryPaginator {
    /// Creates a new empty paginator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns accumulated transaction entries in fetch order.
    pub fn entries(&self) -> &[CreditsHistoryEntry] {
        &self.entries
    }

    /// Returns the next offset token to request (`""` for the first page).
    pub fn next_offset(&self) -> &str {
        &self.next_offset
    }

    /// Returns `true` when all pages have been loaded.
    pub fn all_loaded(&self) -> bool {
        self.all_loaded
    }

    /// Returns the latest reported balance.
    pub fn balance(&self) -> CreditsAmount {
        self.balance
    }

    /// Resets pagination state for a fresh reload.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.seen_ids.clear();
        self.next_offset.clear();
        self.all_loaded = false;
    }

    /// Appends a newly fetched [`CreditsStatusSlice`], deduplicating by transaction `id`.
    pub fn apply_slice(&mut self, slice: CreditsStatusSlice) {
        self.balance = slice.balance;
        for entry in slice.list {
            if self.seen_ids.insert(entry.id.clone()) {
                self.entries.push(entry);
            }
        }
        self.next_offset = slice.token;
        self.all_loaded = slice.all_loaded || self.next_offset.is_empty();
    }
}

// ============================================================================
// TL Conversions & Request Builders
// ============================================================================

/// Builds `payments.getStarsStatus`.
pub fn build_get_stars_status(
    peer: tl_enums::InputPeer,
    ton: bool,
) -> tl_fn::payments::GetStarsStatus {
    tl_fn::payments::GetStarsStatus { ton, peer }
}

/// Builds `payments.getStarsTransactions` matching `Api::CreditsHistory::request`.
///
/// When `filter.inbound == filter.outbound`, neither `inbound` nor `outbound` flag is set
/// (matching tdesktop's `(in == out) ? Flags(0) : ...`).
pub fn build_get_stars_transactions(
    peer: tl_enums::InputPeer,
    filter: CreditsHistoryFilter,
    offset: impl Into<String>,
    limit: i32,
    subscription_id: Option<String>,
) -> tl_fn::payments::GetStarsTransactions {
    let (inbound, outbound) = if filter.inbound == filter.outbound {
        (false, false)
    } else {
        (filter.inbound, filter.outbound)
    };
    tl_fn::payments::GetStarsTransactions {
        inbound,
        outbound,
        ascending: filter.ascending,
        ton: filter.ton,
        subscription_id,
        peer,
        offset: offset.into(),
        limit: if limit > 0 { limit } else { TRANSACTIONS_LIMIT },
    }
}

/// Builds `payments.getStarsSubscriptions`.
pub fn build_get_stars_subscriptions(
    peer: tl_enums::InputPeer,
    offset: impl Into<String>,
    missing_balance: bool,
) -> tl_fn::payments::GetStarsSubscriptions {
    tl_fn::payments::GetStarsSubscriptions {
        missing_balance,
        peer,
        offset: offset.into(),
    }
}

/// Builds `payments.changeStarsSubscription` (`EditCreditsSubscription`).
pub fn build_change_stars_subscription(
    subscription_id: impl Into<String>,
    canceled: bool,
) -> tl_fn::payments::ChangeStarsSubscription {
    tl_fn::payments::ChangeStarsSubscription {
        peer: tl_enums::InputPeer::Self_,
        subscription_id: subscription_id.into(),
        canceled: Some(canceled),
    }
}

/// Builds `payments.getStarsGiftOptions` (`CreditsTopupOptions::request` in `api/api_credits.cpp:131-135`).
///
/// Pass `Some(input_user)` to fetch Stars gift packs tailored for that recipient (`flags.0`),
/// or `None` for generic gift options.
pub fn build_get_stars_gift_options(
    user_id: Option<tl_enums::InputUser>,
) -> tl_fn::payments::GetStarsGiftOptions {
    tl_fn::payments::GetStarsGiftOptions { user_id }
}

/// Converts a TL `StarsSubscription` into [`SubscriptionEntry`] (`SubscriptionFromTL`).
pub fn stars_subscription_from_tl(
    tl: &tl_enums::StarsSubscription,
    now: TimeId,
) -> SubscriptionEntry {
    let tl_enums::StarsSubscription::StarsSubscription(s) = tl;
    let tl_enums::StarsSubscriptionPricing::StarsSubscriptionPricing(ref pricing) = s.pricing;
    SubscriptionEntry {
        id: s.id.clone(),
        invite_hash: s.chat_invite_hash.clone().unwrap_or_default(),
        title: s.title.clone().unwrap_or_default(),
        slug: s.invoice_slug.clone().unwrap_or_default(),
        until: s.until_date,
        subscription: PeerSubscription {
            credits: pricing.amount.max(0) as u64,
            period: pricing.period,
        },
        peer_id: peer_id_from_tl(&s.peer),
        cancelled: s.canceled,
        cancelled_by_bot: s.bot_canceled,
        expired: now > s.until_date,
        can_refulfill: s.can_refulfill,
    }
}

/// Converts a TL `StarsTransaction` into [`CreditsHistoryEntry`] (`CreditsHistoryEntryFromTL`).
pub fn stars_transaction_from_tl(
    tl: &tl_enums::StarsTransaction,
    owner_peer_id: PeerId,
) -> CreditsHistoryEntry {
    let tl_enums::StarsTransaction::StarsTransaction(t) = tl;

    let mut extended = Vec::new();
    if let Some(ref media_list) = t.extended_media {
        for media in media_list {
            match media {
                tl_enums::MessageMedia::Photo(p) => {
                    if let Some(tl_enums::Photo::Photo(photo)) = &p.photo {
                        extended.push(CreditsHistoryMedia {
                            media_type: CreditsHistoryMediaType::Photo,
                            id: PhotoId::from_tl(photo.id).raw(),
                        });
                    }
                }
                tl_enums::MessageMedia::Document(d) => {
                    if let Some(tl_enums::Document::Document(doc)) = &d.document {
                        extended.push(CreditsHistoryMedia {
                            media_type: CreditsHistoryMediaType::Video,
                            id: DocumentId::from_tl(doc.id).raw(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let (peer_type, tx_peer_id) = match &t.peer {
        tl_enums::StarsTransactionPeer::StarsTransactionPeer(p) => {
            (CreditsHistoryPeerType::Peer, peer_id_from_tl(&p.peer))
        }
        tl_enums::StarsTransactionPeer::PlayMarket => {
            (CreditsHistoryPeerType::PlayMarket, PeerId::ZERO)
        }
        tl_enums::StarsTransactionPeer::Fragment => {
            (CreditsHistoryPeerType::Fragment, PeerId::ZERO)
        }
        tl_enums::StarsTransactionPeer::AppStore => {
            (CreditsHistoryPeerType::AppStore, PeerId::ZERO)
        }
        tl_enums::StarsTransactionPeer::Unsupported => {
            (CreditsHistoryPeerType::Unsupported, PeerId::ZERO)
        }
        tl_enums::StarsTransactionPeer::PremiumBot => {
            (CreditsHistoryPeerType::PremiumBot, PeerId::ZERO)
        }
        tl_enums::StarsTransactionPeer::Ads => (CreditsHistoryPeerType::Ads, PeerId::ZERO),
        tl_enums::StarsTransactionPeer::API => (CreditsHistoryPeerType::Api, PeerId::ZERO),
    };

    let amount = CreditsAmount::from_tl(&t.amount);
    let starref_amount = t
        .starref_amount
        .as_ref()
        .map(CreditsAmount::from_tl)
        .unwrap_or_default();
    let starref_commission = t.starref_commission_permille.unwrap_or(0);
    let starref_peer_id = t
        .starref_peer
        .as_ref()
        .map(peer_id_from_tl)
        .unwrap_or(PeerId::ZERO);

    let incoming = amount >= CreditsAmount::ZERO_STARS;
    let paid_messages_count = t.paid_messages.unwrap_or(0);
    let save_actor_id = (t.reaction || !extended.is_empty() || paid_messages_count > 0) && incoming;

    let parsed_gift: Option<StarGift> = t.stargift.as_ref().map(star_gift_from_tl);
    let stars_converted = match &t.stargift {
        Some(tl_enums::StarGift::StarGift(g)) => g.convert_stars,
        _ => 0,
    };

    CreditsHistoryEntry {
        id: t.id.clone(),
        title: t.title.clone().unwrap_or_default(),
        description: t.description.clone().unwrap_or_default(),
        date: t.ads_proceeds_from_date.unwrap_or(t.date),
        extended,
        credits: amount,
        bare_msg_id: t.msg_id.unwrap_or(0).max(0) as u64,
        peer_id: if save_actor_id {
            owner_peer_id
        } else {
            tx_peer_id
        },
        bare_giveaway_msg_id: t.giveaway_post_id.unwrap_or(0).max(0) as u64,
        bare_gift_sticker_id: parsed_gift
            .as_ref()
            .map(|g| g.sticker_document_id)
            .unwrap_or(DocumentId::ZERO),
        actor_id: if save_actor_id {
            tx_peer_id
        } else {
            PeerId::ZERO
        },
        unique_gift: parsed_gift.as_ref().and_then(|g| g.unique.clone()),
        starref_amount: if paid_messages_count > 0 {
            CreditsAmount::ZERO_STARS
        } else {
            starref_amount
        },
        starref_commission: if paid_messages_count > 0 {
            0
        } else {
            starref_commission
        },
        starref_recipient_id: if paid_messages_count > 0 {
            PeerId::ZERO
        } else {
            starref_peer_id
        },
        peer_type,
        subscription_period: t.subscription_period.unwrap_or(0),
        ads_proceeds_to_date: t.ads_proceeds_to_date.unwrap_or(0),
        success_date: t.transaction_date.unwrap_or(0),
        success_link: t.transaction_url.clone().unwrap_or_default(),
        paid_messages_count,
        paid_messages_amount: if paid_messages_count > 0 {
            starref_amount
        } else {
            CreditsAmount::ZERO_STARS
        },
        paid_messages_commission: if paid_messages_count > 0 {
            starref_commission
        } else {
            0
        },
        limited_count: parsed_gift.as_ref().map(|g| g.limited_count).unwrap_or(0),
        limited_left: parsed_gift.as_ref().map(|g| g.limited_left).unwrap_or(0),
        stars_converted,
        premium_months_for_stars: t.premium_gift_months.unwrap_or(0),
        flood_skip: t.floodskip_number.unwrap_or(0),
        converted: t.stargift.is_some() && incoming,
        stargift: t.stargift.is_some(),
        posts_search: t.posts_search,
        gift_upgraded: t.stargift_upgrade,
        gift_resale: t.stargift_resale,
        gift_offer: t.offer,
        reaction: t.reaction,
        refunded: t.refund,
        pending: t.pending,
        failed: t.failed,
        incoming,
        gift: t.gift || t.stargift.is_some(),
    }
}

/// Converts a TL `payments.StarsStatus` into [`CreditsStatusSlice`] (`StatusFromTL`).
pub fn stars_status_from_tl(
    tl: &tl_enums::payments::StarsStatus,
    owner_peer_id: PeerId,
    now: TimeId,
) -> CreditsStatusSlice {
    let tl_enums::payments::StarsStatus::StarsStatus(s) = tl;
    let list = s
        .history
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|tx| stars_transaction_from_tl(tx, owner_peer_id))
        .collect();
    let subscriptions = s
        .subscriptions
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|sub| stars_subscription_from_tl(sub, now))
        .collect();
    CreditsStatusSlice {
        list,
        subscriptions,
        balance: CreditsAmount::from_tl(&s.balance),
        subscriptions_missing_balance: s.subscriptions_missing_balance.unwrap_or(0).max(0) as u64,
        all_loaded: s.next_offset.is_none() && s.subscriptions_next_offset.is_none(),
        token: s.next_offset.clone().unwrap_or_default(),
        token_subscriptions: s.subscriptions_next_offset.clone().unwrap_or_default(),
    }
}

/// Converts TL `Vector<StarsTopupOption>` into `Vec<CreditTopupOption>`.
pub fn stars_topup_options_from_tl(
    options: &[tl_enums::StarsTopupOption],
) -> Vec<CreditTopupOption> {
    options
        .iter()
        .map(
            |tl_enums::StarsTopupOption::StarsTopupOption(o)| CreditTopupOption {
                credits: o.stars.max(0) as u64,
                product: o.store_product.clone().unwrap_or_default(),
                currency: o.currency.clone(),
                amount: o.amount.max(0) as u64,
                extended: o.extended,
                gift_bare_peer_id: 0,
            },
        )
        .collect()
}

/// Converts TL `Vector<StarsGiftOption>` into `Vec<CreditTopupOption>`.
pub fn stars_gift_options_from_tl(
    options: &[tl_enums::StarsGiftOption],
    gift_peer_id: PeerId,
) -> Vec<CreditTopupOption> {
    options
        .iter()
        .map(
            |tl_enums::StarsGiftOption::StarsGiftOption(o)| CreditTopupOption {
                credits: o.stars.max(0) as u64,
                product: o.store_product.clone().unwrap_or_default(),
                currency: o.currency.clone(),
                amount: o.amount.max(0) as u64,
                extended: o.extended,
                gift_bare_peer_id: gift_peer_id.raw(),
            },
        )
        .collect()
}

/// Converts TL `Vector<StarsGiveawayOption>` into `Vec<CreditsGiveawayOption>`.
pub fn stars_giveaway_options_from_tl(
    options: &[tl_enums::StarsGiveawayOption],
) -> Vec<CreditsGiveawayOption> {
    options
        .iter()
        .map(
            |tl_enums::StarsGiveawayOption::StarsGiveawayOption(o)| CreditsGiveawayOption {
                winners: o
                    .winners
                    .iter()
                    .map(
                        |tl_enums::StarsGiveawayWinnersOption::StarsGiveawayWinnersOption(w)| {
                            CreditsGiveawayWinnerOption {
                                users: w.users,
                                per_user_stars: w.per_user_stars.max(0) as u64,
                                is_default: w.default,
                            }
                        },
                    )
                    .collect(),
                store_product: o.store_product.clone().unwrap_or_default(),
                currency: o.currency.clone(),
                amount: o.amount.max(0) as u64,
                credits: o.stars.max(0) as u64,
                yearly_boosts: o.yearly_boosts,
                is_extended: o.extended,
                is_default: o.default,
            },
        )
        .collect()
}

/// Converts TL `payments.StarsRevenueStats` into [`CreditsEarnStatistics`].
pub fn stars_revenue_stats_from_tl(
    tl: &tl_enums::payments::StarsRevenueStats,
    buy_ads_url: String,
) -> CreditsEarnStatistics {
    let tl_enums::payments::StarsRevenueStats::StarsRevenueStats(stats) = tl;
    let tl_enums::StarsRevenueStatus::StarsRevenueStatus(ref status) = stats.status;
    CreditsEarnStatistics {
        current_balance: CreditsAmount::from_tl(&status.current_balance),
        available_balance: CreditsAmount::from_tl(&status.available_balance),
        overall_revenue: CreditsAmount::from_tl(&status.overall_revenue),
        usd_rate: stats.usd_rate,
        is_withdrawal_enabled: status.withdrawal_enabled,
        next_withdrawal_at: status.next_withdrawal_at.unwrap_or(0),
        buy_ads_url,
    }
}
