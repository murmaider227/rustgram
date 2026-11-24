//! Payment form model for invoices, shipping, tips, saved credentials, and TL request builders.
//!
//! Port of `payments/payments_form.h`, `payments/payments_form.cpp`, and
//! `payments/ui/payments_panel_data.h`.

use serde_json::Value as JsonValue;
use tg_data::{MsgId, PeerId, TimeId, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};

use crate::card::{NativeMethodDetails, UncheckedCardDetails, YearMonth, validate_card_details};
use crate::credits::CreditsAmount;
use crate::currency::CREDITS_CURRENCY;

/// Temporary cloud password validity period requested for saved cards (`kPasswordPeriod`, 15 min).
pub const TMP_PASSWORD_PERIOD_SECS: i32 = 15 * 60;

/// One line item in an invoice or shipping option (`Payments::Ui::LabeledPrice`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LabeledPrice {
    /// Human-readable item label.
    pub label: String,
    /// Price in minor currency units (can be negative for discounts).
    pub price: i64,
}

impl LabeledPrice {
    /// Converts from a TL [`tl_enums::LabeledPrice`].
    pub fn from_tl(tl: &tl_enums::LabeledPrice) -> Self {
        let tl_enums::LabeledPrice::LabeledPrice(p) = tl;
        Self {
            label: p.label.clone(),
            price: p.amount,
        }
    }

    /// Converts to a TL [`tl_enums::LabeledPrice`].
    pub fn to_tl(&self) -> tl_enums::LabeledPrice {
        tl_enums::LabeledPrice::LabeledPrice(tl_types::LabeledPrice {
            label: self.label.clone(),
            amount: self.price,
        })
    }
}

/// Product cover metadata (`Payments::Ui::Cover`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cover {
    /// Product title.
    pub title: String,
    /// Product description text.
    pub description: String,
    /// Seller/bot display name.
    pub seller: String,
}

/// Paid invoice receipt summary (`Payments::Ui::Receipt`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipt {
    /// Payment Unix timestamp.
    pub date: TimeId,
    /// Total paid amount in minor currency units.
    pub total_amount: i64,
    /// Currency code.
    pub currency: String,
    /// Whether this form represents an already-paid receipt.
    pub paid: bool,
}

/// Parsed invoice fields (`Payments::Ui::Invoice`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Invoice {
    /// Product cover info.
    pub cover: Cover,
    /// Base invoice line items.
    pub prices: Vec<LabeledPrice>,
    /// Suggested tip amounts in minor currency units.
    pub suggested_tips: Vec<i64>,
    /// Maximum allowed tip amount (`0` if tips are disabled).
    pub tips_max: i64,
    /// Currently selected tip amount.
    pub tips_selected: i64,
    /// ISO-4217 currency code (or `"XTR"` for Telegram Stars).
    pub currency: String,
    /// Receipt summary if viewing a paid receipt.
    pub receipt: Receipt,
    /// Whether customer's name is requested.
    pub is_name_requested: bool,
    /// Whether customer's phone number is requested.
    pub is_phone_requested: bool,
    /// Whether customer's email address is requested.
    pub is_email_requested: bool,
    /// Whether shipping address is requested.
    pub is_shipping_address_requested: bool,
    /// Whether this invoice sets up recurring payments.
    pub is_recurring: bool,
    /// Whether final price depends on shipping option.
    pub is_flexible: bool,
    /// Whether this is a test invoice.
    pub is_test: bool,
    /// Payment provider display name.
    pub provider: String,
    /// Optional terms-of-service URL requiring explicit user acceptance.
    pub terms_url: String,
    /// Whether phone number is forwarded to the payment provider.
    pub phone_sent_to_provider: bool,
    /// Whether email is forwarded to the payment provider.
    pub email_sent_to_provider: bool,
    /// Subscription period in seconds (for Stars subscriptions), if any.
    pub subscription_period: Option<i32>,
}

impl Invoice {
    /// Returns `true` if the invoice has a currency and at least one price or tip ceiling.
    pub fn is_valid(&self) -> bool {
        !self.currency.is_empty() && (!self.prices.is_empty() || self.tips_max > 0)
    }

    /// Computes total amount across base `prices`, optional selected shipping option, and `tips_selected`.
    pub fn total_amount(&self, selected_shipping: Option<&ShippingOption>) -> i64 {
        let base: i64 = self.prices.iter().map(|p| p.price).sum();
        let shipping: i64 = selected_shipping
            .map(|opt| opt.prices.iter().map(|p| p.price).sum())
            .unwrap_or(0);
        base.saturating_add(shipping)
            .saturating_add(self.tips_selected)
    }

    /// Returns `true` if any customer information field must be validated before checkout.
    pub fn requires_information(&self) -> bool {
        self.is_shipping_address_requested
            || self.is_name_requested
            || self.is_email_requested
            || self.is_phone_requested
    }
}

/// One shipping option returned by `payments.validateRequestedInfo` (`Payments::Ui::ShippingOption`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShippingOption {
    /// Provider-defined shipping option ID.
    pub id: String,
    /// Option display title.
    pub title: String,
    /// Shipping price breakdown.
    pub prices: Vec<LabeledPrice>,
}

impl ShippingOption {
    /// Converts from a TL [`tl_enums::ShippingOption`].
    pub fn from_tl(tl: &tl_enums::ShippingOption) -> Self {
        let tl_enums::ShippingOption::ShippingOption(s) = tl;
        Self {
            id: s.id.clone(),
            title: s.title.clone(),
            prices: s.prices.iter().map(LabeledPrice::from_tl).collect(),
        }
    }
}

/// Available shipping options and active selection (`Payments::Ui::ShippingOptions`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShippingOptions {
    /// Invoice currency code.
    pub currency: String,
    /// Available shipping options.
    pub list: Vec<ShippingOption>,
    /// Currently selected option ID (empty if none selected).
    pub selected_id: String,
}

impl ShippingOptions {
    /// Returns the currently selected [`ShippingOption`], if any.
    pub fn selected(&self) -> Option<&ShippingOption> {
        self.list.iter().find(|o| o.id == self.selected_id)
    }
}

/// Shipping address (`Payments::Ui::Address`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PostAddress {
    /// Street address line 1.
    pub address1: String,
    /// Street address line 2.
    pub address2: String,
    /// City.
    pub city: String,
    /// State / region.
    pub state: String,
    /// Two-letter ISO-3166-1 alpha-2 country code.
    pub country_iso2: String,
    /// Postal / ZIP code.
    pub postcode: String,
}

