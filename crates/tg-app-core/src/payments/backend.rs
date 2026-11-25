//! Decoupled backend traits and view-model bundles for Payments, Stars, Star Gifts,
//! Premium, Boosts/Giveaways, and Telegram Passport UI screens.
//!
//! Port origin: `payments/payments_checkout_process.h`, `payments/payments_form.h`,
//! `api/api_credits.h`, `api/api_premium.h`, `passport/passport_form_controller.h`.

use tg_data::TimeId;
use tg_passport::types::{
    FileType, FormRequest, PassportConfig, PassportFile, PassportForm, ValueMap, ValueType,
};
use tg_payments::card::UncheckedCardDetails;
use tg_payments::credits::{
    CreditTopupOption, CreditsAmount, CreditsHistoryEntry, PaidMediaCost, PaidReactionConfig,
    SubscriptionEntry,
};
use tg_payments::form::{NewCredentials, PaymentForm, RequestedInformation, ShippingOption};
use tg_payments::premium::{
    Boost, BoostStatus, GiveawayInfo, LevelLimits, MyBoostSlot, PremiumLimits, PremiumPromoData,
    PremiumSubscriptionOption,
};
use tg_payments::star_gifts::{CanSendGiftResult, SavedStarGift, StarGift};

/// Bundle returned when loading a checkout invoice form.
#[derive(Debug, Clone)]
pub struct CheckoutBundle {
    /// Stable identifier of the mock/remote invoice.
    pub invoice_id: String,
    /// Populated `tg-payments` form model.
    pub form: PaymentForm,
    /// Subtitle badge shown on the cover artwork.
    pub cover_badge: String,
    /// Whether this invoice demonstrates the 3-D Secure verification URL step on submit.
    pub requires_3ds_demo: bool,
    /// Verification URL loaded inside the `tg-webview` host when 3-D Secure triggers.
    pub verification_url: String,
}

/// Result of validating customer info (`payments.validateRequestedInfo`).
#[derive(Debug, Clone)]
pub struct ValidatedInfoResult {
    /// Server-issued requested info ID.
    pub requested_info_id: String,
    /// Available shipping options for the validated address.
    pub shipping_options: Vec<ShippingOption>,
}

/// Outcome of submitting a payment form (`payments.sendPaymentForm`).
#[derive(Debug, Clone)]
pub enum PaymentSubmitOutcome {
    /// Payment completed (test/mock only; never real money).
    Paid {
        /// Payment Unix timestamp.
        date: TimeId,
        /// Total amount paid in minor units.
        total_amount: i64,
        /// Currency code.
        currency: String,
        /// Receipt reference ID.
        receipt_id: String,
    },
    /// 3-D Secure / provider verification URL step required.
    VerificationNeeded {
        /// Verification URL opened in the `tg-webview` verification host.
        url: String,
        /// Provider display name for the bottom attribution bar.
        provider: String,
    },
}

/// Top reactor entry shown in the Paid Reactions confirmation box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaidReactorEntry {
    /// Display name (fictional placeholder name).
    pub name: String,
    /// Avatar initials.
    pub initials: String,
    /// Stars reacted so far.
    pub stars: i32,
    /// Whether this entry represents the current user.
    pub is_self: bool,
}

/// Sample paid media preview metadata for the paid media confirmation box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaidMediaPreview {
    /// Sample media bundle ID.
    pub media_id: u64,
    /// Channel or creator display title.
    pub author_title: String,
    /// Caption shown below the blurred preview.
    pub caption: String,
    /// Number of photos in the paid bundle.
    pub photo_count: usize,
    /// Number of videos in the paid bundle.
    pub video_count: usize,
    /// Cost model from `tg-payments`.
    pub cost: PaidMediaCost,
}

/// Sample post context for the paid reactions confirmation box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaidReactionContext {
    /// Target post ID.
    pub post_id: i32,
    /// Channel title.
    pub channel_title: String,
    /// Post excerpt text.
    pub post_excerpt: String,
    /// Clamping configuration from `tg-payments`.
    pub config: PaidReactionConfig,
    /// Top reactors leaderboard (up to 3 entries).
    pub top_reactors: Vec<PaidReactorEntry>,
}

