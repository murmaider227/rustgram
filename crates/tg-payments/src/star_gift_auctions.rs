//! Star Gift auctions state, bid levels, rounds, active auctions hashing,
//! and Layer 233 TL conversions and request builders.
//!
//! Port of `data/components/gift_auctions.h`, `data/components/gift_auctions.cpp`,
//! and `boxes/star_gift_auction_box.cpp`.

use tg_data::{PeerId, TimeId, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};

use crate::star_gifts::{StarGift, peer_id_from_tl, star_gift_from_tl};

/// Default maximum shown bid slider ceiling (`kMaxShownBid` in `star_gift_auction_box.cpp`).
pub const AUCTION_MAX_SHOWN_BID: i64 = 30_000;
/// Number of top bidder places highlighted (`kShowTopPlaces`).
pub const AUCTION_TOP_PLACES: usize = 3;

/// One bid level in a Star Gift auction (`Data::GiftAuctionBidLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GiftAuctionBidLevel {
    /// Bid amount in Stars.
    pub amount: i64,
    /// Leaderboard rank (1-based).
    pub position: i32,
    /// Timestamp when this bid was placed.
    pub date: TimeId,
}

/// Current user's bid state in a Star Gift auction (`Data::StarGiftAuctionMyState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarGiftAuctionMyState {
    /// Recipient peer ID for the bid (`PeerId::ZERO` if self/unset).
    pub to_peer_id: PeerId,
    /// Minimum bid amount required for the user to place/raise a bid.
    pub min_bid_amount: i64,
    /// User's current active bid in Stars (`0` if not bidding).
    pub bid: i64,
    /// Timestamp of user's active bid (`0` if not bidding).
    pub date: TimeId,
    /// Number of gifts already acquired by the user in earlier rounds.
    pub got_count: i32,
    /// Whether the user's bid was returned/outbid.
    pub returned: bool,
}

/// Round schedule descriptor for an auction (`Data::GiftAuctionRound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GiftAuctionRound {
    /// Round number (1-based).
    pub number: i32,
    /// Base round duration in seconds.
    pub duration: TimeId,
    /// Top-N positions whose bids extend the round (`0` for non-extendable rounds).
    pub extend_top: i32,
    /// Extension window in seconds (`0` for non-extendable rounds).
    pub extend_duration: TimeId,
}

impl GiftAuctionRound {
    /// Returns `true` if late bids in the top ranks can extend this round.
    pub const fn is_extendable(&self) -> bool {
        self.extend_top > 0 && self.extend_duration > 0
    }
}

/// Live or finished Star Gift auction state (`Data::GiftAuctionState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftAuctionState {
    /// Auctioned gift catalog item.
    pub gift: Option<StarGift>,
    /// Current user's bid and acquisition state.
    pub my: StarGiftAuctionMyState,
    /// Ranked bid levels from highest to lowest.
    pub bid_levels: Vec<GiftAuctionBidLevel>,
    /// User IDs of top bidders.
    pub top_bidders: Vec<UserId>,
    /// Per-round schedule and extension parameters.
    pub round_parameters: Vec<GiftAuctionRound>,
    /// Minimum bid amount in Stars for the current round.
    pub min_bid_amount: i64,
    /// Final average sale price in Stars (`> 0` when finished).
    pub average_price: i64,
    /// Auction start Unix timestamp.
    pub start_date: TimeId,
    /// Auction end Unix timestamp.
    pub end_date: TimeId,
    /// Next round boundary Unix timestamp.
    pub next_round_at: TimeId,
    /// Last assigned gift serial number.
    pub last_gift_num: i32,
    /// Current round index (1-based).
    pub current_round: i32,
    /// Total rounds in the auction.
    pub total_rounds: i32,
    /// Gifts remaining across remaining rounds.
    pub gifts_left: i32,
    /// Monotonic state version used for incremental polling.
    pub version: i32,
    /// Server-recommended polling timeout in seconds.
    pub timeout: i32,
    /// Resale listings count (for finished auctions).
    pub listed_count: i32,
    /// Fragment resale listings count (for finished auctions).
    pub fragment_listed_count: i32,
    /// Fragment listing URL (for finished auctions).
    pub fragment_listed_url: String,
}

impl GiftAuctionState {
    /// Returns `true` if the auction has concluded (`GiftAuctionState::finished`).
    pub const fn is_finished(&self) -> bool {
        self.average_price != 0
    }