impl PostAddress {
    /// Returns `true` if required shipping address fields (`address1`, `city`, `country_iso2`)
    /// are non-empty (`Address::valid`).
    pub fn is_valid(&self) -> bool {
        !self.address1.is_empty() && !self.city.is_empty() && !self.country_iso2.is_empty()
    }

    /// Converts from a TL [`tl_enums::PostAddress`].
    pub fn from_tl(tl: &tl_enums::PostAddress) -> Self {
        let tl_enums::PostAddress::PostAddress(a) = tl;
        Self {
            address1: a.street_line1.clone(),
            address2: a.street_line2.clone(),
            city: a.city.clone(),
            state: a.state.clone(),
            country_iso2: a.country_iso2.clone(),
            postcode: a.post_code.clone(),
        }
    }

    /// Converts to a TL [`tl_enums::PostAddress`].
    pub fn to_tl(&self) -> tl_enums::PostAddress {
        tl_enums::PostAddress::PostAddress(tl_types::PostAddress {
            street_line1: self.address1.clone(),
            street_line2: self.address2.clone(),
            city: self.city.clone(),
            state: self.state.clone(),
            country_iso2: self.country_iso2.clone(),
            post_code: self.postcode.clone(),
        })
    }
}

/// Customer information requested by an invoice (`Payments::Ui::RequestedInformation`).
#[derive(Debug, Clone, Eq)]
pub struct RequestedInformation {
    /// User's default phone number (hint, not part of equality comparison).
    pub default_phone: String,
    /// User's default country ISO2 (hint, not part of equality comparison).
    pub default_country: String,
    /// Whether to save this info on Telegram servers (`payments.validateRequestedInfo.save`).
    pub save: bool,
    /// Customer full name.
    pub name: String,
    /// Customer phone number.
    pub phone: String,
    /// Customer email address.
    pub email: String,
    /// Shipping address.
    pub shipping_address: PostAddress,
}

impl Default for RequestedInformation {
    fn default() -> Self {
        Self {
            default_phone: String::new(),
            default_country: String::new(),
            save: true,
            name: String::new(),
            phone: String::new(),
            email: String::new(),
            shipping_address: PostAddress::default(),
        }
    }
}

impl PartialEq for RequestedInformation {
    /// Matches C++ `RequestedInformation::operator==` (compares user-entered fields only).
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.phone == other.phone
            && self.email == other.email
            && self.shipping_address == other.shipping_address
    }
}

impl RequestedInformation {
    /// Returns `true` if all user-entered info fields are empty.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
            && self.phone.is_empty()
            && self.email.is_empty()
            && !self.shipping_address.is_valid()
    }

    /// Parses from a TL [`tl_enums::PaymentRequestedInfo`].
    pub fn from_tl(
        tl: &tl_enums::PaymentRequestedInfo,
        default_phone: String,
        default_country: String,
    ) -> Self {
        let tl_enums::PaymentRequestedInfo::PaymentRequestedInfo(info) = tl;
        Self {
            default_phone,
            default_country,
            save: true,
            name: info.name.clone().unwrap_or_default(),
            phone: info.phone.clone().unwrap_or_default(),
            email: info.email.clone().unwrap_or_default(),
            shipping_address: info
                .shipping_address
                .as_ref()
                .map(PostAddress::from_tl)
                .unwrap_or_default(),
        }
    }

    /// Serializes to a TL [`tl_enums::PaymentRequestedInfo`] (`Serialize` in `payments_form.cpp`).
    pub fn to_tl(&self) -> tl_enums::PaymentRequestedInfo {
        tl_enums::PaymentRequestedInfo::PaymentRequestedInfo(tl_types::PaymentRequestedInfo {
            name: (!self.name.is_empty()).then(|| self.name.clone()),
            phone: (!self.phone.is_empty()).then(|| self.phone.clone()),
            email: (!self.email.is_empty()).then(|| self.email.clone()),
            shipping_address: self
                .shipping_address
                .is_valid()
                .then(|| self.shipping_address.to_tl()),
        })
    }
}

/// Customer information form fields (`Payments::Ui::InformationField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InformationField {
    /// Street address line 1.
    ShippingStreet,
    /// City.
    ShippingCity,
    /// State / region.
    ShippingState,
    /// Country ISO2.
    ShippingCountry,
    /// Postal / ZIP code.
    ShippingPostcode,
    /// Customer full name.
    Name,
    /// Customer email.
    Email,
    /// Customer phone.
    Phone,
}

/// Server-level metadata of a loaded payment form (`Payments::FormDetails`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormDetails {
    /// Server-assigned form ID (`form_id`).
    pub form_id: i64,
    /// Hosted web payment URL (`url`).
    pub url: String,
    /// Native provider name (`"stripe"`, `"smartglocal"`, or empty).
    pub native_provider: String,
    /// Seller bot username (for terms-of-service confirmation prompt).
    pub terms_bot_username: String,
    /// Raw JSON string from `native_params`.
    pub native_params_json: String,
    /// Merchant bot user ID.
    pub bot_id: UserId,
    /// Payment provider bot user ID.
    pub provider_id: UserId,
    /// Whether credentials can be saved on Telegram servers.
    pub can_save_credentials: bool,
    /// Whether 2FA password must be created before credentials can be saved.
    pub password_missing: bool,
    /// Whether the user has accepted `invoice.terms_url`.
    pub terms_accepted: bool,
}

impl FormDetails {
    /// Returns `true` when `url` is populated.
    pub fn is_valid(&self) -> bool {
        !self.url.is_empty()
    }
}

/// Saved payment credentials card (`Payments::SavedCredentials`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedCredentials {
    /// Server credential ID.
    pub id: String,
    /// Display title (e.g., `"visa *4242"`).
    pub title: String,
}

/// Newly tokenized payment credentials (`Payments::NewCredentials`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewCredentials {
    /// Display title (e.g., `"visa *4242"`).
    pub title: String,
    /// Compact JSON payload sent in `inputPaymentCredentials.data`.
    pub data: String,
    /// Whether the user opted to save credentials on Telegram servers.
    pub save_on_server: bool,
}