/// Bundle for the Telegram Stars settings screen (`settings_credits.cpp`).
#[derive(Debug, Clone)]
pub struct CreditsOverviewBundle {
    /// Current Stars balance.
    pub balance: CreditsAmount,
    /// Current TON balance (display only).
    pub ton_balance: CreditsAmount,
    /// Available Stars top-up packs.
    pub topup_options: Vec<CreditTopupOption>,
    /// Transaction history ledger.
    pub history: Vec<CreditsHistoryEntry>,
    /// Human-readable peer names keyed by `CreditsHistoryEntry::id`.
    pub history_peer_names: Vec<(String, String)>,
    /// Active & past recurring Stars subscriptions.
    pub subscriptions: Vec<SubscriptionEntry>,
    /// Sample paid media bundle for testing the confirmation box.
    pub sample_paid_media: PaidMediaPreview,
    /// Sample post context for testing the paid reactions confirmation box.
    pub sample_paid_reaction: PaidReactionContext,
}

/// Catalog gift display metadata wrapping `tg_payments::star_gifts::StarGift`.
#[derive(Debug, Clone)]
pub struct CatalogGiftItem {
    /// Core `tg-payments` gift data.
    pub gift: StarGift,
    /// Short display title for the picker grid.
    pub title: String,
    /// Stylized vector emblem kind (`"star"`, `"cake"`, `"gem"`, `"trophy"`, `"rose"`, `"crown"`, `"rocket"`, `"heart"`).
    pub artwork_kind: String,
}

/// Received gift display metadata wrapping `tg_payments::star_gifts::SavedStarGift`.
#[derive(Debug, Clone)]
pub struct ReceivedGiftItem {
    /// Core `tg-payments` saved gift data.
    pub saved: SavedStarGift,
    /// Gift display title.
    pub title: String,
    /// Sender display name (`"Anonymous"` if hidden).
    pub sender_name: String,
    /// Stylized vector emblem kind.
    pub artwork_kind: String,
}

/// Action triggered from the received Star Gift detail box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivedGiftAction {
    /// Upgrade a regular upgradable gift into a unique collectible.
    Upgrade,
    /// Convert a gift into Stars.
    ConvertToStars,
    /// Transfer a unique collectible to another user.
    Transfer,
    /// Toggle pinning the gift on the user's profile.
    TogglePin,
    /// Toggle visibility on the user's profile.
    ToggleVisibility,
}

/// Bundle for the Star Gifts screen (`boxes/star_gift_box.cpp`).
#[derive(Debug, Clone)]
pub struct StarGiftsBundle {
    /// Current user Stars balance for affordability checks.
    pub balance: CreditsAmount,
    /// Default recipient display name for the Send Gift flow.
    pub default_recipient: String,
    /// Catalog gifts available in the picker grid.
    pub catalog: Vec<CatalogGiftItem>,
    /// Received gifts owned by the current user.
    pub received: Vec<ReceivedGiftItem>,
}

/// One Telegram Premium feature row (`settings_premium.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumFeatureItem {
    /// Stable feature slug (matches `FallbackOrder` in `settings_premium.cpp`).
    pub slug: String,
    /// Feature title.
    pub title: String,
    /// Short feature description.
    pub description: String,
    /// Detailed comparison note shown when expanded.
    pub detail: String,
    /// Accent RGB tint for the feature icon badge.
    pub icon_rgb: [u8; 3],
    /// Short symbol drawn inside the icon badge.
    pub icon_symbol: String,
    /// Whether to display the "NEW" badge.
    pub is_new: bool,
}

/// Non-personal Telegram Premium promo telemetry event sent via `help.saveAppLog` (`peer = 0`),
/// matching `SendScreenShow` / `SendScreenAccept` (`settings/sections/settings_premium.cpp:519-549`).
///
/// Personal identifiers (peer IDs, phone numbers, account data) are intentionally excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PremiumPromoLogEvent {
    /// Premium promo screen opened (`"premium.promo_screen_show"`).
    ScreenShow {
        /// Ordered promo feature slugs shown on screen (`premium_promo_order`).
        order: Vec<String>,
        /// Entry-point source tag (`source`; empty defaults to `"settings"`).
        source: String,
    },
    /// User accepted / clicked subscribe on the Premium promo screen (`"premium.promo_screen_accept"`).
    ScreenAccept,
}

/// Bundle for the Telegram Premium promo screen (`settings_premium.cpp`).
#[derive(Debug, Clone)]
pub struct PremiumPromoBundle {
    /// Core promo data and subscription tiers from `tg-payments`.
    pub promo: PremiumPromoData,
    /// Account limits for free users.
    pub default_limits: PremiumLimits,
    /// Account limits for Premium subscribers.
    pub premium_limits: PremiumLimits,
    /// Ordered feature list with icons.
    pub features: Vec<PremiumFeatureItem>,
}

