//! WP-50 integration tests:
//! 1. Live Payments 3-D Secure `tg-webview` handoff (`PaymentVerificationHost` + `CheckoutView`):
//!    `payments.sendPaymentForm` -> `PaymentResult::PaymentVerificationNeeded` ->
//!    `FakePaymentVerificationHost` -> intermediate bank navigation allowed -> `tg://` completion
//!    redirect advances to Receipt; `tg://cancel` returns to Summary; zero `"Placeholder"` text.
//! 2. Live Stars Subscription Management (`StarsView` + `payments.changeStarsSubscription`):
//!    clicking a subscription opens confirmation modal without sending RPC; pressing Cancel/Renew
//!    sends exact decoded `payments.changeStarsSubscription` and refreshes subscription list.
//! 3. Live Premium Subscribe read-only invoice preview (`PremiumView` + `payments.getPaymentForm`,
//!    never auto-purchasing) and Boost Channel confirmation (`BoostsGiveawaysView` + `premium.applyBoost`).
//! 4. Star Gift Auction bid confirmation (`payments.getPaymentForm` + `payments.sendStarsForm`) and
//!    Giveaway invoice confirmation (`payments.getPaymentForm` read-only), plus verification that
//!    Passport validation, Statistics strings, and Settings Premium strings contain zero placeholder copy.
//!
//! All tests run strictly against `FakeRpcClient` and `FakePaymentVerificationHost` — zero real
//! Telegram servers, zero real payment providers, and never auto-purchasing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::payments::marketplace_backend::GiveawayOptionsBundle;
use tg_app_core::payments::verification::{FakePaymentVerificationHost, PaymentVerificationHost};
use tg_data::{ChannelId, PeerId as DataPeerId, UserId as DataUserId};
use tg_passport::types::ValueType;
use tg_payments::card::UncheckedCardDetails;
use tg_payments::checkout::CheckoutMode;
use tg_payments::credits::CreditsAmount;
use tg_payments::giveaways::{GiveawayDraft, GiveawayPrizeKind};
use tg_payments::providers::{HttpClient, HttpRequest, HttpResponse, HttpTransportError};
use tg_payments::star_gift_auctions::{
    ActiveAuctionsSnapshot, GiftAuctionState, StarGiftAuctionMyState,
};
use tg_payments::star_gifts::StarGift;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::live::LiveSession;
use tg_app::payments::PaymentsScreenHost;
use tg_app::payments::backend::{PassportBackend, PaymentsBackend};
use tg_app::payments::checkout::{CheckoutSubView, CheckoutView};
use tg_app::payments::credits::{StarsModal, StarsView};
use tg_app::payments::giveaways_ui::CreateGiveawayPanel;
use tg_app::payments::mock::{MockPassportBackend, SAMPLE_PASSPORT_BOT};
use tg_app::payments::passport::PassportView;
use tg_app::payments::premium::{BoostsGiveawaysView, PremiumView};
use tg_app::payments::star_gifts_marketplace::AuctionsView;
use tg_app::settings::strings as settings_strings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::PeerId;
use tg_app::statistics::strings as stats_strings;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

// =============================================================================
// Reusable FakeRpcClient (zero network access)
// =============================================================================

#[derive(Debug, Clone)]
struct RecordedCall {
    ctor: u32,
    bytes: Vec<u8>,
}

type QueuedResponse = (Option<u32>, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: DcId,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    queued: Arc<Mutex<VecDeque<QueuedResponse>>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: 2,
            updates_tx,
            queued: Arc::new(Mutex::new(VecDeque::new())),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.queued
            .lock()
            .expect("lock queue")
            .push_back((Some(expected_ctor), Ok(resp.to_bytes())));
    }

    fn recorded(&self) -> Vec<RecordedCall> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn count_ctor(&self, ctor: u32) -> usize {
        self.recorded()
            .iter()
            .filter(|call| call.ctor == ctor)
            .count()
    }

    fn decode_last<R: RemoteCall + Deserializable>(&self, ctor: u32) -> R {
        let calls = self.recorded();
        let call = calls
            .iter()
            .rev()
            .find(|c| c.ctor == ctor)
            .unwrap_or_else(|| panic!("no recorded call for ctor 0x{ctor:08x}"));
        R::from_bytes(&call.bytes)
            .unwrap_or_else(|e| panic!("failed to decode ctor 0x{ctor:08x}: {e}"))
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push(RecordedCall {
                ctor,
                bytes: bytes.clone(),
            });

        let popped = {
            let mut q = self.queued.lock().expect("lock queue");
            if let Some(pos) = q
                .iter()
                .position(|(expected, _)| expected.is_none() || *expected == Some(ctor))
            {
                q.remove(pos).map(|(_, res)| res)
            } else {
                None
            }
        };

        let raw = popped.ok_or_else(|| {
            InvocationError::Deserialize(format!("unexpected RPC ctor 0x{ctor:08x} on dc {dc_id}"))
        })??;

        R::Return::from_bytes(&raw)
            .map_err(|e| InvocationError::Deserialize(format!("ctor 0x{ctor:08x}: {e}")))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        self.main_dc
    }
}