    /// Computes the current user's leaderboard rank (1-based) matching `Data::MyAuctionPosition`.
    pub fn my_position(&self) -> i32 {
        for level in &self.bid_levels {
            if level.amount < self.my.bid
                || (level.amount == self.my.bid && level.date >= self.my.date)
            {
                return level.position;
            }
        }
        self.bid_levels.last().map_or(0, |l| l.position) + 1
    }

    /// Gifts awarded per round (falls back to `1` if gift metadata is absent).
    pub fn gifts_per_round(&self) -> i32 {
        self.gift
            .as_ref()
            .map(|g| g.auction_gifts_per_round)
            .filter(|&n| n > 0)
            .unwrap_or(1)
    }

    /// Returns `true` if the user has an active bid inside the winning cutoff for this round.
    pub fn is_my_bid_winning(&self) -> bool {
        self.my.bid > 0 && !self.my.returned && self.my_position() <= self.gifts_per_round()
    }

    /// Minimum bid required to be in the winning cutoff for the current round.
    pub fn min_winning_bid(&self) -> i64 {
        let per_round = self.gifts_per_round();
        let mut min_win = 0_i64;
        for level in &self.bid_levels {
            if level.position > per_round {
                break;
            }
            if min_win == 0 || level.amount < min_win {
                min_win = level.amount;
            }
        }
        if min_win > 0 {
            min_win
        } else {
            self.min_bid_amount.max(self.my.min_bid_amount)
        }
    }

    /// Minimum allowed next bid amount when placing or raising a bid.
    pub fn effective_min_bid(&self) -> i64 {
        self.min_bid_amount.max(self.my.min_bid_amount).max(1)
    }
}

/// One gift acquired by the user in an auction round (`Data::GiftAcquired`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftAuctionAcquiredGift {
    /// Recipient peer ID.
    pub to_peer_id: PeerId,
    /// Optional gift caption text.
    pub message: String,
    /// Acquisition Unix timestamp.
    pub date: TimeId,
    /// Winning bid amount in Stars.
    pub bid_amount: i64,
    /// Round in which the gift was won.
    pub round: i32,
    /// Assigned gift serial number (`0` if unspecified).
    pub number: i32,
    /// Winning rank position in that round.
    pub position: i32,
    /// Whether sender name was hidden.
    pub name_hidden: bool,
}

/// Snapshot of active auctions the user is participating in (`Data::ActiveAuctions`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActiveAuctionsSnapshot {
    /// Active auctions list.
    pub auctions: Vec<GiftAuctionState>,
}

impl ActiveAuctionsSnapshot {
    /// Returns `true` if any active auction currently has the user outbid.
    pub fn any_outbid(&self) -> bool {
        self.auctions
            .iter()
            .any(|a| !a.is_finished() && a.my.bid > 0 && !a.is_my_bid_winning())
    }
}

/// Computes Telegram's 64-bit hash over active auctions (`GiftAuctions::countActiveHash`).
///
/// Entries are sorted by `((my.date as u32) << 32) | (version as u32)` before hashing
/// `(version, my.date)` with `Api::HashUpdate`.
pub fn compute_active_auctions_hash(auctions: &[GiftAuctionState]) -> i64 {
    let mut entries: Vec<(i32, i32)> = auctions
        .iter()
        .filter(|a| a.gift.is_some() && a.my.date != 0)
        .map(|a| (a.version, a.my.date))
        .collect();
    entries.sort_by_key(|&(version, date)| ((date as u32 as u64) << 32) | (version as u32 as u64));

    let mut hash: u64 = 0;
    for (version, date) in entries {
        hash_update_i32(&mut hash, version);
        hash_update_i32(&mut hash, date);
    }
    hash as i64
}

fn hash_update_u64(already: &mut u64, value: u64) {
    *already ^= *already >> 21;
    *already ^= *already << 35;
    *already ^= *already >> 4;
    *already = already.wrapping_add(value);
}

fn hash_update_i32(already: &mut u64, value: i32) {
    hash_update_u64(already, value as i64 as u64);
}

/// Formats an auction countdown duration in seconds (`NiceCountdownText` in `star_gift_auction_box.cpp`).
pub fn format_auction_countdown(seconds: i32) -> String {
    let clamped = seconds.max(0);
    let minutes = clamped / 60;
    let hours = minutes / 60;
    if hours > 0 {
        format!("{hours}:{:02}:{:02}", minutes % 60, clamped % 60)
    } else {
        format!("{minutes}:{:02}", clamped % 60)
    }
}