/// Read-only payment form preview for a Telegram Premium subscription tier
/// (`payments.getPaymentForm`). Never auto-purchases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumInvoicePreview {
    /// Server-issued payment form ID.
    pub form_id: i64,
    /// Invoice title (e.g. `"Telegram Premium (12 months)"`).
    pub title: String,
    /// Invoice description or billing terms.
    pub description: String,
    /// Formatted total price label (e.g. `"$39.99"`).
    pub total_label: String,
    /// Billing duration in months.
    pub months: i32,
    /// Payment provider display name (e.g. `"Stripe"` or `"PremiumBot"`).
    pub provider: String,
    /// Invoice or bot checkout URL returned by Telegram.
    pub checkout_url: String,
    /// Whether the previewed invoice is in test mode.
    pub is_test: bool,
}

/// One Giveaway info card item wrapping `tg_payments::premium::GiveawayInfo`.
#[derive(Debug, Clone)]
pub struct GiveawayInfoItem {
    /// Display title of the giveaway.
    pub title: String,
    /// Hosting channel display title.
    pub channel_title: String,
    /// Additional channels participants must subscribe to.
    pub additional_channels: Vec<String>,
    /// Prize summary string (e.g. `"10 x Telegram Premium (3 months)"`).
    pub prize_summary: String,
    /// Optional custom prize description.
    pub prize_description: String,
    /// Whether restricted to new subscribers only.
    pub only_new_subscribers: bool,
    /// Allowed countries summary (`"Worldwide"` or country list).
    pub countries_summary: String,
    /// Core giveaway state from `tg-payments`.
    pub info: GiveawayInfo,
}

/// Bundle for the Boosts & Giveaway info boxes screen.
#[derive(Debug, Clone)]
pub struct BoostsGiveawayBundle {
    /// Target channel display title.
    pub channel_title: String,
    /// Current channel boost status from `tg-payments`.
    pub boost_status: BoostStatus,
    /// Level unlock thresholds from `tg-payments`.
    pub level_limits: LevelLimits,
    /// Current user's boost slots.
    pub my_slots: Vec<MyBoostSlot>,
    /// Recent channel boost entries with display names.
    pub recent_boosts: Vec<(Boost, String)>,
    /// Sample giveaways (active + finished) for giveaway info boxes.
    pub giveaways: Vec<GiveawayInfoItem>,
}

/// Backend trait decoupling all Payments, Stars, Star Gifts, Premium, and Boosts/Giveaways UI
/// from network and storage layers.
///
/// Implementations in `tg-app` MUST NOT initiate real payments or contact external payment
/// providers.
pub trait PaymentsBackend: Send {
    /// Loads a checkout invoice form by identifier.
    fn load_checkout_form(&self, invoice_id: &str) -> Result<CheckoutBundle, String>;

    /// Validates customer info and returns shipping options (if flexible).
    fn validate_requested_info(
        &mut self,
        invoice_id: &str,
        info: &RequestedInformation,
    ) -> Result<ValidatedInfoResult, String>;

    /// Locally validates and produces a test-only token for a bank card (no network calls).
    fn tokenize_test_card(
        &self,
        details: &UncheckedCardDetails,
        save_on_server: bool,
    ) -> Result<NewCredentials, String>;

    /// Verifies a local 2FA password to authorize using a saved card (`account.getTmpPassword`).
    fn verify_tmp_password(&self, password: &str) -> Result<Vec<u8>, String>;

    /// Submits a test/mock checkout form. Never contacts any real payment gateway.
    fn submit_payment_form(
        &mut self,
        invoice_id: &str,
        form_id: i64,
        shipping_option_id: Option<&str>,
        tip_amount: i64,
        credentials_title: &str,
        require_3ds: bool,
    ) -> Result<PaymentSubmitOutcome, String>;

    /// Loads Stars balance, top-up packs, transaction history, and subscriptions.
    fn load_credits_overview(&self) -> Result<CreditsOverviewBundle, String>;

    /// Requests a Stars top-up payment form (`payments.getPaymentForm` with `InputInvoiceStars`;
    /// read-only preview — never auto-charges).
    fn topup_stars(&mut self, option: &CreditTopupOption) -> Result<String, String>;

    /// Cancels (`canceled = true`) or renews (`canceled = false`) a recurring Telegram Stars
    /// subscription (`payments.changeStarsSubscription`).
    fn change_stars_subscription(
        &mut self,
        subscription_id: &str,
        canceled: bool,
    ) -> Result<String, String>;

    /// Confirms unlocking a paid media post with Stars and returns updated balance.
    fn confirm_paid_media(&mut self, media_id: u64, stars: i64) -> Result<CreditsAmount, String>;

    /// Confirms sending a paid Star reaction and returns updated balance.
    fn confirm_paid_reaction(
        &mut self,
        post_id: i32,
        stars: i32,
        anonymous: bool,
    ) -> Result<CreditsAmount, String>;

