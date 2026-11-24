//! `tg-payments`: Pure-logic Telegram payments, Telegram Stars, Star Gifts, Premium,
//! boosts, and giveaways models and state machines.
//!
//! Rust port of Telegram Desktop's payment and monetization logic:
//! - [`currency`]: Currency rules, amount formatting, and money field validation
//!   (`ui/text/format_values.cpp`, `payments/ui/payments_field.cpp`).
//! - [`card`]: Bank card brand/BIN detection, Luhn checksum, expiry/CVC/billing validation,
//!   and card field formatting (`payments/stripe/stripe_card_validator.cpp`,
//!   `payments/ui/payments_edit_card.cpp`).
//! - [`providers`]: Stripe and SmartGlocal tokenization HTTP request builders and response
//!   parsers behind an abstract [`providers::HttpClient`] trait (`payments/stripe/*`,
//!   `payments/smartglocal/*`).
//! - [`form`]: Payment form model for invoices, saved info, shipping, tips, saved credentials,
//!   and `payments.validateRequestedInfo`/`payments.sendPaymentForm` TL builders
//!   (`payments/payments_form.cpp`).
//! - [`checkout`]: Checkout and 3-D Secure/verification URL state machine
//!   (`payments/payments_checkout_process.cpp`).
//! - [`credits`]: Telegram Stars balance tracking, transaction history pagination, top-up
//!   options, paid media/reactions/messages costs, and subscriptions (`core/credits_amount.h`,
//!   `data/data_credits*.h`, `data/components/credits.cpp`, `api/api_credits*.cpp`).
//! - [`star_gifts`]: Star Gifts & unique collectible gifts, attributes, upgrade/transfer/resale
//!   states, collections, peer gifts filtering, and TL conversions (`data/data_star_gift.cpp`,
//!   `api/api_premium.cpp`, `info/peer_gifts/*`).
//! - [`star_gift_auctions`]: Star Gift auctions state, bid levels, rounds, active auctions hash,
//!   and Layer 233 auction TL RPCs (`data/components/gift_auctions.cpp`,
//!   `boxes/star_gift_auction_box.cpp`).
//! - [`star_gift_craft`]: Star Gift crafting forge validation, probability math, and crafting TL
//!   RPCs (`boxes/star_gift_craft_box.cpp`).
//! - [`premium`]: Premium subscription options, promo data, Premium/boost limits, gift codes,
//!   giveaways, and boosts (`api/api_premium*.cpp`, `data/data_premium_limits.cpp`,
//!   `data/data_boosts.h`, `api/api_statistics.cpp`).
//! - [`giveaways`]: Giveaway creation draft/validation, gift code catalog, prepaid launch,
//!   giveaway results, and boost slot reassignment (`create_giveaway_box.cpp`,
//!   `replace_boost_box.cpp`, `api/api_premium.cpp`).
//! - [`starref`]: Telegram Stars Affiliate Program models, validation, formatting, and TL RPCs
//!   (`info/bot/starref/*`).

pub mod card;
pub mod checkout;
pub mod credits;
pub mod currency;
pub mod form;
pub mod giveaways;
pub mod premium;
pub mod providers;
pub mod star_gift_auctions;
pub mod star_gift_craft;
pub mod star_gifts;
pub mod starref;