fn synthetic_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    bot: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !bot,
        mutual_contact: false,
        deleted: false,
        bot,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: true,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 100 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn synthetic_channel(id: i64, title: &str, username: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 37 + 11),
        title: title.to_owned(),
        username: Some(username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_760_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(1_200),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: Some(6),
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn make_live_session(
    rpc: FakeRpcClient,
    self_id: i64,
    extra_users: Vec<tl_types::User>,
    extra_channels: Vec<tl_types::Channel>,
) -> (
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    LiveSession,
) {
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_id),
            tg_app::live::ErasedRpcClient::new(rpc),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("runtime ready");
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let extra_user_ids: Vec<i64> = extra_users.iter().map(|u| u.id).collect();
        let extra_channel_ids: Vec<i64> = extra_channels.iter().map(|c| c.id).collect();
        let mut users = vec![tl_enums::User::User(synthetic_user(
            self_id,
            "Self Operator",
            "self_operator",
            true,
            false,
        ))];
        users.extend(extra_users.into_iter().map(tl_enums::User::User));
        let chats: Vec<tl_enums::Chat> = extra_channels
            .into_iter()
            .map(tl_enums::Chat::Channel)
            .collect();
        apply_tl_users_and_chats(&mut s, &users, &chats);
        for uid in extra_user_ids {
            let pid = DataPeerId::from_user(DataUserId::from_tl(uid));
            s.ensure_history(pid).in_chat_list = true;
            s.sync_dialog_lists(pid);
        }
        for cid in extra_channel_ids {
            let pid = DataPeerId::from_channel(ChannelId::from_tl(cid));
            s.ensure_history(pid).in_chat_list = true;
            s.sync_dialog_lists(pid);
        }
    });
    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (runtime, live)
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_765_000_000,
        seq: 1,
    })
}

fn sample_invoice_tl(label: &str, amount: i64, currency: &str) -> tl_enums::Invoice {
    tl_enums::Invoice::Invoice(tl_types::Invoice {
        test: true,
        name_requested: false,
        phone_requested: false,
        email_requested: false,
        shipping_address_requested: false,
        flexible: false,
        phone_to_provider: false,
        email_to_provider: false,
        recurring: false,
        currency: currency.to_owned(),
        prices: vec![tl_enums::LabeledPrice::LabeledPrice(
            tl_types::LabeledPrice {
                label: label.to_owned(),
                amount,
            },
        )],
        max_tip_amount: None,
        suggested_tip_amounts: None,
        terms_url: None,
        subscription_period: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn sample_payment_form_tl(
    form_id: i64,
    bot_id: i64,
    title: &str,
    description: &str,
    label: &str,
    amount: i64,
    currency: &str,
    url: &str,
    saved_credentials: Option<Vec<tl_enums::PaymentSavedCredentials>>,
) -> tl_enums::payments::PaymentForm {
    tl_enums::payments::PaymentForm::PaymentForm(tl_types::payments::PaymentForm {
        can_save_credentials: true,
        password_missing: false,
        form_id,
        bot_id,
        title: title.to_owned(),
        description: description.to_owned(),
        photo: None,
        invoice: sample_invoice_tl(label, amount, currency),
        provider_id: bot_id,
        url: url.to_owned(),
        native_provider: Some("stripe".to_owned()),
        native_params: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
            data: r#"{"publishable_key":"pk_test_wp50","need_country":false,"need_zip":false}"#
                .to_owned(),
        })),
        additional_methods: None,
        saved_info: None,
        saved_credentials,
        users: Vec::new(),
    })
}

fn stars_status_tl(
    balance_stars: i64,
    subscriptions: Vec<tl_enums::StarsSubscription>,
    chats: Vec<tl_enums::Chat>,
) -> tl_enums::payments::StarsStatus {
    tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
        balance: CreditsAmount::stars(balance_stars).to_tl(),
        subscriptions: Some(subscriptions),
        subscriptions_next_offset: None,
        subscriptions_missing_balance: None,
        history: Some(Vec::new()),
        next_offset: None,
        chats,
        users: Vec::new(),
    })
}