// ============================================================================
// TL Conversions & Request Builders
// ============================================================================

/// Applies a TL [`tl_enums::StarGiftAuctionState`] onto `entry` (`GiftAuctions::apply`).
pub fn apply_auction_state_tl(
    entry: &mut GiftAuctionState,
    state: &tl_enums::StarGiftAuctionState,
) {
    match state {
        tl_enums::StarGiftAuctionState::StarGiftAuctionState(data) => {
            if entry.version >= data.version && entry.version != 0 {
                return;
            }
            entry.version = data.version;
            entry.start_date = data.start_date;
            entry.end_date = data.end_date;
            entry.min_bid_amount = data.min_bid_amount;
            entry.bid_levels = data
                .bid_levels
                .iter()
                .map(
                    |tl_enums::AuctionBidLevel::AuctionBidLevel(lvl)| GiftAuctionBidLevel {
                        amount: lvl.amount,
                        position: lvl.pos,
                        date: lvl.date,
                    },
                )
                .collect();
            entry.top_bidders = data
                .top_bidders
                .iter()
                .map(|&uid| UserId::from_tl(uid))
                .collect();
            entry.next_round_at = data.next_round_at;
            entry.last_gift_num = data.last_gift_num;
            entry.gifts_left = data.gifts_left;
            entry.current_round = data.current_round;
            entry.total_rounds = data.total_rounds;
            entry.round_parameters = data
                .rounds
                .iter()
                .map(|r| match r {
                    tl_enums::StarGiftAuctionRound::StarGiftAuctionRound(rd) => GiftAuctionRound {
                        number: rd.num,
                        duration: rd.duration,
                        extend_top: 0,
                        extend_duration: 0,
                    },
                    tl_enums::StarGiftAuctionRound::Extendable(rd) => GiftAuctionRound {
                        number: rd.num,
                        duration: rd.duration,
                        extend_top: rd.extend_top,
                        extend_duration: rd.extend_window,
                    },
                })
                .collect();
            entry.average_price = 0;
        }
        tl_enums::StarGiftAuctionState::Finished(data) => {
            entry.average_price = data.average_price;
            entry.start_date = data.start_date;
            entry.end_date = data.end_date;
            entry.min_bid_amount = 0;
            entry.next_round_at = 0;
            entry.current_round = 0;
            entry.total_rounds = 0;
            entry.gifts_left = 0;
            entry.version = 0;
            entry.listed_count = data.listed_count.unwrap_or(0);
            entry.fragment_listed_count = data.fragment_listed_count.unwrap_or(0);
            entry.fragment_listed_url = data.fragment_listed_url.clone().unwrap_or_default();
        }
        tl_enums::StarGiftAuctionState::NotModified => {}
    }
}

/// Applies a TL [`tl_enums::StarGiftAuctionUserState`] onto `my` (`GiftAuctions::apply`).
pub fn apply_auction_user_state_tl(
    my: &mut StarGiftAuctionMyState,
    state: &tl_enums::StarGiftAuctionUserState,
) {
    let tl_enums::StarGiftAuctionUserState::StarGiftAuctionUserState(data) = state;
    my.to_peer_id = data
        .bid_peer
        .as_ref()
        .map(peer_id_from_tl)
        .unwrap_or(PeerId::ZERO);
    my.min_bid_amount = data.min_bid_amount.unwrap_or(0);
    my.bid = data.bid_amount.unwrap_or(0);
    my.date = data.bid_date.unwrap_or(0);
    my.got_count = data.acquired_count;
    my.returned = data.returned;
}

/// Converts a TL `payments.StarGiftAuctionState` into [`GiftAuctionState`].
pub fn auction_state_from_tl(tl: &tl_enums::payments::StarGiftAuctionState) -> GiftAuctionState {
    let tl_enums::payments::StarGiftAuctionState::StarGiftAuctionState(data) = tl;
    let mut state = GiftAuctionState {
        gift: Some(star_gift_from_tl(&data.gift)),
        timeout: data.timeout,
        ..GiftAuctionState::default()
    };
    apply_auction_state_tl(&mut state, &data.state);
    apply_auction_user_state_tl(&mut state.my, &data.user_state);
    state
}