impl NewCredentials {
    /// Returns `true` if no credential payload has been set.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Native payment provider configuration parsed from `native_params`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NativePaymentMethod {
    /// No native provider (use hosted webview URL).
    #[default]
    None,
    /// Native Stripe tokenization.
    Stripe {
        /// Stripe publishable key (`publishable_key`).
        publishable_key: String,
    },
    /// Native SmartGlocal tokenization.
    SmartGlocal {
        /// SmartGlocal public token (`public_token`).
        public_token: String,
        /// Optional custom tokenization endpoint (`tokenize_url`).
        tokenize_url: String,
    },
}

/// Additional third-party payment method URL (`Payments::Ui::PaymentMethodAdditional`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaymentMethodAdditional {
    /// Method display title.
    pub title: String,
    /// Hosted checkout URL.
    pub url: String,
}

/// Aggregated payment method state (`Payments::PaymentMethod`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaymentMethod {
    /// Native tokenization method, if supported.
    pub native: NativePaymentMethod,
    /// Native card entry field requirements.
    pub native_details: NativeMethodDetails,
    /// Server-saved payment methods.
    pub saved_credentials: Vec<SavedCredentials>,
    /// Index into `saved_credentials`; equals `saved_credentials.len()` when `new_credentials` is active.
    pub saved_credentials_index: usize,
    /// Newly entered/tokenized credentials.
    pub new_credentials: NewCredentials,
    /// Additional external payment methods.
    pub additional_methods: Vec<PaymentMethodAdditional>,
    /// Hosted webview payment form URL.
    pub url: String,
}

impl PaymentMethod {
    /// Returns `true` if a valid payment method (new or saved) is currently selected.
    pub fn has_selected_method(&self) -> bool {
        !self.new_credentials.is_empty()
            || self.saved_credentials_index < self.saved_credentials.len()
    }

    /// Returns the currently selected saved credentials if a saved card is active.
    pub fn selected_saved(&self) -> Option<&SavedCredentials> {
        self.saved_credentials.get(self.saved_credentials_index)
    }
}

/// Payment form error category (`Payments::Error::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormErrorKind {
    /// Error fetching form/receipt (`payments.getPaymentForm` / `getPaymentReceipt`).
    Form,
    /// Local or server validation error (`payments.validateRequestedInfo` or local card/info checks).
    Validate,
    /// Stripe tokenization API error.
    Stripe,
    /// SmartGlocal tokenization API error.
    SmartGlocal,
    /// Error obtaining temporary 2FA password (`account.getTmpPassword`).
    TmpPassword,
    /// Error sending payment form (`payments.sendPaymentForm`).
    Send,
}

/// Payment form error (`Payments::Error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormError {
    /// Error category.
    pub kind: FormErrorKind,
    /// Error identifier (Telegram RPC error string, `LOCAL_CARD_*`, or provider code).
    pub id: String,
}

impl FormError {
    /// Constructs a new [`FormError`].
    pub fn new(kind: FormErrorKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

/// Stars invoice payment form data (`Payments::CreditsFormData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsFormData {
    /// Server form ID.
    pub form_id: i64,
    /// Merchant bot user ID (`0` for star gift forms).
    pub bot_id: UserId,
    /// Product title.
    pub title: String,
    /// Product description.
    pub description: String,
    /// Price in Stars.
    pub credits: u64,
    /// Currency code (`"XTR"`).
    pub currency: String,
    /// Optional subscription period in seconds.
    pub subscription_period: i32,
    /// Whether this form is for purchasing a Star Gift (`payments.paymentFormStarGift`).
    pub star_gift_form: bool,
}

/// Paid Stars receipt data (`Payments::CreditsReceiptData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsReceiptData {
    /// Transaction ID string.
    pub id: String,
    /// Product title.
    pub title: String,
    /// Product description.
    pub description: String,
    /// Merchant bot peer ID.
    pub peer_id: PeerId,
    /// Total Stars paid.
    pub credits: CreditsAmount,
    /// Payment Unix timestamp.
    pub date: TimeId,
}

/// Outcome of applying a TL `payments.PaymentForm` or `payments.PaymentReceipt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormLoadEvent {
    /// Regular fiat payment form or receipt is ready.
    FormReady,
    /// Stars checkout form received; handle via Stars payment flow.
    CreditsPaymentStarted(CreditsFormData),
    /// Stars receipt received.
    CreditsReceiptReady(CreditsReceiptData),
    /// Invalid form payload.
    Error(FormError),
}

/// Outcome of `PaymentForm::build_send_payment_form_request`.
#[derive(Debug, Clone, PartialEq)]
pub enum SendPaymentAction {
    /// Saved card requires a valid 2FA temporary password (`account.getTmpPassword`) first.
    TmpPasswordRequired,
    /// User has not yet trusted this merchant bot for payments.
    BotTrustRequired {
        /// Merchant bot ID.
        bot_id: UserId,
        /// Payment provider ID.
        provider_id: UserId,
    },
    /// Ready-to-send TL RPC call `payments.sendPaymentForm`.
    Send(Box<tl_fn::payments::SendPaymentForm>),
}

/// Outcome of applying `payments.PaymentResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum PaymentResultOutcome {
    /// Payment succeeded immediately (`payments.paymentResult`).
    Finished(Box<tl_enums::Updates>),
    /// 3-D Secure / external browser verification required (`payments.paymentVerificationNeeded`).
    VerificationNeeded {
        /// Verification URL to load in webview or browser.
        url: String,
    },
}

/// Premium gift code / giveaway invoice descriptor (`Payments::InvoicePremiumGiftCode`).
#[derive(Debug, Clone, PartialEq)]
pub enum PremiumGiftPurpose {
    /// Direct premium gift to one or more users.
    Users {
        /// Target users.
        users: Vec<tl_enums::InputUser>,
        /// Optional channel that receives boosts.
        boost_peer: Option<tl_enums::InputPeer>,
        /// Optional gift message.
        message: Option<tl_enums::TextWithEntities>,
    },
    /// Channel giveaway.
    Giveaway {
        /// Primary boosted channel.
        boost_peer: tl_enums::InputPeer,
        /// Additional channels required for participation.
        additional_peers: Vec<tl_enums::InputPeer>,
        /// Allowed ISO2 country codes (empty = worldwide).
        countries_iso2: Vec<String>,
        /// Optional custom prize description.
        prize_description: String,
        /// Giveaway draw timestamp.
        until_date: TimeId,
        /// Restrict to new subscribers only.
        only_new_subscribers: bool,
        /// Publicly show winners list.
        winners_are_visible: bool,
    },
}

