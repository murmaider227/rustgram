//! Deterministic fake backends for Payments, Stars, Star Gifts, Premium, Boosts/Giveaways,
//! and Telegram Passport UI screens.
//!
//! Safety guarantees:
//! - Zero network calls; never contacts Telegram servers or any payment provider.
//! - Uses only fictional placeholder names and documented Stripe test card patterns
//!   (e.g. `4242 4242 4242 4242`).

use std::collections::BTreeMap;

use tg_data::{ChannelId, CollectibleId, DocumentId, FullMsgId, MsgId, PeerId, UserId};
use tg_passport::types::{
    FileType, PassportFile, PassportForm, PassportValue, ValueMap, ValueType,
};
use tg_payments::card::{
    NativeMethodDetails, UncheckedCardDetails, YearMonth, detect_card_brand, remove_non_digits,
    validate_card_details,
};
use tg_payments::credits::{
    CreditTopupOption, CreditsAmount, CreditsBalanceTracker, CreditsHistoryEntry,
    CreditsHistoryPeerType, PaidMediaCost, PaidReactionConfig, PeerSubscription, SubscriptionEntry,
};
use tg_payments::currency::fill_amount_and_currency;
use tg_payments::form::{
    Cover, FormDetails, Invoice, LabeledPrice, NativePaymentMethod, NewCredentials, PaymentForm,
    PaymentMethod, PostAddress, RequestedInformation, SavedCredentials, ShippingOption,
    ShippingOptions,
};
use tg_payments::premium::{
    Boost, BoostPrepaidGiveaway, BoostStatus, BoostsOverview, GiveawayInfo, GiveawayState,
    LevelLimits, MyBoostSlot, PremiumLimits, PremiumPromoData, PremiumSubscriptionOption,
    create_subscription_option,
};
use tg_payments::star_gifts::{
    RgbColor, SavedStarGift, SavedStarGiftId, StarGift, StarGiftBackground, UniqueGift,
    UniqueGiftAttribute, UniqueGiftBackdrop, UniqueGiftModel, UniqueGiftOriginalDetails,
    UniqueGiftPattern, UniqueGiftRarity,
};

use crate::payments::backend::{
    BoostsGiveawayBundle, CatalogGiftItem, CheckoutBundle, CreditsOverviewBundle, GiveawayInfoItem,
    PaidMediaPreview, PaidReactionContext, PaidReactorEntry, PassportBackend,
    PassportSessionBundle, PassportUnlockResult, PassportVerificationChallenge,
    PaymentSubmitOutcome, PaymentsBackend, PremiumFeatureItem, PremiumInvoicePreview,
    PremiumPromoBundle, ReceivedGiftAction, ReceivedGiftItem, StarGiftsBundle, ValidatedInfoResult,
};

/// Default sample invoice identifier served by [`MockPaymentsBackend`].
pub const SAMPLE_INVOICE_ID: &str = "inv_synth_pack_pro_001";
/// Default sample bot username served by [`MockPassportBackend`].
pub const SAMPLE_PASSPORT_BOT: &str = "vericheck_demo_bot";
/// Demo cloud password accepted by both mock backends for local testing.
pub const DEMO_CLOUD_PASSWORD: &str = "demo1234";

/// In-memory fake implementation of [`PaymentsBackend`].
pub struct MockPaymentsBackend {
    credits: CreditsBalanceTracker,
    paid_media_unlocked: bool,
    history: Vec<CreditsHistoryEntry>,
    history_peer_names: Vec<(String, String)>,
    subscriptions: Vec<SubscriptionEntry>,
    received_gifts: Vec<ReceivedGiftItem>,
    my_boost_slots: Vec<MyBoostSlot>,
    channel_boost_count: i32,
    saved_info: RequestedInformation,
    next_tx_seq: u64,
}

impl Default for MockPaymentsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPaymentsBackend {
    /// Creates a new deterministic [`MockPaymentsBackend`] with rich fictional test data.
    #[must_use]
    pub fn new() -> Self {
        let mut credits = CreditsBalanceTracker::new();
        credits.apply_balance(CreditsAmount::stars(1_450));
        credits.apply_balance(CreditsAmount::new(
            12,
            450_000_000,
            tg_payments::credits::CreditsType::Ton,
        ));

        let saved_info = RequestedInformation {
            default_phone: "+1 555 010 2468".to_owned(),
            default_country: "US".to_owned(),
            save: true,
            name: "Alex River".to_owned(),
            phone: "+1 555 010 2468".to_owned(),
            email: "alex.river@example.test".to_owned(),
            shipping_address: PostAddress {
                address1: "742 Evergreen Terrace".to_owned(),
                address2: "Suite 4B".to_owned(),
                city: "Springfield".to_owned(),
                state: "OR".to_owned(),
                country_iso2: "US".to_owned(),
                postcode: "97477".to_owned(),
            },
        };

        let history = vec![
            CreditsHistoryEntry {
                id: "tx_stars_101".to_owned(),
                title: "Stars Top-Up".to_owned(),
                description: "1,000 Telegram Stars pack".to_owned(),
                date: 1_763_650_800,
                credits: CreditsAmount::stars(1_000),
                peer_type: CreditsHistoryPeerType::Fragment,
                incoming: true,
                ..CreditsHistoryEntry::default()
            },
            CreditsHistoryEntry {
                id: "tx_stars_102".to_owned(),
                title: "Paid Media Unlock".to_owned(),
                description: "Acoustic Studio Session (4 photos, 1 video)".to_owned(),
                date: 1_763_614_200,
                credits: CreditsAmount::stars(-120),
                peer_id: PeerId::from_channel(ChannelId::from_tl(2_000_101)),
                peer_type: CreditsHistoryPeerType::Peer,
                incoming: false,
                ..CreditsHistoryEntry::default()
            },
            CreditsHistoryEntry {
                id: "tx_stars_103".to_owned(),
                title: "Star Gift Sent".to_owned(),
                description: "Crystal Prism gift to Sam Taylor".to_owned(),
                date: 1_763_540_000,
                credits: CreditsAmount::stars(-50),
                peer_id: PeerId::from_user(UserId::from_tl(900_002)),
                peer_type: CreditsHistoryPeerType::Peer,
                stargift: true,
                gift: true,
                incoming: false,
                ..CreditsHistoryEntry::default()
            },
            CreditsHistoryEntry {
                id: "tx_stars_104".to_owned(),
                title: "Paid Reaction".to_owned(),
                description: "Star reaction on Design Notes #418".to_owned(),
                date: 1_763_482_400,
                credits: CreditsAmount::stars(-25),
                peer_id: PeerId::from_channel(ChannelId::from_tl(2_000_202)),
                peer_type: CreditsHistoryPeerType::Peer,
                bare_msg_id: 418,
                reaction: true,
                incoming: false,
                ..CreditsHistoryEntry::default()
            },
            CreditsHistoryEntry {
                id: "tx_stars_105".to_owned(),
                title: "Gift Converted to Stars".to_owned(),
                description: "Converted Birthday Cupcake into Stars".to_owned(),
                date: 1_763_390_000,
                credits: CreditsAmount::stars(85),
                peer_id: PeerId::from_user(UserId::from_tl(900_003)),
                peer_type: CreditsHistoryPeerType::Peer,
                stargift: true,
                converted: true,
                stars_converted: 85,
                incoming: true,
                ..CreditsHistoryEntry::default()
            },
            CreditsHistoryEntry {
                id: "tx_stars_106".to_owned(),
                title: "Monthly Channel Subscription".to_owned(),
                description: "Rust Audio Builders Pro".to_owned(),
                date: 1_763_210_000,
                credits: CreditsAmount::stars(-150),
                peer_id: PeerId::from_channel(ChannelId::from_tl(2_000_303)),
                peer_type: CreditsHistoryPeerType::Peer,
                subscription_period: 30 * 86_400,
                incoming: false,
                ..CreditsHistoryEntry::default()
            },
            CreditsHistoryEntry {
                id: "tx_stars_107".to_owned(),
                title: "Bot Purchase Refund".to_owned(),
                description: "Refunded order #8821".to_owned(),
                date: 1_763_100_000,
                credits: CreditsAmount::stars(200),
                peer_id: PeerId::from_user(UserId::from_tl(900_099)),
                peer_type: CreditsHistoryPeerType::Peer,
                refunded: true,
                incoming: true,
                ..CreditsHistoryEntry::default()
            },
        ];

        let history_peer_names = vec![
            ("tx_stars_101".to_owned(), "Fragment".to_owned()),
            ("tx_stars_102".to_owned(), "Northwind Sound Lab".to_owned()),
            ("tx_stars_103".to_owned(), "Sam Taylor".to_owned()),
            ("tx_stars_104".to_owned(), "Design Notes Channel".to_owned()),
            ("tx_stars_105".to_owned(), "Casey Morgan".to_owned()),
            ("tx_stars_106".to_owned(), "Rust Audio Builders".to_owned()),
            ("tx_stars_107".to_owned(), "Demo Shop Bot".to_owned()),
        ];

        let subscriptions = vec![
            SubscriptionEntry {
                id: "sub_01".to_owned(),
                title: "Rust Audio Builders Pro".to_owned(),
                until: 1_765_802_000,
                subscription: PeerSubscription {
                    credits: 150,
                    period: 30 * 86_400,
                },
                peer_id: PeerId::from_channel(ChannelId::from_tl(2_000_303)),
                cancelled: false,
                cancelled_by_bot: false,
                expired: false,
                can_refulfill: false,
                ..SubscriptionEntry::default()
            },
            SubscriptionEntry {
                id: "sub_02".to_owned(),
                title: "Pixel Art Weekly Club".to_owned(),
                until: 1_764_950_000,
                subscription: PeerSubscription {
                    credits: 75,
                    period: 30 * 86_400,
                },
                peer_id: PeerId::from_channel(ChannelId::from_tl(2_000_404)),
                cancelled: true,
                cancelled_by_bot: false,
                expired: false,
                can_refulfill: false,
                ..SubscriptionEntry::default()
            },
        ];

        let received_gifts = build_sample_received_gifts();
        let my_boost_slots = vec![
            MyBoostSlot {
                slot: 1,
                peer_id: Some(PeerId::from_channel(ChannelId::from_tl(2_000_101))),
                date: 1_761_000_000,
                expires: 1_776_552_000,
                cooldown_until_date: 1_761_086_400,
            },
            MyBoostSlot {
                slot: 2,
                peer_id: Some(PeerId::from_channel(ChannelId::from_tl(2_000_101))),
                date: 1_763_740_000,
                expires: 1_779_292_000,
                cooldown_until_date: 1_763_826_400,
            },
            MyBoostSlot {
                slot: 3,
                peer_id: None,
                date: 0,
                expires: 0,
                cooldown_until_date: 0,
            },
        ];

        Self {
            credits,
            paid_media_unlocked: false,
            history,
            history_peer_names,
            subscriptions,
            received_gifts,
            my_boost_slots,
            channel_boost_count: 58,
            saved_info,
            next_tx_seq: 200,
        }
    }