    /// Loads the Star Gifts catalog and user's received gifts.
    fn load_star_gifts(&self) -> Result<StarGiftsBundle, String>;

    /// Sends a Star Gift (`payments.getPaymentForm` + `payments.sendStarsForm` when live).
    fn send_star_gift(
        &mut self,
        gift_id: u64,
        recipient: &str,
        message: &str,
        hide_name: bool,
        include_upgrade: bool,
    ) -> Result<String, String>;

    /// Applies an upgrade / convert / transfer / pin / visibility action on a received Star Gift.
    fn apply_received_gift_action(
        &mut self,
        gift_id: u64,
        action: ReceivedGiftAction,
    ) -> Result<String, String>;

    /// Transfers a received unique collectible Star Gift to `recipient` (username or display name).
    fn transfer_received_gift(&mut self, gift_id: u64, recipient: &str) -> Result<String, String> {
        if recipient.trim().is_empty() {
            return Err("Recipient is required to transfer a Star Gift.".to_owned());
        }
        self.apply_received_gift_action(gift_id, ReceivedGiftAction::Transfer)
    }

    /// Returns candidate recipient display names / `@usernames` for the Star Gift transfer picker.
    fn transfer_recipient_candidates(&self) -> Vec<String> {
        Vec::new()
    }

    /// Loads Telegram Premium promo data, tiers, limits, and features.
    fn load_premium_promo(&self) -> Result<PremiumPromoBundle, String>;

    /// Sends a non-personal Premium promo telemetry event via `help.saveAppLog` (`peer = 0`),
    /// matching `SendScreenShow` / `SendScreenAccept` (`settings/sections/settings_premium.cpp`).
    fn log_premium_promo_event(&self, _event: &PremiumPromoLogEvent) -> Result<bool, String> {
        Ok(false)
    }

    /// Loads a read-only payment form preview for a Telegram Premium subscription tier
    /// (`payments.getPaymentForm`). Never submits a charge automatically.
    fn preview_premium_invoice(
        &self,
        option: &PremiumSubscriptionOption,
    ) -> Result<PremiumInvoicePreview, String>;

    /// Loads Boosts & Giveaway info bundles.
    fn load_boosts_giveaway(&self) -> Result<BoostsGiveawayBundle, String>;

    /// Applies one or more boost `slots` to `channel_title` (`premium.applyBoost`) and returns
    /// the refreshed [`BoostsGiveawayBundle`].
    fn apply_channel_boost(
        &mut self,
        channel_title: &str,
        slots: &[i32],
    ) -> Result<BoostsGiveawayBundle, String>;

    /// Checks whether a catalog Star Gift can currently be sent (`payments.checkCanSendGift`).
    fn check_can_send_gift(&self, _gift_id: u64) -> Result<CanSendGiftResult, String> {
        Ok(CanSendGiftResult::Ok)
    }

    /// Loads Stars gift pack options for gifting Stars to `recipient`
    /// (`payments.getStarsGiftOptions`).
    fn load_stars_gift_options(&self, _recipient: &str) -> Result<Vec<CreditTopupOption>, String> {
        Ok(Vec::new())
    }

    /// Optional access to the WP-16 Star Gifts marketplace, giveaways, boost replacement,
    /// and Stars affiliate program backend.
    fn as_marketplace_backend(
        &mut self,
    ) -> Option<&mut dyn crate::payments::marketplace_backend::StarGiftsMarketplaceBackend> {
        None
    }
}

/// Initial state bundle for a Telegram Passport bot authorization session.
#[derive(Debug, Clone)]
pub struct PassportSessionBundle {
    /// Requesting bot display name (fictional demo bot).
    pub bot_name: String,
    /// Requesting bot username without `@`.
    pub bot_username: String,
    /// Whether the account already has a 2FA cloud password configured.
    pub has_cloud_password: bool,
    /// Password hint shown on the password unlock screen.
    pub password_hint: String,
    /// Whether a recovery email is configured.
    pub has_recovery: bool,
    /// Whether the account already has stored Passport values.
    pub has_secure_values: bool,
    /// Populated `tg-passport` form model (values unlocked once password is verified).
    pub form: PassportForm,
}

/// Outcome of unlocking or creating a Telegram Passport cloud password/secret.
#[derive(Debug, Clone)]
pub struct PassportUnlockResult {
    /// Decrypted or freshly initialized Passport form.
    pub form: PassportForm,
    /// Status note displayed in a toast.
    pub notice: String,
}