/// Parameters for a Premium gift code or giveaway invoice (`Payments::InvoicePremiumGiftCode`).
#[derive(Debug, Clone, PartialEq)]
pub struct InvoicePremiumGiftCode {
    /// Users or giveaway purpose.
    pub purpose: PremiumGiftPurpose,
    /// Currency code (or `"XTR"` for Stars).
    pub currency: String,
    /// Optional store product identifier.
    pub store_product: String,
    /// When `Some(stars)`, creates a Stars giveaway instead of Premium giveaway.
    pub giveaway_credits: Option<u64>,
    /// Client random ID.
    pub random_id: i64,
    /// Price amount in minor units.
    pub amount: i64,
    /// Optional store quantity.
    pub store_quantity: i32,
    /// Number of recipient/winner users.
    pub users: i32,
    /// Subscription duration in months.
    pub months: i32,
}

impl InvoicePremiumGiftCode {
    /// Returns `true` if this is a Premium-for-Stars gift invoice (`IsPremiumForStarsInvoice`).
    pub fn is_premium_for_stars(&self) -> bool {
        self.giveaway_credits.is_none() && self.currency == CREDITS_CURRENCY
    }

    /// Builds `inputStorePaymentPremiumGiveaway` (`InvoicePremiumGiftCodeGiveawayToTL`).
    pub fn giveaway_purpose_to_tl(&self) -> Option<tl_enums::InputStorePaymentPurpose> {
        let PremiumGiftPurpose::Giveaway {
            ref boost_peer,
            ref additional_peers,
            ref countries_iso2,
            ref prize_description,
            until_date,
            only_new_subscribers,
            winners_are_visible,
        } = self.purpose
        else {
            return None;
        };
        Some(
            tl_enums::InputStorePaymentPurpose::InputStorePaymentPremiumGiveaway(
                tl_types::InputStorePaymentPremiumGiveaway {
                    only_new_subscribers,
                    winners_are_visible,
                    boost_peer: boost_peer.clone(),
                    additional_peers: (!additional_peers.is_empty())
                        .then(|| additional_peers.clone()),
                    countries_iso2: (!countries_iso2.is_empty()).then(|| countries_iso2.clone()),
                    prize_description: (!prize_description.is_empty())
                        .then(|| prize_description.clone()),
                    random_id: self.random_id,
                    until_date,
                    currency: self.currency.clone(),
                    amount: self.amount,
                },
            ),
        )
    }

    /// Builds `inputStorePaymentStarsGiveaway` (`InvoiceCreditsGiveawayToTL`).
    pub fn credits_giveaway_purpose_to_tl(&self) -> Option<tl_enums::InputStorePaymentPurpose> {
        let stars = self.giveaway_credits?;
        let PremiumGiftPurpose::Giveaway {
            ref boost_peer,
            ref additional_peers,
            ref countries_iso2,
            ref prize_description,
            until_date,
            only_new_subscribers,
            winners_are_visible,
        } = self.purpose
        else {
            return None;
        };
        Some(
            tl_enums::InputStorePaymentPurpose::InputStorePaymentStarsGiveaway(
                tl_types::InputStorePaymentStarsGiveaway {
                    only_new_subscribers,
                    winners_are_visible,
                    stars: stars as i64,
                    boost_peer: boost_peer.clone(),
                    additional_peers: (!additional_peers.is_empty())
                        .then(|| additional_peers.clone()),
                    countries_iso2: (!countries_iso2.is_empty()).then(|| countries_iso2.clone()),
                    prize_description: (!prize_description.is_empty())
                        .then(|| prize_description.clone()),
                    random_id: self.random_id,
                    until_date,
                    currency: self.currency.clone(),
                    amount: self.amount,
                    users: self.users,
                },
            ),
        )
    }

    /// Converts this descriptor into a TL [`tl_enums::InputInvoice`].
    pub fn to_input_invoice(&self) -> Option<tl_enums::InputInvoice> {
        if self.giveaway_credits.is_some() {
            return Some(tl_enums::InputInvoice::Stars(tl_types::InputInvoiceStars {
                purpose: self.credits_giveaway_purpose_to_tl()?,
            }));
        }
        let option = tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(
            tl_types::PremiumGiftCodeOption {
                users: self.users,
                months: self.months,
                store_product: (!self.store_product.is_empty()).then(|| self.store_product.clone()),
                store_quantity: (self.store_quantity != 0).then_some(self.store_quantity),
                currency: self.currency.clone(),
                amount: self.amount,
            },
        );
        match &self.purpose {
            PremiumGiftPurpose::Users {
                users,
                boost_peer,
                message,
            } => {
                if users.len() == 1 && self.currency == CREDITS_CURRENCY {
                    Some(tl_enums::InputInvoice::PremiumGiftStars(
                        tl_types::InputInvoicePremiumGiftStars {
                            user_id: users[0].clone(),
                            months: self.months,
                            message: message.clone(),
                        },
                    ))
                } else {
                    Some(tl_enums::InputInvoice::PremiumGiftCode(
                        tl_types::InputInvoicePremiumGiftCode {
                            purpose:
                                tl_enums::InputStorePaymentPurpose::InputStorePaymentPremiumGiftCode(
                                    tl_types::InputStorePaymentPremiumGiftCode {
                                        users: users.clone(),
                                        boost_peer: boost_peer.clone(),
                                        currency: self.currency.clone(),
                                        amount: self.amount,
                                        message: message.clone(),
                                    },
                                ),
                            option,
                        },
                    ))
                }
            }
            PremiumGiftPurpose::Giveaway { .. } => Some(tl_enums::InputInvoice::PremiumGiftCode(
                tl_types::InputInvoicePremiumGiftCode {
                    purpose: self.giveaway_purpose_to_tl()?,
                    option,
                },
            )),
        }
    }
}

/// Invoice target identifier (`Payments::InvoiceId`).
#[derive(Debug, Clone, PartialEq)]
pub enum InvoiceTarget {
    /// Invoice attached to a message in a peer chat.
    Message {
        /// Chat peer.
        peer: tl_enums::InputPeer,
        /// Message ID.
        msg_id: MsgId,
    },
    /// Deep-link invoice slug (`t.me/$<slug>`).
    Slug(String),
    /// Stars top-up or gift invoice.
    StarsTopup {
        /// Number of Stars.
        stars: i64,
        /// Currency code.
        currency: String,
        /// Fiat price in minor units.
        amount: i64,
        /// If gifting Stars to another user.
        gift_user: Option<tl_enums::InputUser>,
        /// Optional spend purpose peer for top-up.
        spend_purpose_peer: Option<tl_enums::InputPeer>,
    },
    /// Star gift purchase invoice.
    StarGift {
        /// Recipient peer.
        recipient: tl_enums::InputPeer,
        /// Gift catalog ID.
        gift_id: i64,
        /// Optional gift message.
        message: Option<tl_enums::TextWithEntities>,
        /// Hide sender name.
        hide_name: bool,
        /// Pre-pay gift upgrade.
        include_upgrade: bool,
    },
    /// Premium gift code or giveaway invoice.
    PremiumGiftCode(Box<InvoicePremiumGiftCode>),
}