    fn sample_shipping_options() -> Vec<ShippingOption> {
        vec![
            ShippingOption {
                id: "ship_standard".to_owned(),
                title: "Standard Courier (3-5 days)".to_owned(),
                prices: vec![LabeledPrice {
                    label: "Standard Delivery".to_owned(),
                    price: 599,
                }],
            },
            ShippingOption {
                id: "ship_express".to_owned(),
                title: "Express Air (1-2 days)".to_owned(),
                prices: vec![LabeledPrice {
                    label: "Express Shipping".to_owned(),
                    price: 1499,
                }],
            },
            ShippingOption {
                id: "ship_pickup".to_owned(),
                title: "Local Studio Pickup".to_owned(),
                prices: vec![LabeledPrice {
                    label: "Pickup".to_owned(),
                    price: 0,
                }],
            },
        ]
    }
}

impl PaymentsBackend for MockPaymentsBackend {
    fn load_checkout_form(&self, invoice_id: &str) -> Result<CheckoutBundle, String> {
        let mut form = PaymentForm::new("+1 555 010 2468", "US");
        form.invoice = Invoice {
            cover: Cover {
                title: "Synth Pack Pro + Hardware Dongle".to_owned(),
                description:
                    "48 studio analog presets, lossless WAV stems, and anodized USB-C license key."
                        .to_owned(),
                seller: "Northwind Audio Bot".to_owned(),
            },
            prices: vec![
                LabeledPrice {
                    label: "Synth Pack Pro License".to_owned(),
                    price: 2900,
                },
                LabeledPrice {
                    label: "USB-C Hardware Key".to_owned(),
                    price: 1200,
                },
                LabeledPrice {
                    label: "Early-Bird Bundle Discount".to_owned(),
                    price: -400,
                },
            ],
            suggested_tips: vec![100, 300, 500, 1000],
            tips_max: 2500,
            tips_selected: 300,
            currency: "USD".to_owned(),
            is_name_requested: true,
            is_phone_requested: true,
            is_email_requested: true,
            is_shipping_address_requested: true,
            is_recurring: false,
            is_flexible: true,
            is_test: true,
            provider: "Stripe (Test Mode)".to_owned(),
            terms_url: "https://example.test/northwind/terms".to_owned(),
            phone_sent_to_provider: true,
            email_sent_to_provider: true,
            ..Invoice::default()
        };
        form.details = FormDetails {
            form_id: 901_004_200,
            url: "https://example.test/payments/checkout-demo".to_owned(),
            native_provider: "stripe".to_owned(),
            terms_bot_username: "northwind_audio_bot".to_owned(),
            native_params_json:
                r#"{"publishable_key":"pk_test_placeholder","need_cardholder_name":true,"need_country":true,"need_zip":true}"#
                    .to_owned(),
            bot_id: UserId::from_tl(910_001),
            provider_id: UserId::from_tl(910_002),
            can_save_credentials: true,
            password_missing: false,
            terms_accepted: false,
        };
        form.saved_information = self.saved_info.clone();
        form.information = self.saved_info.clone();
        form.requested_information_id = "req_info_demo_01".to_owned();
        form.shipping_options = ShippingOptions {
            currency: "USD".to_owned(),
            list: Self::sample_shipping_options(),
            selected_id: "ship_standard".to_owned(),
        };
        form.payment_method = PaymentMethod {
            native: NativePaymentMethod::Stripe {
                publishable_key: "pk_test_placeholder".to_owned(),
            },
            native_details: NativeMethodDetails {
                default_country: "US".to_owned(),
                supported: true,
                need_country: true,
                need_zip: true,
                need_cardholder_name: true,
                can_save_information: true,
            },
            saved_credentials: vec![
                SavedCredentials {
                    id: "saved_test_visa_4242".to_owned(),
                    title: "Visa *4242".to_owned(),
                },
                SavedCredentials {
                    id: "saved_test_mc_4444".to_owned(),
                    title: "MasterCard *4444".to_owned(),
                },
            ],
            saved_credentials_index: 0,
            new_credentials: NewCredentials::default(),
            additional_methods: Vec::new(),
            url: "https://example.test/payments/checkout-demo".to_owned(),
        };

        Ok(CheckoutBundle {
            invoice_id: invoice_id.to_owned(),
            form,
            cover_badge: "TEST".to_owned(),
            requires_3ds_demo: true,
            verification_url: "https://verify.example.test/3ds/challenge?session=demo_local_only"
                .to_owned(),
        })
    }