/// Converts a TL `payments.StarGiftAuctionAcquiredGifts` into `Vec<GiftAuctionAcquiredGift>`.
pub fn acquired_gifts_from_tl(
    tl: &tl_enums::payments::StarGiftAuctionAcquiredGifts,
) -> Vec<GiftAuctionAcquiredGift> {
    let tl_enums::payments::StarGiftAuctionAcquiredGifts::StarGiftAuctionAcquiredGifts(data) = tl;
    data.gifts
        .iter()
        .map(
            |tl_enums::StarGiftAuctionAcquiredGift::StarGiftAuctionAcquiredGift(g)| {
                let message = g
                    .message
                    .as_ref()
                    .map(|tl_enums::TextWithEntities::TextWithEntities(t)| t.text.clone())
                    .unwrap_or_default();
                GiftAuctionAcquiredGift {
                    to_peer_id: peer_id_from_tl(&g.peer),
                    message,
                    date: g.date,
                    bid_amount: g.bid_amount,
                    round: g.round,
                    number: g.gift_num.unwrap_or(0),
                    position: g.pos,
                    name_hidden: g.name_hidden,
                }
            },
        )
        .collect()
}

/// Converts a TL `payments.StarGiftActiveAuctions` into `Option<ActiveAuctionsSnapshot>`.
///
/// Returns `None` for `payments.starGiftActiveAuctionsNotModified`.
pub fn active_auctions_from_tl(
    tl: &tl_enums::payments::StarGiftActiveAuctions,
) -> Option<ActiveAuctionsSnapshot> {
    match tl {
        tl_enums::payments::StarGiftActiveAuctions::NotModified => None,
        tl_enums::payments::StarGiftActiveAuctions::StarGiftActiveAuctions(data) => {
            let auctions = data
                .auctions
                .iter()
                .map(
                    |tl_enums::StarGiftActiveAuctionState::StarGiftActiveAuctionState(item)| {
                        let mut st = GiftAuctionState {
                            gift: Some(star_gift_from_tl(&item.gift)),
                            ..GiftAuctionState::default()
                        };
                        apply_auction_state_tl(&mut st, &item.state);
                        apply_auction_user_state_tl(&mut st.my, &item.user_state);
                        st
                    },
                )
                .collect();
            Some(ActiveAuctionsSnapshot { auctions })
        }
    }
}

/// Builds `payments.getStarGiftAuctionState` addressed by `gift_id`.
pub fn build_get_star_gift_auction_state_by_id(
    gift_id: u64,
    version: i32,
) -> tl_fn::payments::GetStarGiftAuctionState {
    tl_fn::payments::GetStarGiftAuctionState {
        auction: tl_enums::InputStarGiftAuction::InputStarGiftAuction(
            tl_types::InputStarGiftAuction {
                gift_id: gift_id as i64,
            },
        ),
        version,
    }
}

/// Builds `payments.getStarGiftAuctionState` addressed by auction `slug`.
pub fn build_get_star_gift_auction_state_by_slug(
    slug: impl Into<String>,
    version: i32,
) -> tl_fn::payments::GetStarGiftAuctionState {
    tl_fn::payments::GetStarGiftAuctionState {
        auction: tl_enums::InputStarGiftAuction::Slug(tl_types::InputStarGiftAuctionSlug {
            slug: slug.into(),
        }),
        version,
    }
}

/// Builds `payments.getStarGiftAuctionAcquiredGifts`.
pub fn build_get_star_gift_auction_acquired_gifts(
    gift_id: u64,
) -> tl_fn::payments::GetStarGiftAuctionAcquiredGifts {
    tl_fn::payments::GetStarGiftAuctionAcquiredGifts {
        gift_id: gift_id as i64,
    }
}

/// Builds `payments.getStarGiftActiveAuctions`.
pub fn build_get_star_gift_active_auctions(
    hash: i64,
) -> tl_fn::payments::GetStarGiftActiveAuctions {
    tl_fn::payments::GetStarGiftActiveAuctions { hash }
}

/// Builds `inputInvoiceStarGiftAuctionBid` for placing or raising a bid.
pub fn build_auction_bid_invoice(
    gift_id: u64,
    bid_amount: i64,
    update_bid: bool,
    hide_name: bool,
    peer: Option<tl_enums::InputPeer>,
    message: Option<tl_enums::TextWithEntities>,
) -> tl_enums::InputInvoice {
    tl_enums::InputInvoice::StarGiftAuctionBid(tl_types::InputInvoiceStarGiftAuctionBid {
        hide_name,
        update_bid,
        peer,
        gift_id: gift_id as i64,
        bid_amount,
        message,
    })
}
