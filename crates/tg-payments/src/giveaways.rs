//! Giveaway creation, prepaid giveaway launch, gift code option catalog,
//! giveaway results parsing, and boost slot reassignment models and TL builders.
//!
//! Port of `info/channel_statistics/boosts/create_giveaway_box.cpp`,
//! `boxes/gift_premium_box.cpp`, `boxes/peers/replace_boost_box.cpp`,
//! and `api/api_premium.cpp` (`PremiumGiftCodeOptions`, `applyPrepaid`).

use std::collections::BTreeMap;

use tg_data::{ChannelId, MsgId, PeerId, TimeId, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};

use crate::credits::CreditsGiveawayOption;
use crate::currency::CREDITS_CURRENCY;
use crate::form::{InvoicePremiumGiftCode, PremiumGiftPurpose};
use crate::premium::{
    GIVEAWAY_ADD_PEERS_MAX_FALLBACK, GIVEAWAY_BOOSTS_PER_PREMIUM_FALLBACK,
    GIVEAWAY_COUNTRIES_MAX_FALLBACK, GIVEAWAY_PERIOD_MAX_FALLBACK, MyBoostSlot,
    PremiumSubscriptionOption, filter_non_stars_options, gift_code_options_from_tl,
};

/// Maximum length of a custom additional prize description (`kAdditionalPrizeLengthMax`).
pub const GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN: usize = 128;
/// Default giveaway duration in days (`ThreeDaysAfterToday` in `create_giveaway_box.cpp`).
pub const GIVEAWAY_DEFAULT_DURATION_DAYS: i32 = 3;

/// Server-configured giveaway limits (`PremiumGiftCodeOptions` appConfig helpers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GiveawayLimits {
    /// Boosts granted per Premium prize (default 4).
    pub boosts_per_premium: i32,
    /// Maximum allowed country filters (default 10).
    pub countries_max: i32,
    /// Maximum additional channels required to subscribe (default 10).
    pub add_peers_max: i32,
    /// Maximum duration in seconds (default 7 days).
    pub period_max: i32,
}

impl Default for GiveawayLimits {
    fn default() -> Self {
        Self {
            boosts_per_premium: GIVEAWAY_BOOSTS_PER_PREMIUM_FALLBACK,
            countries_max: GIVEAWAY_COUNTRIES_MAX_FALLBACK,
            add_peers_max: GIVEAWAY_ADD_PEERS_MAX_FALLBACK,
            period_max: GIVEAWAY_PERIOD_MAX_FALLBACK,
        }
    }
}

/// Prize kind configured in a giveaway draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GiveawayPrizeKind {
    /// Telegram Premium subscriptions (`months` per winner).
    Premium {
        /// Subscription length in months (typically 3, 6, or 12).
        months: i32,
    },
    /// Telegram Stars prize pool split across winners.
    Stars {
        /// Total Stars distributed.
        stars: u64,
    },
}

/// Validation errors for [`GiveawayDraft`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GiveawayValidationError {
    /// Winner quantity must be positive.
    InvalidQuantity,
    /// Premium months or Stars pool must be positive.
    InvalidPrize,
    /// Draw timestamp must be in the future.
    DateInPast,
    /// Draw timestamp exceeds `limits.period_max`.
    DateTooFar {
        /// Maximum duration in seconds.
        max_period_secs: i32,
    },
    /// Too many additional channels attached.
    TooManyAdditionalPeers {
        /// Provided count.
        count: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// Too many country ISO2 filters attached.
    TooManyCountries {
        /// Provided count.
        count: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// Additional prize description exceeds [`GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN`].
    PrizeDescriptionTooLong {
        /// Character count.
        len: usize,
        /// Maximum allowed.
        max: usize,
    },
}

/// Editable draft for creating or launching a giveaway (`create_giveaway_box.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveawayDraft {
    /// Primary boosted channel/supergroup peer ID.
    pub boost_peer: PeerId,
    /// Prize configuration (Premium months or Stars pool).
    pub prize: GiveawayPrizeKind,
    /// Number of winners / prizes.
    pub quantity: i32,
    /// Additional channels users must subscribe to.
    pub additional_peers: Vec<PeerId>,
    /// Allowed ISO2 country codes (empty = worldwide).
    pub countries_iso2: Vec<String>,
    /// Optional custom additional prize description.
    pub prize_description: String,
    /// Draw timestamp (Unix seconds).
    pub until_date: TimeId,
    /// Restrict entries to users who subscribed after giveaway start.
    pub only_new_subscribers: bool,
    /// Publicly list winners after draw.
    pub winners_are_visible: bool,
    /// Prepaid giveaway ID when launching an already-paid giveaway via `launchPrepaidGiveaway`.
    pub prepaid_id: Option<u64>,
}