    fn validate_requested_info(
        &mut self,
        _invoice_id: &str,
        info: &RequestedInformation,
    ) -> Result<ValidatedInfoResult, String> {
        if info.shipping_address.address1.trim().is_empty() {
            return Err("ADDRESS_STREET_LINE1_INVALID".to_owned());
        }
        if info.shipping_address.city.trim().len() < 2 {
            return Err("ADDRESS_CITY_INVALID".to_owned());
        }
        if info.shipping_address.country_iso2.trim().len() != 2 {
            return Err("ADDRESS_COUNTRY_INVALID".to_owned());
        }
        if info.name.trim().is_empty() {
            return Err("REQ_INFO_NAME_INVALID".to_owned());
        }
        if !info.email.contains('@') || !info.email.contains('.') {
            return Err("REQ_INFO_EMAIL_INVALID".to_owned());
        }
        if info.phone.trim().len() < 5 {
            return Err("REQ_INFO_PHONE_INVALID".to_owned());
        }
        if info.save {
            self.saved_info = info.clone();
        }
        Ok(ValidatedInfoResult {
            requested_info_id: "req_info_validated_02".to_owned(),
            shipping_options: Self::sample_shipping_options(),
        })
    }

    fn tokenize_test_card(
        &self,
        details: &UncheckedCardDetails,
        save_on_server: bool,
    ) -> Result<NewCredentials, String> {
        let native = NativeMethodDetails {
            default_country: "US".to_owned(),
            supported: true,
            need_country: true,
            need_zip: true,
            need_cardholder_name: true,
            can_save_information: true,
        };
        validate_card_details(details, &native, YearMonth::new(2026, 11)).map_err(str::to_owned)?;
        let digits = remove_non_digits(&details.number);
        let last4 = digits
            .get(digits.len().saturating_sub(4)..)
            .unwrap_or("0000");
        let brand = detect_card_brand(&digits);
        let title = format!("{brand} *{last4}");
        let data = format!(r#"{{"type":"mock_test_token","brand":"{brand}","last4":"{last4}"}}"#);
        Ok(NewCredentials {
            title,
            data,
            save_on_server,
        })
    }

    fn verify_tmp_password(&self, password: &str) -> Result<Vec<u8>, String> {
        if password == DEMO_CLOUD_PASSWORD {
            Ok(b"mock_tmp_password_token".to_vec())
        } else {
            Err(format!(
                "Incorrect password (use \"{DEMO_CLOUD_PASSWORD}\" in offline demo mode)."
            ))
        }
    }

    fn submit_payment_form(
        &mut self,
        _invoice_id: &str,
        _form_id: i64,
        shipping_option_id: Option<&str>,
        tip_amount: i64,
        _credentials_title: &str,
        require_3ds: bool,
    ) -> Result<PaymentSubmitOutcome, String> {
        if require_3ds {
            return Ok(PaymentSubmitOutcome::VerificationNeeded {
                url: "https://verify.example.test/3ds/challenge?session=demo_local_only".to_owned(),
                provider: "Stripe (Test Mode)".to_owned(),
            });
        }
        let ship_cost = shipping_option_id
            .and_then(|id| {
                Self::sample_shipping_options()
                    .into_iter()
                    .find(|s| s.id == id)
            })
            .map(|s| s.prices.iter().map(|p| p.price).sum::<i64>())
            .unwrap_or(0);
        let total = 2900 + 1200 - 400 + ship_cost + tip_amount.max(0);
        Ok(PaymentSubmitOutcome::Paid {
            date: 1_763_760_000,
            total_amount: total,
            currency: "USD".to_owned(),
            receipt_id: "TEST-RCPT-2026-042".to_owned(),
        })
    }

    fn load_credits_overview(&self) -> Result<CreditsOverviewBundle, String> {
        let topup_options = vec![
            CreditTopupOption {
                credits: 100,
                product: "stars_100".to_owned(),
                currency: "USD".to_owned(),
                amount: 199,
                extended: false,
                gift_bare_peer_id: 0,
            },
            CreditTopupOption {
                credits: 250,
                product: "stars_250".to_owned(),
                currency: "USD".to_owned(),
                amount: 499,
                extended: false,
                gift_bare_peer_id: 0,
            },
            CreditTopupOption {
                credits: 500,
                product: "stars_500".to_owned(),
                currency: "USD".to_owned(),
                amount: 999,
                extended: false,
                gift_bare_peer_id: 0,
            },
            CreditTopupOption {
                credits: 1_000,
                product: "stars_1000".to_owned(),
                currency: "USD".to_owned(),
                amount: 1999,
                extended: true,
                gift_bare_peer_id: 0,
            },
            CreditTopupOption {
                credits: 2_500,
                product: "stars_2500".to_owned(),
                currency: "USD".to_owned(),
                amount: 4999,
                extended: true,
                gift_bare_peer_id: 0,
            },
        ];

        Ok(CreditsOverviewBundle {
            balance: self.credits.available_balance(),
            ton_balance: self.credits.ton_balance(),
            topup_options,
            history: self.history.clone(),
            history_peer_names: self.history_peer_names.clone(),
            subscriptions: self.subscriptions.clone(),
            sample_paid_media: PaidMediaPreview {
                media_id: 77_001,
                author_title: "Northwind Sound Lab".to_owned(),
                caption: "Behind-the-scenes modular synth patch notes & lossless demo video."
                    .to_owned(),
                photo_count: 4,
                video_count: 1,
                cost: PaidMediaCost {
                    stars_amount: 75,
                    media_count: 5,
                    unlocked: self.paid_media_unlocked,
                },
            },
            sample_paid_reaction: PaidReactionContext {
                post_id: 512,
                channel_title: "Design Notes Channel".to_owned(),
                post_excerpt: "New vector icon pipeline and GPU shader benchmarks are live!"
                    .to_owned(),
                config: PaidReactionConfig::new(2_500),
                top_reactors: vec![
                    PaidReactorEntry {
                        name: "Sam Taylor".to_owned(),
                        initials: "ST".to_owned(),
                        stars: 250,
                        is_self: false,
                    },
                    PaidReactorEntry {
                        name: "Casey Morgan".to_owned(),
                        initials: "CM".to_owned(),
                        stars: 140,
                        is_self: false,
                    },
                    PaidReactorEntry {
                        name: "Alex River".to_owned(),
                        initials: "AR".to_owned(),
                        stars: 50,
                        is_self: true,
                    },
                ],
            },
        })
    }

    fn topup_stars(&mut self, option: &CreditTopupOption) -> Result<String, String> {
        let price = fill_amount_and_currency(option.amount as i64, &option.currency, false);
        Ok(format!(
            "Stars top-up form preview ready: {} Stars ({price}). Review in live checkout to purchase.",
            option.credits
        ))
    }

    fn change_stars_subscription(
        &mut self,
        subscription_id: &str,
        canceled: bool,
    ) -> Result<String, String> {
        let Some(sub) = self
            .subscriptions
            .iter_mut()
            .find(|s| s.id == subscription_id)
        else {
            return Err(format!("Subscription '{subscription_id}' not found."));
        };
        sub.cancelled = canceled;
        if canceled {
            Ok(format!("Canceled subscription to {}.", sub.title))
        } else {
            Ok(format!("Renewed subscription to {}.", sub.title))
        }
    }

    fn confirm_paid_media(&mut self, _media_id: u64, stars: i64) -> Result<CreditsAmount, String> {
        let cost = CreditsAmount::stars(stars);
        if !self.credits.lock(cost) {
            return Err("Insufficient Stars balance.".to_owned());
        }
        self.credits.withdraw_locked(cost);
        self.paid_media_unlocked = true;
        self.next_tx_seq += 1;
        let id = format!("tx_stars_{}", self.next_tx_seq);
        self.history.insert(
            0,
            CreditsHistoryEntry {
                id: id.clone(),
                title: "Paid Media Unlocked".to_owned(),
                description: "Northwind Sound Lab (4 photos, 1 video)".to_owned(),
                date: 1_763_761_200,
                credits: CreditsAmount::stars(-stars),
                peer_type: CreditsHistoryPeerType::Peer,
                incoming: false,
                ..CreditsHistoryEntry::default()
            },
        );
        self.history_peer_names
            .push((id, "Northwind Sound Lab".to_owned()));
        Ok(self.credits.available_balance())
    }

    fn confirm_paid_reaction(
        &mut self,
        post_id: i32,
        stars: i32,
        anonymous: bool,
    ) -> Result<CreditsAmount, String> {
        let cost = CreditsAmount::stars(i64::from(stars));
        if !self.credits.lock(cost) {
            return Err("Insufficient Stars balance.".to_owned());
        }
        self.credits.withdraw_locked(cost);
        self.next_tx_seq += 1;
        let id = format!("tx_stars_{}", self.next_tx_seq);
        self.history.insert(
            0,
            CreditsHistoryEntry {
                id: id.clone(),
                title: "Paid Reaction".to_owned(),
                description: if anonymous {
                    format!("Anonymous reaction on post #{post_id}")
                } else {
                    format!("Star reaction on Design Notes #{post_id}")
                },
                date: 1_763_761_500,
                credits: CreditsAmount::stars(-i64::from(stars)),
                bare_msg_id: post_id as u64,
                reaction: true,
                incoming: false,
                ..CreditsHistoryEntry::default()
            },
        );
        self.history_peer_names
            .push((id, "Design Notes Channel".to_owned()));
        Ok(self.credits.available_balance())
    }

    fn load_star_gifts(&self) -> Result<StarGiftsBundle, String> {
        Ok(StarGiftsBundle {
            balance: self.credits.available_balance(),
            default_recipient: "Sam Taylor".to_owned(),
            recipient_peer_id: 1_000_007,
            recipient_is_self: false,
            catalog: build_sample_catalog_gifts(),
            received: self.received_gifts.clone(),
        })
    }

    fn send_star_gift(
        &mut self,
        gift_id: u64,
        recipient: &str,
        message: &str,
        hide_name: bool,
        include_upgrade: bool,
    ) -> Result<String, String> {
        let upgrade_note = if include_upgrade {
            " + upgrade pre-paid"
        } else {
            ""
        };
        let anon_note = if hide_name { " (anonymous)" } else { "" };
        let msg_note = if message.trim().is_empty() {
            String::new()
        } else {
            format!(" with note \"{}\"", message.trim())
        };
        Ok(format!(
            "Placeholder: Gift #{gift_id} queued for {recipient}{anon_note}{upgrade_note}{msg_note} (offline demo)."
        ))
    }

    fn apply_received_gift_action(
        &mut self,
        gift_id: u64,
        action: ReceivedGiftAction,
    ) -> Result<String, String> {
        if let Some(item) = self
            .received_gifts
            .iter_mut()
            .find(|g| g.saved.info.id == gift_id)
        {
            match action {
                ReceivedGiftAction::TogglePin => {
                    item.saved.pinned = !item.saved.pinned;
                    return Ok(if item.saved.pinned {
                        format!("\"{}\" pinned to profile (demo).", item.title)
                    } else {
                        format!("\"{}\" unpinned from profile (demo).", item.title)
                    });
                }
                ReceivedGiftAction::ToggleVisibility => {
                    item.saved.hidden = !item.saved.hidden;
                    return Ok(if item.saved.hidden {
                        format!("\"{}\" hidden from profile (demo).", item.title)
                    } else {
                        format!("\"{}\" shown on profile (demo).", item.title)
                    });
                }
                ReceivedGiftAction::Upgrade => {
                    return Ok(format!(
                        "Placeholder: Upgrade \"{}\" to unique collectible is disabled in demo mode.",
                        item.title
                    ));
                }
                ReceivedGiftAction::ConvertToStars => {
                    return Ok(format!(
                        "Placeholder: Convert \"{}\" for {} Stars is disabled in demo mode.",
                        item.title, item.saved.stars_converted
                    ));
                }
                ReceivedGiftAction::Transfer => {
                    return Ok(format!(
                        "Placeholder: Transfer \"{}\" to another user is disabled in demo mode.",
                        item.title
                    ));
                }
            }
        }
        Err(format!("Gift #{gift_id} not found."))
    }

    fn transfer_received_gift(&mut self, gift_id: u64, recipient: &str) -> Result<String, String> {
        let cleaned = recipient.trim();
        if cleaned.is_empty() {
            return Err("Recipient is required to transfer a Star Gift.".to_owned());
        }
        if let Some(item) = self
            .received_gifts
            .iter()
            .find(|g| g.saved.info.id == gift_id)
        {
            return Ok(format!(
                "Placeholder: Transfer \"{}\" to {cleaned} is disabled in demo mode.",
                item.title
            ));
        }
        Err(format!("Gift #{gift_id} not found."))
    }

    fn transfer_recipient_candidates(&self) -> Vec<String> {
        vec![
            "Sam Taylor".to_owned(),
            "Casey Morgan".to_owned(),
            "Jordan Blake".to_owned(),
        ]
    }

    fn load_premium_promo(&self) -> Result<PremiumPromoBundle, String> {
        let monthly_base = 499.0_f64;
        let opt_12 = create_subscription_option(
            12,
            monthly_base,
            3_999,
            "USD",
            "https://t.me/PremiumBot?start=demo_12",
        );
        let opt_6 = create_subscription_option(
            6,
            monthly_base,
            2_399,
            "USD",
            "https://t.me/PremiumBot?start=demo_6",
        );
        let opt_1 = create_subscription_option(
            1,
            monthly_base,
            499,
            "USD",
            "https://t.me/PremiumBot?start=demo_1",
        );

        let default_limits = PremiumLimits::new(false);
        let premium_limits = PremiumLimits::new(true);

        let promo = PremiumPromoData {
            status_text: "Go beyond the limits and unlock dozens of exclusive features.".to_owned(),
            videos: BTreeMap::new(),
            period_options: vec![opt_12, opt_6, opt_1],
            monthly_amount: 499,
            monthly_currency: "USD".to_owned(),
        };

        let features = vec![
            PremiumFeatureItem {
                slug: "double_limits".to_owned(),
                title: "Doubled Limits".to_owned(),
                description: format!(
                    "Up to {} channels, {} folders, {} pinned chats, and {} bio characters.",
                    premium_limits.channels(),
                    premium_limits.dialog_filters(),
                    premium_limits.dialogs_pinned(),
                    premium_limits.about_length(),
                ),
                detail: format!(
                    "Channels: {} -> {}  |  Folders: {} -> {}  |  Pinned: {} -> {}  |  GIFs: {} -> {}",
                    default_limits.channels(),
                    premium_limits.channels(),
                    default_limits.dialog_filters(),
                    premium_limits.dialog_filters(),
                    default_limits.dialogs_pinned(),
                    premium_limits.dialogs_pinned(),
                    default_limits.saved_gifs(),
                    premium_limits.saved_gifs(),
                ),
                icon_rgb: [0x6A, 0x7A, 0xFF],
                icon_symbol: "2x".to_owned(),
                is_new: false,
            },
            PremiumFeatureItem {
                slug: "more_upload".to_owned(),
                title: "4 GB File Uploads".to_owned(),
                description: "Upload documents and media up to 4 GB each instead of 2 GB."
                    .to_owned(),
                detail: format!(
                    "Max upload parts: {} (~2 GB) -> {} (~4 GB).",
                    default_limits.upload_max_fileparts(),
                    premium_limits.upload_max_fileparts(),
                ),
                icon_rgb: [0x4D, 0x9F, 0xFF],
                icon_symbol: "4G".to_owned(),
                is_new: false,
            },
            PremiumFeatureItem {
                slug: "faster_download".to_owned(),
                title: "Faster Download Speed".to_owned(),
                description: "Download media and files at maximum network throughput.".to_owned(),
                detail: "Removes standard per-connection media download throttling.".to_owned(),
                icon_rgb: [0x9B, 0x5C, 0xF6],
                icon_symbol: ">>".to_owned(),
                is_new: false,
            },
            PremiumFeatureItem {
                slug: "voice_to_text".to_owned(),
                title: "Voice-to-Text Conversion".to_owned(),
                description: "Instant transcripts for voice and round video messages.".to_owned(),
                detail: "Unlimited automatic speech-to-text across all chats and channels."
                    .to_owned(),
                icon_rgb: [0xE2, 0x55, 0x9C],
                icon_symbol: "Aa".to_owned(),
                is_new: false,
            },
            PremiumFeatureItem {
                slug: "no_ads".to_owned(),
                title: "No Sponsored Messages".to_owned(),
                description: "Hide promotional messages in large public channels.".to_owned(),
                detail: "Clean reading flow across every public broadcast channel.".to_owned(),
                icon_rgb: [0xF2, 0x6B, 0x4E],
                icon_symbol: "AD".to_owned(),
                is_new: false,
            },
            PremiumFeatureItem {
                slug: "translations".to_owned(),
                title: "Real-Time Chat Translation".to_owned(),
                description: "Translate entire chats and channels on the fly.".to_owned(),
                detail: "Inline translation bar at the top of foreign-language chats.".to_owned(),
                icon_rgb: [0x2D, 0xB8, 0x9A],
                icon_symbol: "Tr".to_owned(),
                is_new: true,
            },
            PremiumFeatureItem {
                slug: "emoji_status".to_owned(),
                title: "Animated Emoji & Profile Status".to_owned(),
                description: "Use custom animated emoji anywhere and set a live status icon."
                    .to_owned(),
                detail: "Includes custom profile accent colors and animated reactions.".to_owned(),
                icon_rgb: [0xF5, 0xA6, 0x23],
                icon_symbol: "*".to_owned(),
                is_new: true,
            },
        ];

        Ok(PremiumPromoBundle {
            promo,
            default_limits,
            premium_limits,
            features,
        })
    }

    fn preview_premium_invoice(
        &self,
        option: &PremiumSubscriptionOption,
    ) -> Result<PremiumInvoicePreview, String> {
        Ok(PremiumInvoicePreview {
            form_id: 901_000_000 + i64::from(option.months),
            title: format!("Telegram Premium ({} months)", option.months),
            description: format!(
                "{} total ({}/month). Read-only invoice preview. No charge is made until you confirm checkout.",
                option.total, option.cost_per_month
            ),
            total_label: option.total.clone(),
            months: option.months,
            provider: "PremiumBot (Test Preview)".to_owned(),
            checkout_url: option.bot_url.clone(),
            is_test: true,
        })
    }

    fn load_boosts_giveaway(&self) -> Result<BoostsGiveawayBundle, String> {
        let target_channel = PeerId::from_channel(ChannelId::from_tl(2_000_101));
        let my_slot_ids: Vec<i32> = self
            .my_boost_slots
            .iter()
            .filter(|s| s.peer_id == Some(target_channel))
            .map(|s| s.slot)
            .collect();
        let boost_status = BoostStatus {
            overview: BoostsOverview {
                is_group: false,
                mine: my_slot_ids.len() as i32,
                level: 7,
                boost_count: self.channel_boost_count,
                current_level_boost_count: 50,
                next_level_boost_count: 65,
                premium_member_count: 420,
                premium_member_percentage: 14.8,
            },
            prepaid_giveaways: vec![
                BoostPrepaidGiveaway {
                    date: 1_763_600_000,
                    id: 501,
                    credits: 0,
                    months: 3,
                    quantity: 10,
                    boosts: 40,
                },
                BoostPrepaidGiveaway {
                    date: 1_763_700_000,
                    id: 502,
                    credits: 2_500,
                    months: 0,
                    quantity: 5,
                    boosts: 10,
                },
            ],
            link: "https://t.me/boost/northwind_sound_lab".to_owned(),
            my_boost_slots: my_slot_ids,
        };

        let my_slots = self.my_boost_slots.clone();

        let recent_boosts = vec![
            (
                Boost {
                    id: "boost_a".to_owned(),
                    user_id: UserId::from_tl(900_002),
                    giveaway_message: FullMsgId::ZERO,
                    date: 1_763_690_000,
                    expires_at: 1_771_466_000,
                    expires_after_months: 3,
                    gift_code_link: None,
                    multiplier: 1,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: false,
                    is_unclaimed: false,
                },
                "Sam Taylor".to_owned(),
            ),
            (
                Boost {
                    id: "boost_b".to_owned(),
                    user_id: UserId::from_tl(900_003),
                    giveaway_message: FullMsgId::new(
                        PeerId::from_channel(ChannelId::from_tl(2_000_101)),
                        MsgId::from(880),
                    ),
                    date: 1_763_500_000,
                    expires_at: 1_771_276_000,
                    expires_after_months: 3,
                    gift_code_link: None,
                    multiplier: 4,
                    credits: 0,
                    is_gift: false,
                    is_giveaway: true,
                    is_unclaimed: false,
                },
                "Casey Morgan (Giveaway)".to_owned(),
            ),
        ];

        let giveaways = vec![
            GiveawayInfoItem {
                title: "Autumn Community Giveaway".to_owned(),
                channel_title: "Northwind Sound Lab".to_owned(),
                additional_channels: vec!["Design Notes Channel".to_owned()],
                prize_summary: "10 x Telegram Premium (3 months)".to_owned(),
                prize_description: "Plus early access to Synth Pack Pro stems".to_owned(),
                only_new_subscribers: false,
                countries_summary: "Worldwide".to_owned(),
                info: GiveawayInfo {
                    state: GiveawayState::Running,
                    start_date: 1_763_500_000,
                    finish_date: 1_764_104_800,
                    winners_count: 10,
                    participating: true,
                    ..GiveawayInfo::default()
                },
            },
            GiveawayInfoItem {
                title: "Milestone Stars Giveaway".to_owned(),
                channel_title: "Design Notes Channel".to_owned(),
                additional_channels: Vec::new(),
                prize_summary: "2,500 Telegram Stars split among 5 winners".to_owned(),
                prize_description: String::new(),
                only_new_subscribers: false,
                countries_summary: "Worldwide".to_owned(),
                info: GiveawayInfo {
                    state: GiveawayState::Finished,
                    gift_code: "DEMO-WIN-2026".to_owned(),
                    start_date: 1_762_000_000,
                    finish_date: 1_762_604_800,
                    credits: 500,
                    winners_count: 5,
                    activated_count: 4,
                    participating: true,
                    ..GiveawayInfo::default()
                },
            },
        ];

        Ok(BoostsGiveawayBundle {
            channel_title: "Northwind Sound Lab".to_owned(),
            boost_status,
            level_limits: LevelLimits::default(),
            my_slots,
            recent_boosts,
            giveaways,
        })
    }

    fn apply_channel_boost(
        &mut self,
        _channel_title: &str,
        slots: &[i32],
    ) -> Result<BoostsGiveawayBundle, String> {
        if slots.is_empty() {
            return Err("Select at least one boost slot to apply.".to_owned());
        }
        let target = PeerId::from_channel(ChannelId::from_tl(2_000_101));
        let mut applied = 0i32;
        for &slot_id in slots {
            if let Some(slot) = self.my_boost_slots.iter_mut().find(|s| s.slot == slot_id)
                && slot.peer_id != Some(target)
            {
                slot.peer_id = Some(target);
                slot.date = 1_763_760_000;
                slot.expires = 1_779_312_000;
                slot.cooldown_until_date = 1_763_846_400;
                applied += 1;
            }
        }
        if applied == 0 {
            return Err("Selected boost slot is already applied to this channel.".to_owned());
        }
        self.channel_boost_count += applied;
        self.load_boosts_giveaway()
    }

    fn as_marketplace_backend(
        &mut self,
    ) -> Option<&mut dyn crate::payments::marketplace_backend::StarGiftsMarketplaceBackend> {
        Some(self)
    }
}

fn build_sample_catalog_gifts() -> Vec<CatalogGiftItem> {
    vec![
        CatalogGiftItem {
            gift: StarGift {
                id: 1001,
                stars: 25,
                stars_converted: 21,
                stars_to_upgrade: 25,
                sticker_document_id: DocumentId::from_tl(1001),
                upgradable: true,
                birthday: true,
                background: Some(StarGiftBackground {
                    center: RgbColor::new(0xFF, 0x9E, 0x5E),
                    edge: RgbColor::new(0xE0, 0x5E, 0x4A),
                    text: RgbColor::new(0xFF, 0xFF, 0xFF),
                }),
                ..StarGift::default()
            },
            title: "Starlight Cake".to_owned(),
            artwork_kind: "cake".to_owned(),
        },
        CatalogGiftItem {
            gift: StarGift {
                id: 1002,
                stars: 50,
                stars_converted: 43,
                stars_to_upgrade: 25,
                sticker_document_id: DocumentId::from_tl(1002),
                limited_count: 500,
                limited_left: 142,
                upgradable: true,
                background: Some(StarGiftBackground {
                    center: RgbColor::new(0x5A, 0xC8, 0xFA),
                    edge: RgbColor::new(0x2F, 0x79, 0xD8),
                    text: RgbColor::new(0xFF, 0xFF, 0xFF),
                }),
                ..StarGift::default()
            },
            title: "Crystal Prism".to_owned(),
            artwork_kind: "gem".to_owned(),
        },
        CatalogGiftItem {
            gift: StarGift {
                id: 1003,
                stars: 100,
                stars_converted: 85,
                stars_to_upgrade: 50,
                sticker_document_id: DocumentId::from_tl(1003),
                limited_count: 250,
                limited_left: 38,
                upgradable: true,
                require_premium: true,
                background: Some(StarGiftBackground {
                    center: RgbColor::new(0xB3, 0x6E, 0xF7),
                    edge: RgbColor::new(0x6C, 0x3B, 0xC9),
                    text: RgbColor::new(0xFF, 0xFF, 0xFF),
                }),
                ..StarGift::default()
            },
            title: "Astral Crown".to_owned(),
            artwork_kind: "crown".to_owned(),
        },
        CatalogGiftItem {
            gift: StarGift {
                id: 1004,
                stars: 15,
                stars_converted: 13,
                sticker_document_id: DocumentId::from_tl(1004),
                background: Some(StarGiftBackground {
                    center: RgbColor::new(0xFF, 0x7A, 0x99),
                    edge: RgbColor::new(0xD6, 0x3E, 0x68),
                    text: RgbColor::new(0xFF, 0xFF, 0xFF),
                }),
                ..StarGift::default()
            },
            title: "Velvet Rose".to_owned(),
            artwork_kind: "rose".to_owned(),
        },
        CatalogGiftItem {
            gift: StarGift {
                id: 1005,
                stars: 75,
                stars_converted: 64,
                stars_to_upgrade: 25,
                sticker_document_id: DocumentId::from_tl(1005),
                upgradable: true,
                background: Some(StarGiftBackground {
                    center: RgbColor::new(0x45, 0xD3, 0x9C),
                    edge: RgbColor::new(0x1E, 0x90, 0x6B),
                    text: RgbColor::new(0xFF, 0xFF, 0xFF),
                }),
                ..StarGift::default()
            },
            title: "Orbital Rocket".to_owned(),
            artwork_kind: "rocket".to_owned(),
        },
        CatalogGiftItem {
            gift: StarGift {
                id: 1006,
                stars: 250,
                stars_converted: 212,
                stars_to_upgrade: 100,
                sticker_document_id: DocumentId::from_tl(1006),
                limited_count: 100,
                limited_left: 0,
                sold_out: true,
                upgradable: true,
                background: Some(StarGiftBackground {
                    center: RgbColor::new(0xF6, 0xC4, 0x45),
                    edge: RgbColor::new(0xC9, 0x86, 0x16),
                    text: RgbColor::new(0xFF, 0xFF, 0xFF),
                }),
                ..StarGift::default()
            },
            title: "Golden Trophy".to_owned(),
            artwork_kind: "trophy".to_owned(),
        },
    ]
}

fn build_sample_received_gifts() -> Vec<ReceivedGiftItem> {
    let unique = UniqueGift {
        id: CollectibleId::from_tl(88_001),
        initial_gift_id: 1002,
        slug: "CrystalPrism-42".to_owned(),
        title: "Crystal Prism".to_owned(),
        owner_name: "Alex River".to_owned(),
        owner_id: PeerId::from_user(UserId::from_tl(900_001)),
        stars_for_transfer: 25,
        number: 42,
        can_transfer_at: 1_760_000_000,
        can_resell_at: 1_760_000_000,
        model: UniqueGiftModel {
            attribute: UniqueGiftAttribute {
                name: "Aurora Quartz".to_owned(),
                rarity_value: UniqueGiftRarity::Rare as i32,
            },
            document_id: DocumentId::from_tl(1002),
            crafted: false,
        },
        pattern: UniqueGiftPattern {
            attribute: UniqueGiftAttribute {
                name: "Starburst".to_owned(),
                rarity_value: 15, // 1.5%
            },
            document_id: DocumentId::from_tl(2002),
        },
        backdrop: UniqueGiftBackdrop {
            attribute: UniqueGiftAttribute {
                name: "Midnight Sapphire".to_owned(),
                rarity_value: 20, // 2%
            },
            id: 7,
            center_color: RgbColor::new(0x3B, 0x82, 0xF6),
            edge_color: RgbColor::new(0x1E, 0x3A, 0x8A),
            pattern_color: RgbColor::new(0x93, 0xC5, 0xFD),
            text_color: RgbColor::new(0xFF, 0xFF, 0xFF),
        },
        original_details: UniqueGiftOriginalDetails {
            sender_id: PeerId::from_user(UserId::from_tl(900_002)),
            recipient_id: PeerId::from_user(UserId::from_tl(900_001)),
            date: 1_763_400_000,
            message: "Congrats on shipping the Rust port!".to_owned(),
        },
        ..UniqueGift::default()
    };

    vec![
        ReceivedGiftItem {
            saved: SavedStarGift {
                info: StarGift {
                    id: 88_001,
                    unique: Some(unique),
                    stars: 50,
                    stars_converted: 43,
                    upgradable: false,
                    ..StarGift::default()
                },
                manage_id: Some(SavedStarGiftId::Slug),
                message: "Congrats on shipping the Rust port!".to_owned(),
                stars_converted: 43,
                from_id: PeerId::from_user(UserId::from_tl(900_002)),
                date: 1_763_400_000,
                gift_num: 42,
                pinned: true,
                mine: true,
                ..SavedStarGift::default()
            },
            title: "Crystal Prism #42".to_owned(),
            sender_name: "Sam Taylor".to_owned(),
            artwork_kind: "gem".to_owned(),
        },
        ReceivedGiftItem {
            saved: SavedStarGift {
                info: StarGift {
                    id: 88_002,
                    stars: 75,
                    stars_converted: 64,
                    stars_to_upgrade: 25,
                    upgradable: true,
                    background: Some(StarGiftBackground {
                        center: RgbColor::new(0x45, 0xD3, 0x9C),
                        edge: RgbColor::new(0x1E, 0x90, 0x6B),
                        text: RgbColor::new(0xFF, 0xFF, 0xFF),
                    }),
                    ..StarGift::default()
                },
                manage_id: Some(SavedStarGiftId::User(MsgId::from(940))),
                message: "Happy launch week!".to_owned(),
                stars_converted: 64,
                from_id: PeerId::from_user(UserId::from_tl(900_003)),
                date: 1_763_520_000,
                upgradable: true,
                mine: true,
                ..SavedStarGift::default()
            },
            title: "Orbital Rocket".to_owned(),
            sender_name: "Casey Morgan".to_owned(),
            artwork_kind: "rocket".to_owned(),
        },
        ReceivedGiftItem {
            saved: SavedStarGift {
                info: StarGift {
                    id: 88_003,
                    stars: 15,
                    stars_converted: 13,
                    upgradable: false,
                    background: Some(StarGiftBackground {
                        center: RgbColor::new(0xFF, 0x7A, 0x99),
                        edge: RgbColor::new(0xD6, 0x3E, 0x68),
                        text: RgbColor::new(0xFF, 0xFF, 0xFF),
                    }),
                    ..StarGift::default()
                },
                manage_id: Some(SavedStarGiftId::User(MsgId::from(945))),
                message: "Thanks for the code review!".to_owned(),
                stars_converted: 13,
                from_id: PeerId::ZERO,
                date: 1_763_610_000,
                anonymous: true,
                mine: true,
                ..SavedStarGift::default()
            },
            title: "Velvet Rose".to_owned(),
            sender_name: "Anonymous".to_owned(),
            artwork_kind: "rose".to_owned(),
        },
    ]
}

/// In-memory fake implementation of [`PassportBackend`] for local testing and screenshots.
pub struct MockPassportBackend {
    has_cloud_password: bool,
    password: String,
    password_hint: String,
    form: PassportForm,
}

impl Default for MockPassportBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPassportBackend {
    /// Creates a new [`MockPassportBackend`] with a sample bot authorization request.
    #[must_use]
    pub fn new() -> Self {
        Self {
            has_cloud_password: true,
            password: DEMO_CLOUD_PASSWORD.to_owned(),
            password_hint: "demo1234".to_owned(),
            form: build_sample_passport_form(),
        }
    }