impl InvoiceTarget {
    /// Builds the corresponding TL [`tl_enums::InputInvoice`].
    pub fn to_input_invoice(&self) -> Option<tl_enums::InputInvoice> {
        match self {
            Self::Message { peer, msg_id } => Some(tl_enums::InputInvoice::Message(
                tl_types::InputInvoiceMessage {
                    peer: peer.clone(),
                    msg_id: msg_id.as_i32().unwrap_or(0),
                },
            )),
            Self::Slug(slug) => Some(tl_enums::InputInvoice::Slug(tl_types::InputInvoiceSlug {
                slug: slug.clone(),
            })),
            Self::StarsTopup {
                stars,
                currency,
                amount,
                gift_user,
                spend_purpose_peer,
            } => {
                let purpose = if let Some(user) = gift_user {
                    tl_enums::InputStorePaymentPurpose::InputStorePaymentStarsGift(
                        tl_types::InputStorePaymentStarsGift {
                            user_id: user.clone(),
                            stars: *stars,
                            currency: currency.clone(),
                            amount: *amount,
                        },
                    )
                } else {
                    tl_enums::InputStorePaymentPurpose::InputStorePaymentStarsTopup(
                        tl_types::InputStorePaymentStarsTopup {
                            stars: *stars,
                            currency: currency.clone(),
                            amount: *amount,
                            spend_purpose_peer: spend_purpose_peer.clone(),
                        },
                    )
                };
                Some(tl_enums::InputInvoice::Stars(tl_types::InputInvoiceStars {
                    purpose,
                }))
            }
            Self::StarGift {
                recipient,
                gift_id,
                message,
                hide_name,
                include_upgrade,
            } => Some(tl_enums::InputInvoice::StarGift(
                tl_types::InputInvoiceStarGift {
                    hide_name: *hide_name,
                    include_upgrade: *include_upgrade,
                    peer: recipient.clone(),
                    gift_id: *gift_id,
                    message: message.clone(),
                },
            )),
            Self::PremiumGiftCode(gift_code) => gift_code.to_input_invoice(),
        }
    }
}

/// Parses native provider parameters from `native_provider` + `native_params` JSON
/// (`Form::fillPaymentMethodInformation`, `fillStripeNativeMethod`, `fillSmartGlocalNativeMethod`).
pub fn parse_native_payment_method(
    native_provider: &str,
    native_params_json: &str,
    default_country: &str,
    can_save_information: bool,
) -> (NativePaymentMethod, NativeMethodDetails) {
    let mut ui = NativeMethodDetails {
        default_country: default_country.to_string(),
        can_save_information,
        ..NativeMethodDetails::default()
    };
    if native_provider.is_empty() || native_params_json.is_empty() {
        return (NativePaymentMethod::None, ui);
    }
    let Ok(JsonValue::Object(obj)) = serde_json::from_str::<JsonValue>(native_params_json) else {
        return (NativePaymentMethod::None, ui);
    };
    match native_provider {
        "stripe" => {
            let key = obj
                .get("publishable_key")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            if key.is_empty() {
                return (NativePaymentMethod::None, ui);
            }
            ui.supported = true;
            ui.need_country = obj
                .get("need_country")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            ui.need_zip = obj
                .get("need_zip")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            ui.need_cardholder_name = obj
                .get("need_cardholder_name")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            (
                NativePaymentMethod::Stripe {
                    publishable_key: key.to_string(),
                },
                ui,
            )
        }
        "smartglocal" => {
            let token = obj
                .get("public_token")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            if token.is_empty() {
                return (NativePaymentMethod::None, ui);
            }
            let tokenize_url = obj
                .get("tokenize_url")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string();
            ui.supported = true;
            ui.need_country = false;
            ui.need_zip = false;
            ui.need_cardholder_name = false;
            (
                NativePaymentMethod::SmartGlocal {
                    public_token: token.to_string(),
                    tokenize_url,
                },
                ui,
            )
        }
        _ => (NativePaymentMethod::None, ui),
    }
}

/// Payment method selection update returned by [`PaymentForm::set_payment_credentials`] and
/// [`PaymentForm::choose_saved_method`] (`Payments::PaymentMethodUpdate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaymentMethodUpdate {
    /// `true` if the user wants to save credentials on Telegram servers, has no 2FA password,
    /// and should be prompted to create one.
    pub request_new_password: bool,
}

/// Pure-logic payment form model (`Payments::Form` in `payments_form.cpp`).
#[derive(Debug, Clone, Default)]
pub struct PaymentForm {
    /// Invoice details.
    pub invoice: Invoice,
    /// Payment form server metadata.
    pub details: FormDetails,
    /// Saved info snapshot from server (used for `has_changes` dirty tracking).
    pub saved_information: RequestedInformation,
    /// Active customer information.
    pub information: RequestedInformation,
    /// Staged information being validated by `payments.validateRequestedInfo`.
    pub validated_information: Option<RequestedInformation>,
    /// Payment methods (native, saved, new, additional).
    pub payment_method: PaymentMethod,
    /// Available shipping options and current selection.
    pub shipping_options: ShippingOptions,
    /// Server-issued `requested_info_id` from `payments.validateRequestedInfo`.
    pub requested_information_id: String,
    /// Whether the user has trusted the merchant bot for payments.
    pub bot_trusted: bool,
    /// Default phone hint for the current user.
    pub default_phone: String,
    /// Default country ISO2 hint for the current user.
    pub default_country: String,
}

impl PaymentForm {
    /// Creates a new empty [`PaymentForm`] with optional default user phone/country hints.
    pub fn new(default_phone: impl Into<String>, default_country: impl Into<String>) -> Self {
        Self {
            default_phone: default_phone.into(),
            default_country: default_country.into(),
            ..Self::default()
        }
    }