fn subscription_tl(
    id: &str,
    channel_id: i64,
    title: &str,
    canceled: bool,
) -> tl_enums::StarsSubscription {
    tl_enums::StarsSubscription::StarsSubscription(tl_types::StarsSubscription {
        canceled,
        can_refulfill: false,
        missing_balance: false,
        bot_canceled: false,
        id: id.to_owned(),
        peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id }),
        until_date: 1_800_000_000,
        pricing: tl_enums::StarsSubscriptionPricing::StarsSubscriptionPricing(
            tl_types::StarsSubscriptionPricing {
                period: 30 * 86_400,
                amount: 150,
            },
        ),
        chat_invite_hash: None,
        title: Some(title.to_owned()),
        photo: None,
        invoice_slug: None,
    })
}

fn collect_all_accessible_text(harness: &Harness<'_, impl Sized>) -> String {
    harness
        .query_all_by_role(egui::accesskit::Role::Label)
        .chain(harness.query_all_by_role(egui::accesskit::Role::Button))
        .filter_map(|n| n.value().or_else(|| n.accesskit_node().label()))
        .collect::<Vec<_>>()
        .join("\n")
}

// =============================================================================
// Test 1: 3-D Secure tg-webview Handoff (VerificationNeeded -> Redirect -> Receipt / Cancel)
// =============================================================================

struct FakeStripeHttpClient;

impl HttpClient for FakeStripeHttpClient {
    fn send(&self, _req: &HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        Ok(HttpResponse {
            status_code: 200,
            body: br#"{"id":"tok_wp50_3ds","livemode":false,"created":1750000000,"card":{"id":"card_1","last4":"4242","brand":"Visa","exp_month":12,"exp_year":2030}}"#.to_vec(),
        })
    }
}

#[test]
fn wp50_checkout_3ds_webview_handoff_completion_and_cancel() {
    let rpc = FakeRpcClient::new();
    let bot = synthetic_user(700_001, "Synth Merchant Bot", "synth_shop_bot", false, true);
    let (_runtime, mut live) = make_live_session(rpc.clone(), 500_001, vec![bot.clone()], vec![]);
    live.set_payment_http_client(Arc::new(FakeStripeHttpClient));

    // 1. Load checkout form via `payments.getPaymentForm`.
    rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &sample_payment_form_tl(
            42_050,
            bot.id,
            "Synth Bundle",
            "Audio pack checkout test",
            "Synth Bundle",
            1999,
            "USD",
            "https://checkout.example.test/pay",
            None,
        ),
    );
    let bundle = live
        .load_checkout_form("wp50_3ds_slug")
        .expect("load_checkout_form");
    assert_eq!(bundle.form.details.form_id, 42_050);

    // Tokenize documented Stripe test card against the local fake HTTP client.
    let creds = live
        .tokenize_test_card(
            &UncheckedCardDetails {
                number: "4242 4242 4242 4242".to_owned(),
                cvc: "123".to_owned(),
                expire_year: 2030,
                expire_month: 12,
                cardholder_name: "Alex Tester".to_owned(),
                address_country: "US".to_owned(),
                address_zip: "10001".to_owned(),
            },
            false,
        )
        .expect("tokenize_test_card");

    // 2. Submit payment form -> server returns `PaymentVerificationNeeded(url)`.
    let verification_url = "https://3ds.example.test/acs/challenge?tx=wp50_01";
    rpc.enqueue_ok(
        tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentVerificationNeeded(
            tl_types::payments::PaymentVerificationNeeded {
                url: verification_url.to_owned(),
            },
        ),
    );

    let mut checkout = CheckoutView::new(bundle.clone());
    let (fake_host, ctrl) = FakePaymentVerificationHost::new();
    checkout.set_verification_host(Box::new(fake_host));

    // Confirm checkout via live backend (simulating user pressing Pay & Confirm).
    let outcome = live
        .submit_payment_form("wp50_3ds_slug", 42_050, None, 0, &creds.title, false)
        .expect("submit_payment_form");
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID),
        1
    );
    match outcome {
        tg_app::payments::backend::PaymentSubmitOutcome::VerificationNeeded { url, provider } => {
            assert_eq!(url, verification_url);
            let _ = checkout.begin_verification(url, provider);
        }
        other => panic!("expected VerificationNeeded, got {other:?}"),
    }

    assert_eq!(checkout.sub_view, CheckoutSubView::VerificationWebView);
    assert!(ctrl.is_active());
    let opened = ctrl.opened_requests();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].0, verification_url);

    // Render in egui_kittest and verify zero "Placeholder" strings appear.
    let palette = Palette::day();
    let mut harness = Harness::builder()
        .with_size(Vec2::new(720.0, 620.0))
        .build_ui_state(
            move |ui, state: &mut (CheckoutView, LiveSession)| {
                let (cv, be) = state;
                let _ = cv.show(ui, be, &palette);
            },
            (checkout, live),
        );
    harness.run_steps(2);

    let rendered = collect_all_accessible_text(&harness);
    assert!(
        !rendered.to_ascii_lowercase().contains("placeholder"),
        "3-D Secure view must not render placeholder text; got:\n{rendered}"
    );
    assert!(
        harness
            .query_by_label_contains("Cancel Verification")
            .is_some(),
        "Cancel Verification button must be visible"
    );

    // 3. Intermediate bank HTTPS navigation is allowed (does NOT close/complete verification).
    assert!(ctrl.simulate_redirect("https://3ds.example.test/acs/otp-step"));
    harness.run_steps(2);
    assert_eq!(
        harness.state().0.sub_view,
        CheckoutSubView::VerificationWebView
    );

    // 4. Completion redirect (`tg://resolve?...`) is intercepted and advances to Receipt!
    assert!(!ctrl.simulate_redirect("tg://resolve?domain=payments_success&tx=wp50_01"));
    harness.run_steps(2);
    assert_eq!(harness.state().0.mode, CheckoutMode::Receipt);
    assert_eq!(harness.state().0.sub_view, CheckoutSubView::Summary);
    assert!(!ctrl.is_active());
    assert_eq!(ctrl.close_count(), 1);

    // 5. Verify `tg://cancel` returns cleanly to CheckoutSubView::Summary without Receipt.
    let mut host = PaymentsScreenHost::with_mock();
    host.set_verification_host_factory(Some(Box::new(|| {
        let (inner, _) = FakePaymentVerificationHost::new();
        Box::new(inner) as Box<dyn PaymentVerificationHost>
    })));
    let mut checkout2 = CheckoutView::new(bundle);
    let (fh2, ctrl2) = FakePaymentVerificationHost::new();
    checkout2.set_verification_host(Box::new(fh2));
    let _ = checkout2.begin_verification(
        "https://3ds.example.test/acs/challenge?tx=wp50_cancel".to_owned(),
        "Stripe".to_owned(),
    );
    assert_eq!(checkout2.sub_view, CheckoutSubView::VerificationWebView);
    assert!(!ctrl2.simulate_redirect("tg://cancel"));
    let toast = checkout2.poll_verification_host();
    assert!(toast.is_some(), "cancel redirect should emit toast");
    assert_eq!(checkout2.sub_view, CheckoutSubView::Summary);
    assert_eq!(checkout2.mode, CheckoutMode::Payment);
    assert_eq!(ctrl2.close_count(), 1);
}