impl GiveawayDraft {
    /// Computes default draw timestamp 3 days after `now`, rounded up to a 5-minute boundary
    /// (`ThreeDaysAfterToday` in `create_giveaway_box.cpp`).
    pub const fn default_until_date(now: TimeId) -> TimeId {
        let target = now.saturating_add(GIVEAWAY_DEFAULT_DURATION_DAYS * 86_400);
        let rem = target % 300;
        if rem == 0 {
            target
        } else {
            target.saturating_add(300 - rem)
        }
    }

    /// Expected boosts granted to the primary channel for this giveaway.
    pub const fn expected_boosts(&self, limits: &GiveawayLimits, stars_yearly_boosts: i32) -> i32 {
        match self.prize {
            GiveawayPrizeKind::Premium { .. } => {
                self.quantity.saturating_mul(limits.boosts_per_premium)
            }
            GiveawayPrizeKind::Stars { .. } => stars_yearly_boosts,
        }
    }

    /// Validates all giveaway parameters against `now` and `limits`.
    pub fn validate(
        &self,
        now: TimeId,
        limits: &GiveawayLimits,
    ) -> Result<(), GiveawayValidationError> {
        if self.quantity <= 0 {
            return Err(GiveawayValidationError::InvalidQuantity);
        }
        match self.prize {
            GiveawayPrizeKind::Premium { months } if months <= 0 => {
                return Err(GiveawayValidationError::InvalidPrize);
            }
            GiveawayPrizeKind::Stars { stars: 0 } => {
                return Err(GiveawayValidationError::InvalidPrize);
            }
            _ => {}
        }
        if self.until_date <= now {
            return Err(GiveawayValidationError::DateInPast);
        }
        if self.until_date - now > limits.period_max {
            return Err(GiveawayValidationError::DateTooFar {
                max_period_secs: limits.period_max,
            });
        }
        if self.additional_peers.len() > limits.add_peers_max.max(0) as usize {
            return Err(GiveawayValidationError::TooManyAdditionalPeers {
                count: self.additional_peers.len(),
                max: limits.add_peers_max.max(0) as usize,
            });
        }
        if self.countries_iso2.len() > limits.countries_max.max(0) as usize {
            return Err(GiveawayValidationError::TooManyCountries {
                count: self.countries_iso2.len(),
                max: limits.countries_max.max(0) as usize,
            });
        }
        let desc_len = self.prize_description.trim().chars().count();
        if desc_len > GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN {
            return Err(GiveawayValidationError::PrizeDescriptionTooLong {
                len: desc_len,
                max: GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN,
            });
        }
        Ok(())
    }

    /// Builds an [`InvoicePremiumGiftCode`] suitable for `payments.getPaymentForm` or
    /// `payments.launchPrepaidGiveaway`.
    #[allow(clippy::too_many_arguments)]
    pub fn to_invoice(
        &self,
        boost_peer: tl_enums::InputPeer,
        additional_peers: Vec<tl_enums::InputPeer>,
        currency: impl Into<String>,
        amount: i64,
        store_product: impl Into<String>,
        store_quantity: i32,
        random_id: i64,
    ) -> InvoicePremiumGiftCode {
        let (giveaway_credits, months) = match self.prize {
            GiveawayPrizeKind::Premium { months } => (None, months),
            GiveawayPrizeKind::Stars { stars } => (Some(stars), 0),
        };
        InvoicePremiumGiftCode {
            purpose: PremiumGiftPurpose::Giveaway {
                boost_peer,
                additional_peers,
                countries_iso2: self.countries_iso2.clone(),
                prize_description: self.prize_description.trim().to_owned(),
                until_date: self.until_date,
                only_new_subscribers: self.only_new_subscribers,
                winners_are_visible: self.winners_are_visible,
            },
            currency: currency.into(),
            store_product: store_product.into(),
            giveaway_credits,
            random_id,
            amount,
            store_quantity,
            users: self.quantity,
            months,
        }
    }
}