    /// Configures whether the mock account already has a cloud password (`true`) or needs first-time
    /// password/secret creation (`false`).
    #[must_use]
    pub fn with_cloud_password(mut self, has_password: bool) -> Self {
        self.has_cloud_password = has_password;
        self
    }
}

fn build_sample_passport_form() -> PassportForm {
    let mut values = BTreeMap::new();

    // 1. Personal Details (complete, valid)
    let mut personal = PassportValue::new(ValueType::PersonalDetails);
    personal.data.parsed = ValueMap::from_texts([
        ("first_name", "Alex"),
        ("middle_name", ""),
        ("last_name", "River"),
        ("birth_date", "14.05.1994"),
        ("gender", "male"),
        ("country_code", "US"),
        ("residence_country_code", "US"),
    ]);
    values.insert(ValueType::PersonalDetails, personal);

    // 2. Identity Document: Passport (front side + selfie present)
    let mut passport = PassportValue::new(ValueType::Passport);
    passport.selfie_required = true;
    passport.data.parsed =
        ValueMap::from_texts([("document_no", "X00019283"), ("expiry_date", "14.05.2031")]);
    passport.special_scans.insert(
        FileType::FrontSide,
        PassportFile {
            id: 5001,
            size: 184_320,
            date: 1_763_000_000,
            ..PassportFile::default()
        },
    );
    passport.special_scans.insert(
        FileType::Selfie,
        PassportFile {
            id: 5002,
            size: 215_040,
            date: 1_763_000_100,
            ..PassportFile::default()
        },
    );
    values.insert(ValueType::Passport, passport);

    // Alternative identity document option: Driver License (empty initially)
    let mut license = PassportValue::new(ValueType::DriverLicense);
    license.selfie_required = true;
    values.insert(ValueType::DriverLicense, license);

    // 3. Residential Address (partial: missing postcode to demonstrate validation!)
    let mut address = PassportValue::new(ValueType::Address);
    address.data.parsed = ValueMap::from_texts([
        ("street_line1", "742 Evergreen Terrace"),
        ("street_line2", "Suite 4B"),
        ("city", "Springfield"),
        ("state", "OR"),
        ("country_code", "US"),
        ("post_code", ""),
    ]);
    values.insert(ValueType::Address, address);

    // 4. Proof of Address: Utility Bill (1 scan placeholder)
    let mut utility_bill = PassportValue::new(ValueType::UtilityBill);
    utility_bill.scans.push(PassportFile {
        id: 5010,
        size: 142_000,
        date: 1_763_100_000,
        ..PassportFile::default()
    });
    values.insert(ValueType::UtilityBill, utility_bill);
    values.insert(
        ValueType::BankStatement,
        PassportValue::new(ValueType::BankStatement),
    );

    // 5. Phone (filled)
    let mut phone = PassportValue::new(ValueType::Phone);
    phone.data.parsed.set_text("value", "15550102468");
    values.insert(ValueType::Phone, phone);

    // 6. Email (filled)
    let mut email = PassportValue::new(ValueType::Email);
    email
        .data
        .parsed
        .set_text("value", "alex.river@example.test");
    values.insert(ValueType::Email, email);

    PassportForm {
        request: vec![
            vec![ValueType::PersonalDetails],
            vec![ValueType::Passport, ValueType::DriverLicense],
            vec![ValueType::Address],
            vec![ValueType::UtilityBill, ValueType::BankStatement],
            vec![ValueType::Phone],
            vec![ValueType::Email],
        ],
        values,
        privacy_policy_url: "https://example.test/vericheck/privacy".to_owned(),
        pending_errors: Vec::new(),
    }
}

impl PassportBackend for MockPassportBackend {
    fn load_authorization_form(&self, bot_username: &str) -> Result<PassportSessionBundle, String> {
        Ok(PassportSessionBundle {
            bot_name: "VeriCheck Demo Bot".to_owned(),
            bot_username: if bot_username.is_empty() {
                SAMPLE_PASSPORT_BOT.to_owned()
            } else {
                bot_username.to_owned()
            },
            has_cloud_password: self.has_cloud_password,
            password_hint: self.password_hint.clone(),
            has_recovery: true,
            has_secure_values: true,
            form: self.form.clone(),
        })
    }