// =============================================================================
// Test 2: Live Stars Subscription Management (`payments.changeStarsSubscription`)
// =============================================================================

#[test]
fn wp50_stars_subscription_cancel_and_renew_via_live_rpc() {
    let rpc = FakeRpcClient::new();
    let ch = synthetic_channel(2_000_303, "Rust Audio Builders", "rust_audio_builders");
    let (_runtime, live) = make_live_session(rpc.clone(), 500_002, vec![], vec![ch.clone()]);

    let active_sub = subscription_tl("sub_live_1", ch.id, "Rust Audio Builders Pro", false);
    let canceled_sub = subscription_tl("sub_live_1", ch.id, "Rust Audio Builders Pro", true);
    let chats = vec![tl_enums::Chat::Channel(ch.clone())];

    let enqueue_overview = |rpc: &FakeRpcClient, subs: Vec<tl_enums::StarsSubscription>| {
        let status = stars_status_tl(1_250, subs, chats.clone());
        rpc.enqueue_ok(tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID, &status);
        rpc.enqueue_ok(
            tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID,
            &status,
        );
        rpc.enqueue_ok(
            tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
            &stars_status_tl(0, vec![], vec![]),
        );
        rpc.enqueue_ok(
            tl_fns::payments::GetStarsTopupOptions::CONSTRUCTOR_ID,
            &Vec::<tl_enums::StarsTopupOption>::new(),
        );
    };

    enqueue_overview(&rpc, vec![active_sub.clone()]);
    let initial = live.load_credits_overview().expect("load_credits_overview");
    assert_eq!(initial.subscriptions.len(), 1);
    assert!(!initial.subscriptions[0].cancelled);

    let mut stars_view = StarsView::new(initial);
    // Open the subscription management confirmation modal (simulating clicking the subscription row).
    stars_view.modal = Some(StarsModal::SubscriptionManage {
        entry: stars_view.subscriptions[0].clone(),
        error: None,
    });

    // Money rule: opening the modal MUST NOT call `payments.changeStarsSubscription`.
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID),
        0
    );

    // Enqueue `payments.changeStarsSubscription` + post-cancel refresh.
    rpc.enqueue_ok(
        tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID,
        &true,
    );
    enqueue_overview(&rpc, vec![canceled_sub.clone()]);

    let palette = Palette::day();
    let mut harness = Harness::builder()
        .with_size(Vec2::new(720.0, 720.0))
        .build_ui_state(
            move |ui, state: &mut (StarsView, LiveSession)| {
                let (sv, be) = state;
                let _ = sv.show(ui, be, &palette);
            },
            (stars_view, live),
        );
    harness.run_steps(2);

    let rendered = collect_all_accessible_text(&harness);
    assert!(
        !rendered.to_ascii_lowercase().contains("placeholder"),
        "Stars subscription modal must not show placeholder text; got:\n{rendered}"
    );

    // Press "Cancel Subscription" button.
    harness.get_by_label("Cancel Subscription").click();
    harness.run_steps(2);

    assert_eq!(
        rpc.count_ctor(tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID),
        1
    );
    let cancel_req: tl_fns::payments::ChangeStarsSubscription =
        rpc.decode_last(tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID);
    assert_eq!(cancel_req.subscription_id, "sub_live_1");
    assert_eq!(cancel_req.canceled, Some(true));
    assert!(matches!(cancel_req.peer, tl_enums::InputPeer::Self_));
    assert!(harness.state().0.modal.is_none());
    assert!(harness.state().0.subscriptions[0].cancelled);

    // Now re-open modal on the canceled subscription and press "Renew Subscription".
    let sub_now = harness.state().0.subscriptions[0].clone();
    harness.state_mut().0.modal = Some(StarsModal::SubscriptionManage {
        entry: sub_now,
        error: None,
    });
    rpc.enqueue_ok(
        tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID,
        &true,
    );
    enqueue_overview(&rpc, vec![active_sub]);
    harness.run_steps(2);

    harness.get_by_label("Renew Subscription").click();
    harness.run_steps(2);

    assert_eq!(
        rpc.count_ctor(tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID),
        2
    );
    let renew_req: tl_fns::payments::ChangeStarsSubscription =
        rpc.decode_last(tl_fns::payments::ChangeStarsSubscription::CONSTRUCTOR_ID);
    assert_eq!(renew_req.subscription_id, "sub_live_1");
    assert_eq!(renew_req.canceled, Some(false));
    assert!(!harness.state().0.subscriptions[0].cancelled);
}