/// Pending verification challenge issued by `account.sendVerifyPhoneCode` or
/// `account.sendVerifyEmailCode` (`passport_form_controller.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassportVerificationChallenge {
    /// Contact type being verified (`ValueType::Phone` or `ValueType::Email`).
    pub value_type: ValueType,
    /// Normalized phone number or email address.
    pub target: String,
    /// Expected code length (`-1` if unspecified by server).
    pub code_length: i32,
    /// `phone_code_hash` returned by `account.sendVerifyPhoneCode` (for phone only).
    pub phone_code_hash: Option<String>,
    /// Masked email pattern returned by `account.sendVerifyEmailCode` (for email only).
    pub email_pattern: Option<String>,
    /// Optional Fragment SMS URL when `auth.sentCodeTypeFragmentSms` is returned.
    pub fragment_url: Option<String>,
    /// Optional countdown seconds until phone call resend is enabled.
    pub next_call_timeout_secs: Option<i32>,
}

/// Backend trait decoupling Telegram Passport UI from network and encryption storage.
pub trait PassportBackend: Send {
    /// Configures an explicit Passport [`FormRequest`] (typically parsed from a `tg://passport`
    /// deep link) before loading the authorization form.
    fn configure_form_request(&mut self, _request: FormRequest) {}

    /// Loads a Passport authorization form for `bot_username`.
    fn load_authorization_form(&self, bot_username: &str) -> Result<PassportSessionBundle, String>;

    /// Verifies the user's cloud password and unlocks the Passport master secret.
    fn verify_cloud_password(&mut self, password: &str) -> Result<PassportUnlockResult, String>;

    /// Creates a new cloud password and Passport master secret for first-time setup.
    fn setup_cloud_password(
        &mut self,
        password: &str,
        hint: &str,
    ) -> Result<PassportUnlockResult, String>;

    /// Encrypts and uploads a scan, selfie, or translation file for `value_type` / `file_type`.
    fn upload_passport_file(
        &mut self,
        _value_type: ValueType,
        _file_type: FileType,
        _raw_bytes: &[u8],
    ) -> Result<PassportFile, String> {
        Err("Passport file upload is not supported by this backend.".to_owned())
    }

    /// Saves edited fields and document scans for one Passport [`ValueType`].
    fn save_passport_value(
        &mut self,
        value_type: ValueType,
        fields: ValueMap,
        scan_labels: Vec<String>,
    ) -> Result<PassportForm, String>;

    /// Requests a verification code for a Passport phone number (`account.sendVerifyPhoneCode`)
    /// or email address (`account.sendVerifyEmailCode`).
    fn send_contact_verification_code(
        &mut self,
        _value_type: ValueType,
        _value: &str,
    ) -> Result<PassportVerificationChallenge, String> {
        Err("Contact verification is not supported by this backend.".to_owned())
    }

    /// Resends the Passport phone verification code via `auth.resendCode`.
    fn resend_phone_verification_code(
        &mut self,
        _phone: &str,
        _phone_code_hash: &str,
    ) -> Result<PassportVerificationChallenge, String> {
        Err("Phone code resend is not supported by this backend.".to_owned())
    }

    /// Submits a Passport phone (`account.verifyPhone`) or email (`account.verifyEmail`)
    /// verification code and saves the verified plain value on success.
    fn confirm_contact_verification_code(
        &mut self,
        _value_type: ValueType,
        _value: &str,
        _phone_code_hash: Option<&str>,
        _code: &str,
    ) -> Result<PassportForm, String> {
        Err("Contact verification confirmation is not supported by this backend.".to_owned())
    }

    /// Deletes one or more stored Passport values (`account.deleteSecureValue`).
    fn delete_passport_values(&mut self, _types: &[ValueType]) -> Result<PassportForm, String> {
        Err("Deleting Passport values is not supported by this backend.".to_owned())
    }

    /// Resets the Telegram Passport secret and wipes all stored Passport values
    /// (`FormController::resetSecret` + `account.deleteSecureValue`).
    fn reset_passport(&mut self, _password: &str) -> Result<PassportUnlockResult, String> {
        Err("Resetting Telegram Passport is not supported by this backend.".to_owned())
    }

    /// Loads or refreshes the country-to-preferred-language mapping via `help.getPassportConfig`
    /// and applies it to the active `PassportController` (`passport/passport_form_controller.cpp`).
    fn load_passport_config(&mut self) -> Result<PassportConfig, String> {
        Ok(PassportConfig::default())
    }

    /// Accepts the Passport authorization request (local mock only).
    fn accept_authorization(&mut self) -> Result<String, String>;

    /// Declines the Passport authorization request.
    fn decline_authorization(&mut self) -> Result<String, String>;
}