/// Store metadata for a single `(users, months)` Premium gift code option.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftCodeStoreEntry {
    /// Price in minor currency units.
    pub amount: i64,
    /// Currency code.
    pub currency: String,
    /// Optional app store product ID.
    pub store_product: String,
    /// Optional app store quantity.
    pub store_quantity: i32,
}

/// Parsed catalog of Premium gift code & giveaway options (`Api::PremiumGiftCodeOptions`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PremiumGiftCodeCatalog {
    /// Formatted subscription options keyed by winner/user count.
    pub options_by_users: BTreeMap<i32, Vec<PremiumSubscriptionOption>>,
    /// Raw 1-user options (`(months, amount, currency)`) for fallback linear scaling.
    pub one_person_options: Vec<(i32, i64, String)>,
    /// Distinct winner count presets advertised by the server (in order).
    pub available_presets: Vec<i32>,
    /// Store metadata keyed by `(users, months)`.
    pub stores: BTreeMap<(i32, i32), GiftCodeStoreEntry>,
}

impl PremiumGiftCodeCatalog {
    /// Parses a `Vector<PremiumGiftCodeOption>` returned by `payments.getPremiumGiftCodeOptions`.
    pub fn from_tl(options: &[tl_enums::PremiumGiftCodeOption]) -> Self {
        let mut catalog = Self::default();
        let mut by_users_tl: BTreeMap<i32, Vec<tl_enums::PremiumGiftCodeOption>> = BTreeMap::new();
        for opt in options {
            let tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(data) = opt;
            by_users_tl.entry(data.users).or_default().push(opt.clone());
            catalog.stores.insert(
                (data.users, data.months),
                GiftCodeStoreEntry {
                    amount: data.amount,
                    currency: data.currency.clone(),
                    store_product: data.store_product.clone().unwrap_or_default(),
                    store_quantity: data.store_quantity.unwrap_or(0),
                },
            );
            if !catalog.available_presets.contains(&data.users) {
                catalog.available_presets.push(data.users);
            }
        }
        for (users, tl_list) in by_users_tl {
            if users == 1 && catalog.one_person_options.is_empty() {
                for tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(o) in &tl_list {
                    if o.currency != CREDITS_CURRENCY {
                        catalog
                            .one_person_options
                            .push((o.months, o.amount, o.currency.clone()));
                    }
                }
            }
            catalog
                .options_by_users
                .insert(users, gift_code_options_from_tl(&tl_list));
        }
        catalog
    }

    /// Returns non-Stars giveaway pricing options for `users_count` winners,
    /// extrapolating linearly from 1-user options if the exact preset wasn't returned
    /// (`PremiumGiftCodeOptions::optionsForGiveaway` in `api_premium.cpp:680-707`).
    pub fn options_for_giveaway(&self, users_count: i32) -> Vec<PremiumSubscriptionOption> {
        if let Some(opts) = self.options_by_users.get(&users_count) {
            return filter_non_stars_options(opts.clone());
        }
        let synth: Vec<tl_enums::PremiumGiftCodeOption> = self
            .one_person_options
            .iter()
            .map(|(months, cost, currency)| {
                tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(
                    tl_types::PremiumGiftCodeOption {
                        users: users_count,
                        months: *months,
                        store_product: None,
                        store_quantity: None,
                        currency: currency.clone(),
                        amount: cost.saturating_mul(i64::from(users_count.max(1))),
                    },
                )
            })
            .collect();
        filter_non_stars_options(gift_code_options_from_tl(&synth))
    }