// =============================================================================
// Test 3: Live Premium Subscribe Preview (`payments.getPaymentForm`, read-only)
//         & Boost Channel Confirmation (`premium.applyBoost`)
// =============================================================================

#[test]
fn wp50_premium_subscribe_preview_and_boost_channel_confirm() {
    let rpc = FakeRpcClient::new();
    let ch = synthetic_channel(2_000_505, "Northwind Sound Lab", "northwind_sound");
    let (_runtime, live) = make_live_session(rpc.clone(), 500_003, vec![], vec![ch.clone()]);

    // --- Part A: Premium Subscribe CTA invokes read-only `payments.getPaymentForm` ---
    rpc.enqueue_ok(
        tl_fns::help::GetPremiumPromo::CONSTRUCTOR_ID,
        &tl_enums::help::PremiumPromo::PremiumPromo(tl_types::help::PremiumPromo {
            status_text: "Go beyond the limits.".to_owned(),
            status_entities: Vec::new(),
            video_sections: vec!["double_limits".to_owned()],
            videos: vec![tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 1 })],
            period_options: vec![
                tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(
                    tl_types::PremiumSubscriptionOption {
                        current: false,
                        can_purchase_upgrade: false,
                        transaction: None,
                        months: 12,
                        currency: "USD".to_owned(),
                        amount: 3_999,
                        bot_url: "https://t.me/$premium_annual_wp50".to_owned(),
                        store_product: None,
                    },
                ),
            ],
            users: Vec::new(),
        }),
    );
    let promo = live.load_premium_promo().expect("load_premium_promo");
    let prem_view = PremiumView::new(promo);
    assert!(
        !prem_view
            .subscribe_button_label()
            .to_ascii_lowercase()
            .contains("placeholder"),
        "Subscribe button label must not mention placeholder"
    );

    rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &sample_payment_form_tl(
            77_012,
            700_099,
            "Telegram Premium (12 months)",
            "Annual Premium plan",
            "Telegram Premium",
            3_999,
            "USD",
            "https://t.me/$premium_annual_wp50",
            None,
        ),
    );

    let palette = Palette::day();
    let mut prem_harness = Harness::builder()
        .with_size(Vec2::new(760.0, 760.0))
        .allow_missing_glyphs()
        .build_ui_state(
            move |ui, state: &mut (PremiumView, LiveSession)| {
                let (pv, be) = state;
                let _ = pv.show_with_backend(ui, be, &palette);
            },
            (prem_view, live),
        );
    prem_harness.run_steps(2);

    // Click Subscribe CTA -> requests read-only `payments.getPaymentForm`, NEVER submits payment!
    let sub_label = prem_harness.state().0.subscribe_button_label();
    prem_harness.get_by_label(&sub_label).click();
    prem_harness.run_steps(2);

    assert_eq!(
        rpc.count_ctor(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID),
        0,
        "Subscribe preview MUST NEVER auto-purchase via SendPaymentForm"
    );
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID),
        0,
        "Subscribe preview MUST NEVER auto-purchase via SendStarsForm"
    );
    let form_req: tl_fns::payments::GetPaymentForm =
        rpc.decode_last(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID);
    assert!(
        matches!(
            form_req.invoice,
            tl_enums::InputInvoice::Slug(tl_types::InputInvoiceSlug { ref slug })
            if slug == "premium_annual_wp50"
        ),
        "expected InputInvoice::Slug(premium_annual_wp50), got {:?}",
        form_req.invoice
    );
    let preview = prem_harness
        .state()
        .0
        .invoice_preview
        .as_ref()
        .expect("invoice_preview populated");
    assert_eq!(preview.form_id, 77_012);
    let prem_text = collect_all_accessible_text(&prem_harness);
    assert!(
        !prem_text.to_ascii_lowercase().contains("placeholder"),
        "Premium view must not render placeholder text; got:\n{prem_text}"
    );

    // --- Part B: Boost Channel CTA requires explicit Confirm before `premium.applyBoost` ---
    let (_pv, live) = prem_harness.into_state();
    let enqueue_boosts_load = |rpc: &FakeRpcClient, boost_count: i32, slot2_assigned: bool| {
        rpc.enqueue_ok(
            tl_fns::premium::GetBoostsStatus::CONSTRUCTOR_ID,
            &tl_enums::premium::BoostsStatus::BoostsStatus(tl_types::premium::BoostsStatus {
                my_boost: true,
                level: 6,
                current_level_boosts: 40,
                boosts: boost_count,
                gift_boosts: None,
                next_level_boosts: Some(50),
                premium_audience: None,
                boost_url: "https://t.me/boost/northwind_sound".to_owned(),
                prepaid_giveaways: None,
                my_boost_slots: Some(if slot2_assigned { vec![1, 2] } else { vec![1] }),
            }),
        );
        rpc.enqueue_ok(
            tl_fns::premium::GetBoostsList::CONSTRUCTOR_ID,
            &tl_enums::premium::BoostsList::BoostsList(tl_types::premium::BoostsList {
                count: 0,
                boosts: Vec::new(),
                next_offset: None,
                users: Vec::new(),
            }),
        );
        rpc.enqueue_ok(
            tl_fns::premium::GetMyBoosts::CONSTRUCTOR_ID,
            &tl_enums::premium::MyBoosts::MyBoosts(tl_types::premium::MyBoosts {
                my_boosts: vec![
                    tl_enums::MyBoost::MyBoost(tl_types::MyBoost {
                        slot: 1,
                        peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: ch.id,
                        })),
                        date: 1_760_000_000,
                        expires: 1_780_000_000,
                        cooldown_until_date: None,
                    }),
                    tl_enums::MyBoost::MyBoost(tl_types::MyBoost {
                        slot: 2,
                        peer: slot2_assigned.then_some(tl_enums::Peer::Channel(
                            tl_types::PeerChannel { channel_id: ch.id },
                        )),
                        date: if slot2_assigned { 1_765_000_000 } else { 0 },
                        expires: if slot2_assigned { 1_785_000_000 } else { 0 },
                        cooldown_until_date: None,
                    }),
                ],
                chats: vec![tl_enums::Chat::Channel(ch.clone())],
                users: Vec::new(),
            }),
        );
    };

    live.set_boosts_channel_peer(PeerId::from_channel(ch.id as u64));
    enqueue_boosts_load(&rpc, 44, false);
    let boosts_bundle = live.load_boosts_giveaway().expect("load_boosts_giveaway");
    assert_eq!(boosts_bundle.boost_status.overview.boost_count, 44);
    let boosts_view = BoostsGiveawaysView::new(boosts_bundle);

    let mut boost_harness = Harness::builder()
        .with_size(Vec2::new(760.0, 760.0))
        .build_ui_state(
            move |ui, state: &mut (BoostsGiveawaysView, LiveSession)| {
                let (bv, be) = state;
                let _ = bv.show_with_payments_backend(ui, be, 1_765_000_000, &palette);
            },
            (boosts_view, live),
        );
    boost_harness.run_steps(2);

    // Step 1: click "Boost Channel (1 free slot)" -> opens confirmation panel, 0 applyBoost RPCs yet.
    boost_harness.get_by_label_contains("Boost Channel").click();
    boost_harness.run_steps(2);
    assert_eq!(boost_harness.state().0.pending_boost_slot, Some(2));
    assert_eq!(
        rpc.count_ctor(tl_fns::premium::ApplyBoost::CONSTRUCTOR_ID),
        0,
        "ApplyBoost must not fire until Confirm Boost is pressed"
    );

    // Step 2: press "Confirm Boost" -> sends exact `premium.applyBoost` and refreshes count to 45.
    rpc.enqueue_ok(
        tl_fns::premium::ApplyBoost::CONSTRUCTOR_ID,
        &tl_enums::premium::MyBoosts::MyBoosts(tl_types::premium::MyBoosts {
            my_boosts: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    enqueue_boosts_load(&rpc, 45, true);

    boost_harness.get_by_label("Confirm Boost").click();
    boost_harness.run_steps(2);

    assert_eq!(
        rpc.count_ctor(tl_fns::premium::ApplyBoost::CONSTRUCTOR_ID),
        1
    );
    let apply_req: tl_fns::premium::ApplyBoost =
        rpc.decode_last(tl_fns::premium::ApplyBoost::CONSTRUCTOR_ID);
    assert_eq!(apply_req.slots, Some(vec![2]));
    assert!(matches!(
        apply_req.peer,
        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel { channel_id, .. })
        if channel_id == ch.id
    ));
    assert!(boost_harness.state().0.pending_boost_slot.is_none());
    assert_eq!(
        boost_harness
            .state()
            .0
            .bundle
            .boost_status
            .overview
            .boost_count,
        45
    );
}

// =============================================================================
// Test 4: Auction Bid (`payments.sendStarsForm`), Giveaway Invoice (`payments.getPaymentForm`),
//         & Passport / Statistics / Settings Placeholder Cleanup
// =============================================================================

#[test]
fn wp50_auction_bid_giveaway_confirm_and_placeholder_cleanup() {
    let rpc = FakeRpcClient::new();
    let ch = synthetic_channel(2_000_606, "Audio Guild", "audio_guild");
    let (_runtime, live) = make_live_session(rpc.clone(), 500_004, vec![], vec![ch.clone()]);

    // --- Part A: Auction bid requires confirmation before `payments.getPaymentForm` + `sendStarsForm` ---
    let mut auctions_view = AuctionsView {
        snapshot: ActiveAuctionsSnapshot {
            auctions: vec![GiftAuctionState {
                gift: Some(StarGift {
                    id: 88_001,
                    resell_title: "Comet Crown".to_owned(),
                    stars: 900,
                    ..StarGift::default()
                }),
                my: StarGiftAuctionMyState {
                    bid: 900,
                    min_bid_amount: 950,
                    ..StarGiftAuctionMyState::default()
                },
                min_bid_amount: 950,
                start_date: 1_760_000_000,
                end_date: 1_800_000_000,
                next_round_at: 1_765_000_000,
                last_gift_num: 1,
                current_round: 2,
                total_rounds: 8,
                gifts_left: 10,
                version: 1,
                ..GiftAuctionState::default()
            }],
        },
        selected_index: 0,
        bid_input: "950".to_owned(),
        loaded: true,
        ..AuctionsView::default()
    };
    auctions_view.recipient.clear();

    let palette = Palette::day();
    let mut auction_harness = Harness::builder()
        .with_size(Vec2::new(720.0, 720.0))
        .build_ui_state(
            move |ui, state: &mut (AuctionsView, LiveSession)| {
                let (av, be) = state;
                let _ = av.show(ui, be, &palette);
            },
            (auctions_view, live),
        );
    auction_harness.run_steps(2);

    // Step 1: click Raise Bid button -> opens confirmation card, 0 RPCs fired yet!
    auction_harness
        .get_by_label_contains("Raise Bid to 950 Stars")
        .click();
    auction_harness.run_steps(2);
    assert_eq!(auction_harness.state().0.pending_bid_confirm, Some(950));
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID),
        0
    );

    // Step 2: press "Confirm 950 Stars" -> sends `payments.getPaymentForm` + `payments.sendStarsForm`.
    rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::StarGift(tl_types::payments::PaymentFormStarGift {
            form_id: 55_950,
            invoice: sample_invoice_tl("Auction Bid", 950, "XTR"),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentResult(tl_types::payments::PaymentResult {
            updates: empty_updates(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetStarGiftActiveAuctions::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftActiveAuctions::NotModified,
    );

    auction_harness.get_by_label("Confirm 950 Stars").click();
    auction_harness.run_steps(2);

    assert_eq!(
        rpc.count_ctor(tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID),
        1
    );
    let stars_req: tl_fns::payments::SendStarsForm =
        rpc.decode_last(tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID);
    assert_eq!(stars_req.form_id, 55_950);
    assert!(matches!(
        stars_req.invoice,
        tl_enums::InputInvoice::StarGiftAuctionBid(tl_types::InputInvoiceStarGiftAuctionBid {
            gift_id: 88_001,
            bid_amount: 950,
            update_bid: true,
            ..
        })
    ));

    // --- Part B: Giveaway invoice creation requires confirmation before `payments.getPaymentForm` ---
    let (_av, live) = auction_harness.into_state();
    let gw_panel = CreateGiveawayPanel {
        options: GiveawayOptionsBundle {
            channel_peer_id: DataPeerId::from_channel(ChannelId::from_tl(ch.id)),
            channel_title: ch.title.clone(),
            ..Default::default()
        },
        quantity: 5,
        months: 3,
        duration_days: 7,
        loaded: true,
        ..CreateGiveawayPanel::default()
    };
    let form_count_before = rpc.count_ctor(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i32;
    let draft = GiveawayDraft {
        boost_peer: DataPeerId::from_channel(ChannelId::from_tl(ch.id)),
        additional_peers: Vec::new(),
        countries_iso2: Vec::new(),
        prize: GiveawayPrizeKind::Premium { months: 3 },
        quantity: 5,
        until_date: now + 7 * 86_400,
        only_new_subscribers: false,
        winners_are_visible: true,
        prize_description: String::new(),
        prepaid_id: None,
    };
    assert!(draft.validate(now, &Default::default()).is_ok());

    let mut gw_harness = Harness::builder()
        .with_size(Vec2::new(720.0, 720.0))
        .build_ui_state(
            move |ui, state: &mut (CreateGiveawayPanel, LiveSession)| {
                let (gp, be) = state;
                let _ = gp.show(ui, "Audio Guild", now, be, &palette);
            },
            (gw_panel, live),
        );
    gw_harness.run_steps(2);

    gw_harness.get_by_label("Prepare Giveaway Invoice").click();
    gw_harness.run_steps(2);
    assert!(gw_harness.state().0.pending_confirm);
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID),
        form_count_before,
        "GetPaymentForm must not fire before confirmation"
    );

    rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &sample_payment_form_tl(
            66_500,
            700_099,
            "Channel Giveaway",
            "5x 3m Premium",
            "Giveaway",
            5_995,
            "USD",
            "https://checkout.example.test/giveaway",
            None,
        ),
    );
    gw_harness.get_by_label("Confirm & Prepare Invoice").click();
    gw_harness.run_steps(2);

    assert_eq!(
        rpc.count_ctor(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID),
        form_count_before + 1
    );
    assert_eq!(
        rpc.count_ctor(tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID),
        0,
        "Giveaway invoice preparation must be read-only and never auto-purchase"
    );

    // --- Part C: Leftover placeholder string cleanup verification ---
    let passport_bundle = MockPassportBackend::new()
        .load_authorization_form(SAMPLE_PASSPORT_BOT)
        .expect("load_authorization_form");
    let mut passport = PassportView::new(passport_bundle, true);
    passport.identity_draft.document_type = Some(ValueType::DriverLicense);
    passport.identity_draft.selfie_required = true;
    assert!(!passport.validate_identity_draft());
    let front_err = passport
        .identity_draft
        .errors
        .get("scans")
        .cloned()
        .expect("missing front scan error");
    assert!(
        !front_err.to_ascii_lowercase().contains("placeholder"),
        "front scan validation error must not say placeholder: {front_err}"
    );

    passport.identity_draft.scans.push("Front".to_owned());
    assert!(!passport.validate_identity_draft());
    let rev_err = passport
        .identity_draft
        .errors
        .get("scans")
        .cloned()
        .expect("missing reverse scan error");
    assert!(!rev_err.to_ascii_lowercase().contains("placeholder"));

    passport.identity_draft.scans.push("Reverse".to_owned());
    assert!(!passport.validate_identity_draft());
    let selfie_err = passport
        .identity_draft
        .errors
        .get("scans")
        .cloned()
        .expect("missing selfie scan error");
    assert!(!selfie_err.to_ascii_lowercase().contains("placeholder"));

    passport.address_draft.document_type = Some(ValueType::UtilityBill);
    assert!(!passport.validate_address_draft());
    let addr_err = passport
        .address_draft
        .errors
        .get("scans")
        .cloned()
        .expect("missing address scan error");
    assert!(!addr_err.to_ascii_lowercase().contains("placeholder"));

    for s in [
        settings_strings::PREMIUM_SUBTITLE,
        settings_strings::PREMIUM_OPEN_TOAST,
        stats_strings::GIVEAWAY_CREATE_READ_ONLY_NOTICE,
        stats_strings::WITHDRAW_STARS_READ_ONLY,
        stats_strings::BOT_WITHDRAWAL_READ_ONLY_NOTICE,
        stats_strings::TON_WITHDRAWAL_READ_ONLY_NOTICE,
        stats_strings::STARS_WITHDRAWAL_READ_ONLY_NOTICE,
        stats_strings::GIVEAWAY_READ_ONLY_NOTICE,
    ] {
        assert!(
            !s.to_ascii_lowercase().contains("placeholder"),
            "string must not contain placeholder wording: {s}"
        );
    }
}