    fn verify_cloud_password(&mut self, password: &str) -> Result<PassportUnlockResult, String> {
        if password == self.password {
            Ok(PassportUnlockResult {
                form: self.form.clone(),
                notice: "Passport master secret unlocked (local demo).".to_owned(),
            })
        } else {
            Err(format!(
                "Incorrect cloud password (hint: \"{}\").",
                self.password_hint
            ))
        }
    }

    fn setup_cloud_password(
        &mut self,
        password: &str,
        hint: &str,
    ) -> Result<PassportUnlockResult, String> {
        if password.len() < 4 {
            return Err("Password must be at least 4 characters.".to_owned());
        }
        self.has_cloud_password = true;
        self.password = password.to_owned();
        self.password_hint = hint.to_owned();
        Ok(PassportUnlockResult {
            form: self.form.clone(),
            notice: "Cloud password & Passport master secret created (local demo).".to_owned(),
        })
    }

    fn upload_passport_file(
        &mut self,
        value_type: ValueType,
        file_type: FileType,
        raw_bytes: &[u8],
    ) -> Result<PassportFile, String> {
        if raw_bytes.is_empty() {
            return Err("Passport scan file cannot be empty.".to_owned());
        }
        let hash = tg_crypto::hash::sha256(&[raw_bytes]).to_vec();
        let file = PassportFile {
            id: 8_000 + raw_bytes.len() as i64,
            size: raw_bytes.len() as i64,
            date: 1_763_760_000,
            hash,
            ..PassportFile::default()
        };
        let entry = self
            .form
            .values
            .entry(value_type)
            .or_insert_with(|| PassportValue::new(value_type));
        match file_type {
            FileType::FrontSide | FileType::ReverseSide | FileType::Selfie => {
                entry.special_scans.insert(file_type, file.clone());
            }
            FileType::Scan => entry.scans.push(file.clone()),
            FileType::Translation => entry.translations.push(file.clone()),
        }
        Ok(file)
    }