    /// Looks up store metadata for `(users, months)`, falling back to linear scaling.
    pub fn store_for(&self, users: i32, months: i32) -> Option<GiftCodeStoreEntry> {
        if let Some(found) = self.stores.get(&(users, months)) {
            return Some(found.clone());
        }
        self.one_person_options
            .iter()
            .find(|(m, _, _)| *m == months)
            .map(|(_, cost, currency)| GiftCodeStoreEntry {
                amount: cost.saturating_mul(i64::from(users.max(1))),
                currency: currency.clone(),
                store_product: String::new(),
                store_quantity: 0,
            })
    }

    /// Builds an invoice for a Stars giveaway option.
    pub fn stars_giveaway_invoice(
        draft: &GiveawayDraft,
        option: &CreditsGiveawayOption,
        boost_peer: tl_enums::InputPeer,
        additional_peers: Vec<tl_enums::InputPeer>,
        random_id: i64,
    ) -> InvoicePremiumGiftCode {
        draft.to_invoice(
            boost_peer,
            additional_peers,
            option.currency.clone(),
            option.amount as i64,
            option.store_product.clone(),
            0,
            random_id,
        )
    }
}

/// Parsed summary of a giveaway or giveaway results message (`messageMediaGiveaway*`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiveawayResultsSummary {
    /// Hosting channel ID.
    pub channel_id: ChannelId,
    /// Launch giveaway message ID (`MsgId::ZERO` for launch announcements).
    pub launch_msg_id: MsgId,
    /// Total winners count.
    pub winners_count: i32,
    /// Unclaimed prize count.
    pub unclaimed_count: i32,
    /// Winner user IDs (when `winners_are_visible` or in results).
    pub winners: Vec<UserId>,
    /// Additional required channels count.
    pub additional_peers_count: i32,
    /// Premium months per winner (`0` for Stars giveaway).
    pub months: i32,
    /// Stars prize pool (`0` for Premium giveaway).
    pub stars: u64,
    /// Optional custom additional prize description.
    pub prize_description: String,
    /// Draw/finish timestamp.
    pub until_date: TimeId,
    /// Only new subscribers eligible.
    pub only_new_subscribers: bool,
    /// Giveaway was refunded.
    pub refunded: bool,
}

/// Converts a TL `MessageMedia::GiveawayResults` into [`GiveawayResultsSummary`].
pub fn giveaway_results_from_tl(
    tl: &tl_types::MessageMediaGiveawayResults,
) -> GiveawayResultsSummary {
    GiveawayResultsSummary {
        channel_id: ChannelId::from_tl(tl.channel_id),
        launch_msg_id: MsgId::from(tl.launch_msg_id),
        winners_count: tl.winners_count,
        unclaimed_count: tl.unclaimed_count,
        winners: tl.winners.iter().map(|&id| UserId::from_tl(id)).collect(),
        additional_peers_count: tl.additional_peers_count.unwrap_or(0),
        months: tl.months.unwrap_or(0),
        stars: tl.stars.unwrap_or(0).max(0) as u64,
        prize_description: tl.prize_description.clone().unwrap_or_default(),
        until_date: tl.until_date,
        only_new_subscribers: tl.only_new_subscribers,
        refunded: tl.refunded,
    }
}

// ============================================================================
// Boost Replacement / Reassignment (`boxes/peers/replace_boost_box.cpp`)
// ============================================================================

/// A boost slot currently assigned to another channel (`TakenBoostSlot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TakenBoostSlot {
    /// Slot ID.
    pub id: i32,
    /// Expiration timestamp.
    pub expires: TimeId,
    /// Peer ID currently boosted by this slot (`PeerId::ZERO` if free but on cooldown).
    pub peer_id: PeerId,
    /// Cooldown expiration timestamp (`<= now` when ready to reassign).
    pub cooldown: TimeId,
}

impl TakenBoostSlot {
    /// Returns `true` if this slot is still waiting for its reassignment cooldown.
    pub const fn is_waiting(&self, now: TimeId) -> bool {
        self.cooldown > now
    }

    /// Remaining cooldown seconds at timestamp `now` (`0` if ready).
    pub const fn remaining_cooldown_secs(&self, now: TimeId) -> i32 {
        if self.cooldown > now {
            self.cooldown - now
        } else {
            0
        }
    }
}