    /// Builds `payments.getPaymentForm` for `invoice` with optional WebView `theme_params` JSON.
    pub fn build_get_payment_form_request(
        invoice: tl_enums::InputInvoice,
        theme_params_json: Option<String>,
    ) -> tl_fn::payments::GetPaymentForm {
        tl_fn::payments::GetPaymentForm {
            invoice,
            theme_params: theme_params_json
                .map(|data| tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data })),
        }
    }

    /// Builds `payments.getPaymentReceipt` for a paid invoice message.
    pub fn build_get_payment_receipt_request(
        peer: tl_enums::InputPeer,
        msg_id: MsgId,
    ) -> tl_fn::payments::GetPaymentReceipt {
        tl_fn::payments::GetPaymentReceipt {
            peer,
            msg_id: msg_id.as_i32().unwrap_or(0),
        }
    }

    /// Applies a `payments.PaymentForm` TL response (`Form::requestForm` / `processForm`).
    pub fn apply_payment_form_tl(&mut self, tl: &tl_enums::payments::PaymentForm) -> FormLoadEvent {
        match tl {
            tl_enums::payments::PaymentForm::PaymentForm(data) => {
                self.process_invoice_tl(&data.invoice);
                let native_params_json = data
                    .native_params
                    .as_ref()
                    .map(|tl_enums::DataJSON::DataJSON(j)| j.data.clone())
                    .unwrap_or_default();
                self.details = FormDetails {
                    form_id: data.form_id,
                    url: data.url.clone(),
                    native_provider: data.native_provider.clone().unwrap_or_default(),
                    terms_bot_username: String::new(),
                    native_params_json,
                    bot_id: UserId::from_tl(data.bot_id),
                    provider_id: UserId::from_tl(data.provider_id),
                    can_save_credentials: data.can_save_credentials,
                    password_missing: data.password_missing,
                    terms_accepted: false,
                };
                self.invoice.cover.title = data.title.clone();
                self.invoice.cover.description = data.description.clone();

                // Extract seller/provider names from returned users list if available.
                for user in &data.users {
                    if let tl_enums::User::User(u) = user {
                        let uid = UserId::from_tl(u.id);
                        let display_name = format_user_name(u);
                        if uid == self.details.bot_id {
                            self.invoice.cover.seller = display_name.clone();
                            if let Some(ref uname) = u.username {
                                self.details.terms_bot_username = uname.clone();
                            }
                        }
                        if uid == self.details.provider_id {
                            self.invoice.provider = display_name;
                        }
                    }
                }

                if let Some(ref saved_info) = data.saved_info {
                    let info = RequestedInformation::from_tl(
                        saved_info,
                        self.default_phone.clone(),
                        self.default_country.clone(),
                    );
                    self.saved_information = info.clone();
                    self.information = info;
                }

                self.payment_method.saved_credentials.clear();
                self.payment_method.saved_credentials_index = 0;
                if let Some(ref creds) = data.saved_credentials {
                    for cred in creds {
                        let tl_enums::PaymentSavedCredentials::Card(c) = cred;
                        self.payment_method
                            .saved_credentials
                            .push(SavedCredentials {
                                id: c.id.clone(),
                                title: c.title.clone(),
                            });
                    }
                }

                self.payment_method.additional_methods.clear();
                if let Some(ref methods) = data.additional_methods {
                    for m in methods {
                        let tl_enums::PaymentFormMethod::PaymentFormMethod(pm) = m;
                        self.payment_method
                            .additional_methods
                            .push(PaymentMethodAdditional {
                                title: pm.title.clone(),
                                url: pm.url.clone(),
                            });
                    }
                }

                self.refresh_native_payment_method();
                FormLoadEvent::FormReady
            }
            tl_enums::payments::PaymentForm::Stars(data) => {
                let tl_enums::Invoice::Invoice(ref inv) = data.invoice;
                let amount = inv
                    .prices
                    .first()
                    .map(|tl_enums::LabeledPrice::LabeledPrice(p)| p.amount)
                    .unwrap_or(0);
                if inv.currency != CREDITS_CURRENCY || amount <= 0 {
                    return FormLoadEvent::Error(FormError::new(
                        FormErrorKind::Form,
                        "Bad Stars Form.",
                    ));
                }
                FormLoadEvent::CreditsPaymentStarted(CreditsFormData {
                    form_id: data.form_id,
                    bot_id: UserId::from_tl(data.bot_id),
                    title: data.title.clone(),
                    description: data.description.clone(),
                    credits: amount as u64,
                    currency: inv.currency.clone(),
                    subscription_period: inv.subscription_period.unwrap_or(0),
                    star_gift_form: false,
                })
            }
            tl_enums::payments::PaymentForm::StarGift(data) => {
                let tl_enums::Invoice::Invoice(ref inv) = data.invoice;
                let amount = inv
                    .prices
                    .first()
                    .map(|tl_enums::LabeledPrice::LabeledPrice(p)| p.amount)
                    .unwrap_or(0);
                if inv.currency != CREDITS_CURRENCY || amount <= 0 {
                    return FormLoadEvent::Error(FormError::new(
                        FormErrorKind::Form,
                        "Bad Stars Form.",
                    ));
                }
                FormLoadEvent::CreditsPaymentStarted(CreditsFormData {
                    form_id: data.form_id,
                    bot_id: UserId::ZERO,
                    title: String::new(),
                    description: String::new(),
                    credits: amount as u64,
                    currency: inv.currency.clone(),
                    subscription_period: 0,
                    star_gift_form: true,
                })
            }
        }
    }

    /// Applies a `payments.PaymentReceipt` TL response (`Form::processReceipt`).
    pub fn apply_payment_receipt_tl(
        &mut self,
        tl: &tl_enums::payments::PaymentReceipt,
    ) -> FormLoadEvent {
        match tl {
            tl_enums::payments::PaymentReceipt::PaymentReceipt(data) => {
                self.process_invoice_tl(&data.invoice);
                self.invoice.receipt = Receipt {
                    date: data.date,
                    total_amount: data.total_amount,
                    currency: data.currency.clone(),
                    paid: true,
                };
                self.invoice.tips_selected = data.tip_amount.unwrap_or(0);
                self.details = FormDetails {
                    bot_id: UserId::from_tl(data.bot_id),
                    provider_id: UserId::from_tl(data.provider_id),
                    ..FormDetails::default()
                };
                self.invoice.cover.title = data.title.clone();
                self.invoice.cover.description = data.description.clone();
                for user in &data.users {
                    if let tl_enums::User::User(u) = user
                        && UserId::from_tl(u.id) == self.details.bot_id
                    {
                        self.invoice.cover.seller = format_user_name(u);
                    }
                }
                if let Some(ref info) = data.info {
                    let parsed = RequestedInformation::from_tl(
                        info,
                        self.default_phone.clone(),
                        self.default_country.clone(),
                    );
                    self.saved_information = parsed.clone();
                    self.information = parsed;
                }
                if let Some(ref shipping) = data.shipping {
                    let opt = ShippingOption::from_tl(shipping);
                    let id = opt.id.clone();
                    self.shipping_options = ShippingOptions {
                        currency: self.invoice.currency.clone(),
                        list: vec![opt],
                        selected_id: id,
                    };
                }
                self.payment_method.saved_credentials = vec![SavedCredentials {
                    id: "(used)".to_string(),
                    title: data.credentials_title.clone(),
                }];
                self.payment_method.saved_credentials_index = 0;
                self.refresh_native_payment_method();
                FormLoadEvent::FormReady
            }
            tl_enums::payments::PaymentReceipt::Stars(data) => {
                FormLoadEvent::CreditsReceiptReady(CreditsReceiptData {
                    id: data.transaction_id.clone(),
                    title: data.title.clone(),
                    description: data.description.clone(),
                    peer_id: PeerId::from_user(UserId::from_tl(data.bot_id)),
                    credits: CreditsAmount::stars(data.total_amount),
                    date: data.date,
                })
            }
        }
    }

    fn process_invoice_tl(&mut self, tl: &tl_enums::Invoice) {
        let tl_enums::Invoice::Invoice(inv) = tl;
        let cover = std::mem::take(&mut self.invoice.cover);
        self.invoice = Invoice {
            cover,
            prices: inv.prices.iter().map(LabeledPrice::from_tl).collect(),
            suggested_tips: inv.suggested_tip_amounts.clone().unwrap_or_default(),
            tips_max: inv.max_tip_amount.unwrap_or(0),
            tips_selected: 0,
            currency: inv.currency.clone(),
            receipt: Receipt::default(),
            is_name_requested: inv.name_requested,
            is_phone_requested: inv.phone_requested,
            is_email_requested: inv.email_requested,
            is_shipping_address_requested: inv.shipping_address_requested,
            is_recurring: inv.recurring,
            is_flexible: inv.flexible,
            is_test: inv.test,
            provider: String::new(),
            terms_url: inv.terms_url.clone().unwrap_or_default(),
            phone_sent_to_provider: inv.phone_to_provider,
            email_sent_to_provider: inv.email_to_provider,
            subscription_period: inv.subscription_period,
        };
    }

    fn refresh_native_payment_method(&mut self) {
        let can_save = self.details.can_save_credentials || self.details.password_missing;
        let (native, details) = parse_native_payment_method(
            &self.details.native_provider,
            &self.details.native_params_json,
            &self.default_country,
            can_save,
        );
        self.payment_method.native = native;
        self.payment_method.native_details = details;
        self.payment_method.url = self.details.url.clone();
    }

    /// Locally validates customer information before calling `payments.validateRequestedInfo`
    /// (`Form::informationErrorLocal`).
    pub fn validate_information_local(&self, info: &RequestedInformation) -> Result<(), FormError> {
        if self.invoice.is_shipping_address_requested {
            if info.shipping_address.address1.is_empty() {
                return Err(FormError::new(
                    FormErrorKind::Validate,
                    "ADDRESS_STREET_LINE1_INVALID",
                ));
            }
            if info.shipping_address.city.is_empty() {
                return Err(FormError::new(
                    FormErrorKind::Validate,
                    "ADDRESS_CITY_INVALID",
                ));
            }
            if info.shipping_address.country_iso2.is_empty() {
                return Err(FormError::new(
                    FormErrorKind::Validate,
                    "ADDRESS_COUNTRY_INVALID",
                ));
            }
        }
        if self.invoice.is_name_requested && info.name.is_empty() {
            return Err(FormError::new(
                FormErrorKind::Validate,
                "REQ_INFO_NAME_INVALID",
            ));
        }
        if self.invoice.is_email_requested && info.email.is_empty() {
            return Err(FormError::new(
                FormErrorKind::Validate,
                "REQ_INFO_EMAIL_INVALID",
            ));
        }
        if self.invoice.is_phone_requested && info.phone.is_empty() {
            return Err(FormError::new(
                FormErrorKind::Validate,
                "REQ_INFO_PHONE_INVALID",
            ));
        }
        Ok(())
    }

    /// Validates `info` locally, stages it, and returns `payments.validateRequestedInfo`
    /// (`Form::validateInformation`).
    pub fn build_validate_requested_info_request(
        &mut self,
        info: RequestedInformation,
        invoice: tl_enums::InputInvoice,
    ) -> Result<tl_fn::payments::ValidateRequestedInfo, FormError> {
        self.validate_information_local(&info)?;
        let req = tl_fn::payments::ValidateRequestedInfo {
            save: info.save,
            invoice,
            info: info.to_tl(),
        };
        self.validated_information = Some(info);
        Ok(req)
    }

    /// Applies `payments.ValidatedRequestedInfo`, updating `requested_information_id`,
    /// shipping options (preserving prior selection or auto-selecting a single option),
    /// and saved info (`Form::validateInformation` done handler).
    pub fn apply_validated_requested_info_tl(
        &mut self,
        tl: &tl_enums::payments::ValidatedRequestedInfo,
    ) {
        let tl_enums::payments::ValidatedRequestedInfo::ValidatedRequestedInfo(data) = tl;
        let old_selected_id = self.shipping_options.selected_id.clone();

        self.requested_information_id = data.id.clone().unwrap_or_default();
        let list: Vec<ShippingOption> = data
            .shipping_options
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(ShippingOption::from_tl)
            .collect();

        let mut selected_id = if list.iter().any(|o| o.id == old_selected_id) {
            old_selected_id
        } else {
            String::new()
        };
        if selected_id.is_empty() && list.len() == 1 {
            selected_id = list[0].id.clone();
        }
        self.shipping_options = ShippingOptions {
            currency: self.invoice.currency.clone(),
            list,
            selected_id,
        };

        if let Some(validated) = self.validated_information.take() {
            self.information = validated.clone();
            if self.information.save {
                self.saved_information = validated;
            }
        }
    }

    /// Locally validates native card details against this form's `native_details`
    /// (`Form::cardErrorLocal`).
    pub fn validate_card_local(
        &self,
        details: &UncheckedCardDetails,
        threshold: YearMonth,
    ) -> Result<(), FormError> {
        validate_card_details(details, &self.payment_method.native_details, threshold)
            .map_err(|code| FormError::new(FormErrorKind::Validate, code))
    }

    /// Stores newly tokenized payment credentials and selects them (`Form::setPaymentCredentials`).
    pub fn set_payment_credentials(&mut self, credentials: NewCredentials) -> PaymentMethodUpdate {
        let request_new_password = credentials.save_on_server
            && !self.details.can_save_credentials
            && self.details.password_missing;
        self.payment_method.new_credentials = credentials;
        self.payment_method.saved_credentials_index = self.payment_method.saved_credentials.len();
        PaymentMethodUpdate {
            request_new_password,
        }
    }

    /// Selects a saved payment method by `id`, or selects `new_credentials` when `id` is empty
    /// (`Form::chooseSavedMethod`).
    pub fn choose_saved_method(&mut self, id: &str) -> PaymentMethodUpdate {
        let list_len = self.payment_method.saved_credentials.len();
        if id.is_empty() && !self.payment_method.new_credentials.is_empty() {
            self.payment_method.saved_credentials_index = list_len;
        } else {
            self.payment_method.saved_credentials_index = self
                .payment_method
                .saved_credentials
                .iter()
                .position(|c| c.id == id)
                .unwrap_or(0);
        }
        let request_new_password = self.payment_method.saved_credentials_index == list_len
            && !self.payment_method.new_credentials.is_empty()
            && self.payment_method.new_credentials.save_on_server
            && !self.details.can_save_credentials
            && self.details.password_missing;
        PaymentMethodUpdate {
            request_new_password,
        }
    }

    /// Updates `can_save_credentials` when user cloud password state changes (`Form::setHasPassword`).
    pub fn set_has_password(&mut self, has_password: bool) {
        if self.details.password_missing {
            self.details.can_save_credentials = has_password;
        }
    }

    /// Selects a shipping option by ID (`Form::setShippingOption`).
    pub fn set_shipping_option(&mut self, id: impl Into<String>) {
        self.shipping_options.selected_id = id.into();
    }

    /// Sets the selected tip amount, clamped to `invoice.tips_max` (`Form::setTips`).
    pub fn set_tips(&mut self, value: i64) {
        self.invoice.tips_selected = value.max(0).min(self.invoice.tips_max);
    }

    /// Marks the invoice terms-of-service URL as accepted (`Form::acceptTerms`).
    pub fn accept_terms(&mut self) {
        self.details.terms_accepted = true;
    }

    /// Marks the merchant bot as trusted for payments (`Form::trustBot`).
    pub fn trust_bot(&mut self) {
        self.bot_trusted = true;
    }

    /// Returns `true` if the user has unsaved edits or newly entered credentials (`Form::hasChanges`).
    pub fn has_changes(&self) -> bool {
        let info = self
            .validated_information
            .as_ref()
            .unwrap_or(&self.information);
        *info != self.saved_information
            || (!self.payment_method.new_credentials.is_empty()
                && self.payment_method.saved_credentials_index
                    >= self.payment_method.saved_credentials.len())
    }

    /// Builds `payments.sendPaymentForm`, or reports `TmpPasswordRequired` / `BotTrustRequired`
    /// (`Form::submit`).
    pub fn build_send_payment_form_request(
        &self,
        invoice: tl_enums::InputInvoice,
        tmp_password: Option<&[u8]>,
    ) -> Result<SendPaymentAction, FormError> {
        if !self.payment_method.has_selected_method() {
            return Err(FormError::new(
                FormErrorKind::Send,
                "PAYMENT_CREDENTIALS_INVALID",
            ));
        }

        let using_saved = self.payment_method.saved_credentials_index
            < self.payment_method.saved_credentials.len();
        if using_saved && tmp_password.is_none_or(<[u8]>::is_empty) {
            return Ok(SendPaymentAction::TmpPasswordRequired);
        }
        if !self.bot_trusted {
            return Ok(SendPaymentAction::BotTrustRequired {
                bot_id: self.details.bot_id,
                provider_id: self.details.provider_id,
            });
        }

        let credentials = if using_saved {
            let saved =
                &self.payment_method.saved_credentials[self.payment_method.saved_credentials_index];
            tl_enums::InputPaymentCredentials::Saved(tl_types::InputPaymentCredentialsSaved {
                id: saved.id.clone(),
                tmp_password: tmp_password.unwrap_or_default().to_vec(),
            })
        } else {
            tl_enums::InputPaymentCredentials::InputPaymentCredentials(
                tl_types::InputPaymentCredentials {
                    save: self.payment_method.new_credentials.save_on_server
                        && self.details.can_save_credentials,
                    data: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                        data: self.payment_method.new_credentials.data.clone(),
                    }),
                },
            )
        };

        Ok(SendPaymentAction::Send(Box::new(
            tl_fn::payments::SendPaymentForm {
                form_id: self.details.form_id,
                invoice,
                requested_info_id: (!self.requested_information_id.is_empty())
                    .then(|| self.requested_information_id.clone()),
                shipping_option_id: (!self.shipping_options.selected_id.is_empty())
                    .then(|| self.shipping_options.selected_id.clone()),
                credentials,
                tip_amount: (self.invoice.tips_max > 0).then_some(self.invoice.tips_selected),
            },
        )))
    }

    /// Converts a `payments.PaymentResult` TL response into [`PaymentResultOutcome`].
    pub fn apply_payment_result_tl(tl: tl_enums::payments::PaymentResult) -> PaymentResultOutcome {
        match tl {
            tl_enums::payments::PaymentResult::PaymentResult(res) => {
                PaymentResultOutcome::Finished(Box::new(res.updates))
            }
            tl_enums::payments::PaymentResult::PaymentVerificationNeeded(ver) => {
                PaymentResultOutcome::VerificationNeeded { url: ver.url }
            }
        }
    }
}

fn format_user_name(user: &tl_types::User) -> String {
    match (&user.first_name, &user.last_name) {
        (Some(f), Some(l)) if !l.is_empty() => format!("{f} {l}"),
        (Some(f), _) => f.clone(),
        (None, Some(l)) => l.clone(),
        (None, None) => String::new(),
    }
}