    fn save_passport_value(
        &mut self,
        value_type: ValueType,
        fields: ValueMap,
        scan_labels: Vec<String>,
    ) -> Result<PassportForm, String> {
        let entry = self
            .form
            .values
            .entry(value_type)
            .or_insert_with(|| PassportValue::new(value_type));
        entry.data.parsed = fields;
        entry.error.clear();

        // Update placeholder scans according to value_type.
        match value_type {
            ValueType::Passport
            | ValueType::DriverLicense
            | ValueType::IdentityCard
            | ValueType::InternalPassport => {
                entry.special_scans.clear();
                for (idx, label) in scan_labels.iter().enumerate() {
                    let ft = if label.eq_ignore_ascii_case("reverse") {
                        FileType::ReverseSide
                    } else if label.eq_ignore_ascii_case("selfie") {
                        FileType::Selfie
                    } else {
                        FileType::FrontSide
                    };
                    entry.special_scans.insert(
                        ft,
                        PassportFile {
                            id: 6000 + idx as i64,
                            size: 160_000,
                            date: 1_763_760_000,
                            ..PassportFile::default()
                        },
                    );
                }
            }
            ValueType::UtilityBill
            | ValueType::BankStatement
            | ValueType::RentalAgreement
            | ValueType::PassportRegistration
            | ValueType::TemporaryRegistration => {
                entry.scans = scan_labels
                    .iter()
                    .enumerate()
                    .map(|(idx, _)| PassportFile {
                        id: 7000 + idx as i64,
                        size: 135_000,
                        date: 1_763_760_000,
                        ..PassportFile::default()
                    })
                    .collect();
            }
            _ => {}
        }

        Ok(self.form.clone())
    }