/// Partition of the user's boost slots relative to a target channel (`ForChannelBoostSlots`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForChannelBoostSlots {
    /// Unassigned slots with no active cooldown.
    pub free: Vec<i32>,
    /// Slots already boosting the target channel.
    pub already: Vec<i32>,
    /// Slots boosting another peer (or unassigned but on cooldown).
    pub other: Vec<TakenBoostSlot>,
}

/// Partitions `my_slots` relative to `target_channel` matching `ParseForChannelBoostSlots`
/// in `boxes/peers/replace_boost_box.cpp:392-418`.
pub fn parse_for_channel_boost_slots(
    target_channel: PeerId,
    my_slots: &[MyBoostSlot],
    now: TimeId,
) -> ForChannelBoostSlots {
    let mut result = ForChannelBoostSlots::default();
    for slot in my_slots {
        let peer_id = slot.peer_id.unwrap_or(PeerId::ZERO);
        if peer_id == PeerId::ZERO && slot.cooldown_until_date <= now {
            result.free.push(slot.slot);
        } else if peer_id == target_channel {
            result.already.push(slot.slot);
        } else {
            result.other.push(TakenBoostSlot {
                id: slot.slot,
                expires: slot.expires,
                peer_id,
                cooldown: slot.cooldown_until_date,
            });
        }
    }
    sort_taken_boost_slots(&mut result.other, now);
    result
}

/// Sorts taken boost slots so immediately reassignable slots appear first and
/// cooldown-locked slots appear after in ascending wait-time order
/// (`replace_boost_box.cpp:234-236`).
pub fn sort_taken_boost_slots(slots: &mut [TakenBoostSlot], now: TimeId) {
    slots.sort_by_key(|slot| {
        if slot.cooldown > now {
            i64::from(slot.cooldown)
        } else {
            -i64::from(slot.cooldown)
        }
    });
}

/// Formats boost slot cooldown seconds as `H:MM:SS` or `M:SS` (`Row::updateStatus`).
pub fn format_boost_cooldown(seconds: i32) -> String {
    let s = seconds.max(0);
    let hours = s / 3600;
    let minutes = s / 60;
    if hours > 0 {
        format!("{hours}:{:02}:{:02}", minutes % 60, s % 60)
    } else {
        format!("{minutes}:{:02}", s % 60)
    }
}

// ============================================================================
// TL Request Builders
// ============================================================================

/// Builds `payments.getPremiumGiftCodeOptions`.
pub fn build_get_premium_gift_code_options(
    boost_peer: Option<tl_enums::InputPeer>,
) -> tl_fn::payments::GetPremiumGiftCodeOptions {
    tl_fn::payments::GetPremiumGiftCodeOptions { boost_peer }
}

/// Builds `payments.getStarsGiveawayOptions`.
pub fn build_get_stars_giveaway_options() -> tl_fn::payments::GetStarsGiveawayOptions {
    tl_fn::payments::GetStarsGiveawayOptions {}
}

/// Builds `payments.launchPrepaidGiveaway` from an [`InvoicePremiumGiftCode`].
pub fn build_launch_prepaid_giveaway(
    peer: tl_enums::InputPeer,
    giveaway_id: u64,
    invoice: &InvoicePremiumGiftCode,
) -> Option<tl_fn::payments::LaunchPrepaidGiveaway> {
    let purpose = if invoice.giveaway_credits.is_some() {
        invoice.credits_giveaway_purpose_to_tl()?
    } else {
        invoice.giveaway_purpose_to_tl()?
    };
    Some(tl_fn::payments::LaunchPrepaidGiveaway {
        peer,
        giveaway_id: giveaway_id as i64,
        purpose,
    })
}

/// Builds `premium.applyBoost` to apply or reassign `slots` to `peer`.
pub fn build_apply_boost(peer: tl_enums::InputPeer, slots: Vec<i32>) -> tl_fn::premium::ApplyBoost {
    tl_fn::premium::ApplyBoost {
        slots: (!slots.is_empty()).then_some(slots),
        peer,
    }
}