    fn send_contact_verification_code(
        &mut self,
        value_type: ValueType,
        value: &str,
    ) -> Result<PassportVerificationChallenge, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("Contact value cannot be empty.".to_owned());
        }
        match value_type {
            ValueType::Phone => Ok(PassportVerificationChallenge {
                value_type,
                target: trimmed.to_owned(),
                code_length: 5,
                phone_code_hash: Some("mock_phone_hash_28".to_owned()),
                email_pattern: None,
                fragment_url: None,
                next_call_timeout_secs: Some(60),
            }),
            ValueType::Email => Ok(PassportVerificationChallenge {
                value_type,
                target: trimmed.to_owned(),
                code_length: 6,
                phone_code_hash: None,
                email_pattern: Some(trimmed.to_owned()),
                fragment_url: None,
                next_call_timeout_secs: None,
            }),
            _ => Err("Verification codes are only used for phone and email.".to_owned()),
        }
    }

    fn resend_phone_verification_code(
        &mut self,
        phone: &str,
        phone_code_hash: &str,
    ) -> Result<PassportVerificationChallenge, String> {
        if phone_code_hash.trim().is_empty() {
            return Err("Missing phone code hash.".to_owned());
        }
        Ok(PassportVerificationChallenge {
            value_type: ValueType::Phone,
            target: phone.trim().to_owned(),
            code_length: 5,
            phone_code_hash: Some(format!("{phone_code_hash}_resent")),
            email_pattern: None,
            fragment_url: None,
            next_call_timeout_secs: None,
        })
    }

    fn confirm_contact_verification_code(
        &mut self,
        value_type: ValueType,
        value: &str,
        _phone_code_hash: Option<&str>,
        code: &str,
    ) -> Result<PassportForm, String> {
        let trimmed_code = code.trim();
        let expected = match value_type {
            ValueType::Phone => "12345",
            ValueType::Email => "123456",
            _ => return Err("Unsupported contact verification type.".to_owned()),
        };
        if trimmed_code != expected {
            return Err(format!(
                "Invalid verification code (demo code: {expected})."
            ));
        }
        let mut fields = ValueMap::default();
        fields.set_text("value", value.trim().to_owned());
        self.save_passport_value(value_type, fields, Vec::new())
    }

    fn delete_passport_values(&mut self, types: &[ValueType]) -> Result<PassportForm, String> {
        for &vt in types {
            self.form.values.insert(vt, PassportValue::new(vt));
        }
        Ok(self.form.clone())
    }

    fn reset_passport(&mut self, password: &str) -> Result<PassportUnlockResult, String> {
        if self.has_cloud_password && password != self.password {
            return Err(format!(
                "Incorrect cloud password (hint: \"{}\").",
                self.password_hint
            ));
        }
        let all_types: Vec<ValueType> = self.form.values.keys().copied().collect();
        for vt in all_types {
            self.form.values.insert(vt, PassportValue::new(vt));
        }
        Ok(PassportUnlockResult {
            form: self.form.clone(),
            notice: "Telegram Passport data and secret reset.".to_owned(),
        })
    }

    fn accept_authorization(&mut self) -> Result<String, String> {
        Ok("Passport data encrypted & shared with @vericheck_demo_bot (offline demo).".to_owned())
    }

    fn decline_authorization(&mut self) -> Result<String, String> {
        Ok("Authorization declined. No Passport data was shared.".to_owned())
    }
}
