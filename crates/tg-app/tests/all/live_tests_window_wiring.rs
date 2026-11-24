//! WP-20 window entry-point wiring & live-host runtime binding tests (both live and demo modes).
//!
//! Verifies via unit checks and headless `egui_kittest` harnesses:
//! 1. Upload progress bars + cancel button in `HistoryWidget` (`pending_media_uploads` / `cancel_media_upload`).
//! 2. Payments & Business entry points in `SettingsMain` (Premium, My Stars, Send a Gift,
//!    Stars Affiliate, Telegram Business), `DeepLinkOverlayRequest::OpenPayments` forwarding
//!    (invoice deep link, passport deep link, unique-gift slug lookup), and clicking Invoice /
//!    PaidMedia message bubbles.
//! 3. Downloads reachable via Main Menu `"Downloads"` row and `Ctrl+J` shortcut (`InfoSection::Downloads`).
//! 4. `"Your Color"` row in Settings -> My Account opening `EditPeerColorBox` for the signed-in user.
//! 5. `"Checklist"` entry in compose attach menu (`open_create_todo_box`), channel `"Direct Messages"`
//!    top-bar button + `?direct` deep link (`open_monoforum_section`), and `"Export Invite Link"` in
//!    `settings/folders.rs` (`export_chatlist_invite`).
//! 6. Per-account runtime bindings in `MultiAccountWorkspaceHost`:
//!    `MediaOverlayWidget::sync_screenshot_protection(hwnd)`,
//!    `LiveSession::attach_proxy_tdata_path`, and `LiveSession::set_media_cache_dir`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType, SettingsProxy,
};
use tg_data::{
    ChatFilter, ExtendedMedia, InvoiceData, Message, MessageContent, MessageMedia,
    MsgId as DataMsgId, PaidMediaData, PeerId as DataPeerId, PhotoData, PhotoId, PhotoSize,
    TextWithEntities as DataText, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig, save_proxy_settings_to_tdata};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tokio::sync::broadcast;

use tg_app::boxes::BoxKind;
use tg_app::business::AppBusinessHost;
use tg_app::history::backend::{HistoryBackend, HistoryMessage, MediaUploadStatus, MessageExtra};
use tg_app::history::sections_model::LinkPreviewData;
use tg_app::info::InfoSection;
use tg_app::live::LiveSession;
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::live::multi_account::{MultiAccountWorkspaceHost, account_media_cache_dir};
use tg_app::live::upload::register_test_upload_for;
use tg_app::media::backend::MediaItemId;
use tg_app::notifications::manager::NotificationSystem;
use tg_app::payments::credits::StarsModal;
use tg_app::payments::star_gifts::StarGiftModal;
use tg_app::payments::{AppPaymentsHost, PaymentsScreen};
use tg_app::session_api::AppSession;
use tg_app::settings::SettingsSection;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::tray::MockTrayPlatform;
use tg_app::window::controller::WindowController;
use tg_app::window::section::{SectionShow, SettingsPage};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
pub(super) struct WiringFakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    invoked_ctors: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl WiringFakeRpc {
    pub(super) fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            invoked_ctors: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    pub(super) fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    pub(super) fn invoked_count(&self, ctor: u32) -> usize {
        self.invoked_ctors
            .lock()
            .expect("lock ctors")
            .iter()
            .filter(|&&c| c == ctor)
            .count()
    }
}

impl RpcClient for WiringFakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.invoked_ctors.lock().expect("lock ctors").push(ctor);

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
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
        last_name: Some("Tester".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
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
        verified: true,
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
        broadcast_messages_allowed: true,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 301 + 11),
        title: title.to_owned(),
        username: Some(username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: now_unix_i32() - 3_600,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(128),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn no_bootstrap_runtime_config() -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    }
}

fn make_demo_message(
    id: MsgId,
    peer_id: PeerId,
    from_id: PeerId,
    sender_name: &str,
    date_unix: i32,
    text: &str,
) -> HistoryMessage {
    HistoryMessage {
        id,
        peer_id,
        from_id,
        sender_name: sender_name.to_owned(),
        date_unix,
        outgoing: false,
        is_post: false,
        text: tg_text::TextWithEntities::simple(text),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

pub(super) struct LiveTestEnv {
    pub(super) workspace: LiveActiveWorkspace,
    pub(super) fake: WiringFakeRpc,
    pub(super) runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    pub(super) self_peer: PeerId,
    pub(super) alice_peer: PeerId,
    pub(super) channel_peer: PeerId,
    _tmp: TempDir,
}

impl LiveTestEnv {
    pub(super) fn boot(self_uid: i64, peer_uid: i64, channel_id: i64) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let fake = WiringFakeRpc::new(2);
        let self_user = synthetic_user(self_uid, "Self", "self_live", true);
        let alice_user = synthetic_user(peer_uid, "Alice", "alice_live", false);
        let channel = synthetic_channel(channel_id, "Live News", "live_news_chan");

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId(self_uid as u64),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
                no_bootstrap_runtime_config(),
            )
            .expect("start SessionRuntime"),
        );

        let alice_data_peer = DataPeerId::from_user(DataUserId(peer_uid as u64));
        let channel_data_peer = DataPeerId::from_channel(tg_data::ChannelId(channel_id as u64));
        runtime.tokio_handle().block_on(async {
            let mut s = runtime.session().write().await;
            tg_api::convert::apply_tl_users_and_chats(
                &mut s,
                &[
                    tl_enums::User::User(self_user),
                    tl_enums::User::User(alice_user),
                ],
                &[tl_enums::Chat::Channel(channel)],
            );
            s.insert_message(Message::new_text(
                alice_data_peer,
                DataMsgId(1),
                Some(alice_data_peer),
                now_unix_i32() - 60,
                DataText::plain("Hello in live chat"),
            ));
            s.insert_message(Message::new_text(
                channel_data_peer,
                DataMsgId(10),
                Some(channel_data_peer),
                now_unix_i32() - 30,
                DataText::plain("Channel announcement"),
            ));
            let mut folder = ChatFilter {
                id: 2,
                title: DataText::plain("Work"),
                ..ChatFilter::default()
            };
            folder.always.insert(alice_data_peer);
            s.set_filter(folder);
        });

        let live = LiveSession::new(Arc::clone(&runtime), self_uid as u64);
        let workspace = LiveActiveWorkspace::new_headless(
            live,
            ShellSettings::default(),
            tmp.path().join("settings.json"),
            false,
        );
        Self {
            workspace,
            fake,
            runtime,
            self_peer: PeerId::from_user(self_uid as u64),
            alice_peer: PeerId::from_user(peer_uid as u64),
            channel_peer: PeerId::from_channel(channel_id as u64),
            _tmp: tmp,
        }
    }

    pub(super) fn shutdown_runtime(runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>) {
        if let Ok(rt) = Arc::try_unwrap(runtime) {
            rt.shutdown();
        }
    }
}

fn build_demo_harness(ctrl: WindowController) -> Harness<'static, WindowController> {
    HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 1280.0))
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, state: &mut WindowController| {
                let ctx = ui.ctx().clone();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        state.show(ui);
                    });
                state.dispatch_demo_overlay_requests(&ctx);
                let palette = state.palette();
                let mode = state.theme_mode();
                AppPaymentsHost::update_and_render(&ctx, &palette, mode);
                AppBusinessHost::update_and_render(&ctx, &palette, mode);
            },
            ctrl,
        )
}

pub(super) fn build_live_harness(ws: LiveActiveWorkspace) -> Harness<'static, LiveActiveWorkspace> {
    HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 1280.0))
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, state: &mut LiveActiveWorkspace| {
                let ctx = ui.ctx().clone();
                let _ = state.show(&ctx, ui);
            },
            ws,
        )
}

fn make_demo_controller() -> (WindowController, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        tmp.path().join("demo_settings.json"),
    );
    (ctrl, tmp)
}

fn sample_tl_stars_status(balance: i64) -> tl_enums::payments::StarsStatus {
    tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
        balance: tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
            amount: balance,
            nanos: 0,
        }),
        subscriptions: None,
        subscriptions_next_offset: None,
        subscriptions_missing_balance: None,
        history: Some(Vec::new()),
        next_offset: None,
        chats: Vec::new(),
        users: Vec::new(),
    })
}

fn sample_tl_payment_form(title: &str) -> tl_enums::payments::PaymentForm {
    tl_enums::payments::PaymentForm::PaymentForm(tl_types::payments::PaymentForm {
        can_save_credentials: true,
        password_missing: false,
        form_id: 9001,
        bot_id: 770_050,
        title: title.to_owned(),
        description: "Test checkout description".to_owned(),
        photo: None,
        invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
            test: true,
            name_requested: false,
            phone_requested: false,
            email_requested: false,
            shipping_address_requested: false,
            flexible: false,
            phone_to_provider: false,
            email_to_provider: false,
            recurring: false,
            currency: "USD".to_owned(),
            prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                tl_types::LabeledPrice {
                    label: title.to_owned(),
                    amount: 500,
                },
            )],
            max_tip_amount: None,
            suggested_tip_amounts: None,
            terms_url: None,
            subscription_period: None,
        }),
        provider_id: 770_051,
        url: "https://payments.example.test/checkout".to_owned(),
        native_provider: Some("stripe".to_owned()),
        native_params: None,
        additional_methods: None,
        saved_info: None,
        saved_credentials: None,
        users: Vec::new(),
    })
}

fn sample_tl_unique_star_gift(slug: &str) -> tl_enums::payments::UniqueStarGift {
    tl_enums::payments::UniqueStarGift::UniqueStarGift(tl_types::payments::UniqueStarGift {
        gift: tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
            require_premium: false,
            resale_ton_only: false,
            theme_available: false,
            burned: false,
            crafted: false,
            id: 42_001,
            gift_id: 42_000,
            title: "Plush Pepe".to_owned(),
            slug: slug.to_owned(),
            num: 1,
            owner_id: None,
            owner_name: Some("Collector".to_owned()),
            owner_address: None,
            attributes: Vec::new(),
            availability_issued: 10,
            availability_total: 100,
            gift_address: None,
            resell_amount: None,
            released_by: None,
            value_amount: None,
            value_currency: None,
            value_usd_amount: None,
            theme_peer: None,
            peer_color: None,
            host_id: None,
            offer_min_stars: None,
            craft_chance_permille: None,
        }),
        chats: Vec::new(),
        users: Vec::new(),
    })
}

fn sample_tl_user_full(user_id: i64) -> tl_enums::users::UserFull {
    tl_enums::users::UserFull::UserFull(tl_types::users::UserFull {
        full_user: tl_enums::UserFull::UserFull(tl_types::UserFull {
            blocked: false,
            phone_calls_available: false,
            phone_calls_private: false,
            can_pin_message: true,
            has_scheduled: false,
            video_calls_available: false,
            voice_messages_forbidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            blocked_my_stories_from: false,
            wallpaper_overridden: false,
            contact_require_premium: false,
            read_dates_private: false,
            sponsored_enabled: true,
            can_view_revenue: false,
            bot_can_manage_emoji_status: false,
            display_gifts_button: false,
            noforwards_my_enabled: false,
            noforwards_peer_enabled: false,
            unofficial_security_risk: false,
            id: user_id,
            about: Some("Business bio".to_owned()),
            settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
                report_spam: false,
                add_contact: false,
                block_contact: false,
                share_contact: false,
                need_contacts_exception: false,
                report_geo: false,
                autoarchived: false,
                invite_members: false,
                request_chat_broadcast: false,
                business_bot_paused: false,
                business_bot_can_reply: false,
                geo_distance: None,
                request_chat_title: None,
                request_chat_date: None,
                business_bot_id: None,
                business_bot_manage_url: None,
                charge_paid_message_stars: None,
                registration_month: None,
                phone_country: None,
                name_change_date: None,
                photo_change_date: None,
            }),
            personal_photo: None,
            profile_photo: None,
            fallback_photo: None,
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
                    show_previews: Some(true),
                    silent: Some(false),
                    mute_until: Some(0),
                    ios_sound: None,
                    android_sound: None,
                    other_sound: None,
                    stories_muted: None,
                    stories_hide_sender: None,
                    stories_ios_sound: None,
                    stories_android_sound: None,
                    stories_other_sound: None,
                },
            ),
            bot_info: None,
            pinned_msg_id: None,
            common_chats_count: 0,
            folder_id: None,
            ttl_period: None,
            theme: None,
            private_forward_name: None,
            bot_group_admin_rights: None,
            bot_broadcast_admin_rights: None,
            wallpaper: None,
            stories: None,
            business_work_hours: None,
            business_location: None,
            business_greeting_message: None,
            business_away_message: None,
            business_intro: None,
            birthday: None,
            personal_channel_id: None,
            personal_channel_message: None,
            stargifts_count: None,
            starref_program: None,
            bot_verification: None,
            send_paid_messages_stars: None,
            disallowed_gifts: None,
            stars_rating: None,
            stars_my_pending_rating: None,
            stars_my_pending_rating_date: None,
            main_tab: None,
            saved_music: None,
            note: None,
            bot_manager_id: None,
            wallet_card: None,
            share_theme: None,
        }),
        chats: Vec::new(),
        users: vec![tl_enums::User::User(synthetic_user(
            user_id,
            "Self",
            "self_live",
            true,
        ))],
    })
}

fn enqueue_live_stars_and_gifts_responses(fake: &WiringFakeRpc) {
    for _ in 0..4 {
        fake.enqueue_ok(
            tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
            &sample_tl_stars_status(500),
        );
        fake.enqueue_ok(
            tl_fns::payments::GetStarsTopupOptions::CONSTRUCTOR_ID,
            &Vec::<tl_enums::StarsTopupOption>::new(),
        );
        fake.enqueue_ok(
            tl_fns::payments::GetStarGifts::CONSTRUCTOR_ID,
            &tl_enums::payments::StarGifts::StarGifts(tl_types::payments::StarGifts {
                hash: 1,
                gifts: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        fake.enqueue_ok(
            tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
            &tl_enums::payments::SavedStarGifts::SavedStarGifts(
                tl_types::payments::SavedStarGifts {
                    count: 0,
                    chat_notifications_enabled: None,
                    gifts: Vec::new(),
                    next_offset: None,
                    chats: Vec::new(),
                    users: Vec::new(),
                },
            ),
        );
        fake.enqueue_ok(
            tl_fns::help::GetPremiumPromo::CONSTRUCTOR_ID,
            &tl_enums::help::PremiumPromo::PremiumPromo(tl_types::help::PremiumPromo {
                status_text: "Telegram Premium".to_owned(),
                status_entities: Vec::new(),
                video_sections: Vec::new(),
                videos: Vec::new(),
                period_options: Vec::new(),
                users: Vec::new(),
            }),
        );
    }
}

// =============================================================================
// 1. Upload progress bar + cancel button (demo + live)
// =============================================================================

#[test]
fn upload_progress_bar_renders_and_cancels_in_demo_and_live_mode() {
    // Demo mode
    let (mut ctrl, _tmp) = make_demo_controller();
    let peer = session_peers::USER_MIRA;
    ctrl.session_mut().expect("session").show_peer_history(
        peer,
        SectionShow::clear_stack(),
        MsgId::SHOW_AT_END,
    );
    ctrl.session_handle()
        .push_pending_upload(MediaUploadStatus {
            upload_id: 77,
            peer_id: peer,
            file_name: "archive.zip".to_owned(),
            uploaded_bytes: 512 * 1024,
            total_bytes: 1024 * 1024,
            failed: false,
            cancelled: false,
        });

    let mut demo_harness = build_demo_harness(ctrl);
    demo_harness.step();
    demo_harness.step();
    assert_eq!(
        demo_harness
            .state()
            .session_backend()
            .pending_media_uploads(peer)
            .len(),
        1
    );
    demo_harness
        .get_by_label("Cancel upload archive.zip")
        .click();
    demo_harness.step();
    demo_harness.step();
    assert!(
        demo_harness
            .state()
            .session_backend()
            .pending_media_uploads(peer)
            .is_empty(),
        "demo cancel button must cancel pending upload"
    );

    // Live mode
    let env = LiveTestEnv::boot(720_001, 720_002, 720_003);
    let alice = env.alice_peer;
    let (_uid, cancel_token) = register_test_upload_for(
        env.workspace.live_session(),
        alice,
        "report.pdf",
        256 * 1024,
        1024 * 1024,
    );
    let mut ws = env.workspace;
    ws.controller_mut()
        .session_mut()
        .expect("session")
        .show_peer_history(alice, SectionShow::clear_stack(), MsgId::SHOW_AT_END);

    let mut live_harness = build_live_harness(ws);
    live_harness.step();
    live_harness.step();
    live_harness
        .get_by_label("Cancel upload report.pdf")
        .click();
    live_harness.step();
    live_harness.step();

    assert!(
        cancel_token.is_cancelled(),
        "live cancel button must trigger CancelToken"
    );
    assert!(
        live_harness
            .state()
            .live_session()
            .pending_media_uploads(alice)
            .iter()
            .all(|u| u.cancelled),
        "live upload status must be marked cancelled"
    );
    drop(live_harness);
    LiveTestEnv::shutdown_runtime(env.runtime);
}

// =============================================================================
// 2. Payments & Business SettingsMain rows, deep-link forwarding, and bubbles
// =============================================================================

#[test]
fn settings_main_payments_and_business_rows_reachable_in_demo_and_live_mode() {
    let expected_payments = [
        ("Telegram Premium", PaymentsScreen::Premium),
        ("My Stars", PaymentsScreen::Stars),
        ("Send a Gift", PaymentsScreen::StarGifts),
        ("Stars Affiliate", PaymentsScreen::Affiliate),
    ];

    // Demo mode
    for (label, expected_screen) in expected_payments {
        let (mut ctrl, _tmp) = make_demo_controller();
        ctrl.session_mut()
            .expect("session")
            .show_settings(SettingsPage::Main);
        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.step();
        harness.get_by_label(label).click();
        harness.step();
        harness.step();
        assert!(
            AppPaymentsHost::is_open(&harness.ctx),
            "demo SettingsMain row '{label}' should open Payments overlay"
        );
        assert_eq!(
            AppPaymentsHost::active_screen(&harness.ctx),
            Some(expected_screen),
            "demo SettingsMain row '{label}' should select {expected_screen:?}"
        );
    }

    {
        let (mut ctrl, _tmp) = make_demo_controller();
        ctrl.session_mut()
            .expect("session")
            .show_settings(SettingsPage::Main);
        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.step();
        harness.get_by_label("Telegram Business").click();
        harness.step();
        harness.step();
        assert!(
            AppBusinessHost::is_open(&harness.ctx),
            "demo SettingsMain row 'Telegram Business' should open Business overlay"
        );
    }

    // Live mode
    for (label, expected_screen) in expected_payments {
        let env = LiveTestEnv::boot(721_001, 721_002, 721_003);
        enqueue_live_stars_and_gifts_responses(&env.fake);
        let mut ws = env.workspace;
        ws.controller_mut()
            .session_mut()
            .expect("session")
            .show_settings(SettingsPage::Main);
        let mut harness = build_live_harness(ws);
        harness.step();
        harness.step();
        harness.get_by_label(label).click();
        harness.step();
        harness.step();
        assert!(
            harness.state().payments_open(),
            "live SettingsMain row '{label}' should open Payments overlay"
        );
        assert_eq!(
            harness.state().payments_host().map(|h| h.active_screen()),
            Some(expected_screen),
            "live SettingsMain row '{label}' should select {expected_screen:?}"
        );
        drop(harness);
        if let Ok(rt) = Arc::try_unwrap(env.runtime) {
            rt.shutdown();
        }
    }

    {
        let env = LiveTestEnv::boot(721_101, 721_102, 721_103);
        env.fake.enqueue_ok(
            tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
            &sample_tl_user_full(721_101),
        );
        let mut ws = env.workspace;
        ws.controller_mut()
            .session_mut()
            .expect("session")
            .show_settings(SettingsPage::Main);
        let mut harness = build_live_harness(ws);
        harness.step();
        harness.step();
        harness.get_by_label("Telegram Business").click();
        harness.step();
        harness.step();
        assert!(
            harness.state().business_open(),
            "live SettingsMain row 'Telegram Business' should open Business overlay"
        );
        drop(harness);
        if let Ok(rt) = Arc::try_unwrap(env.runtime) {
            rt.shutdown();
        }
    }
}

#[test]
fn deep_link_payments_forwarding_invoice_passport_and_unique_gift_in_live_and_demo() {
    // Live mode: invoice deep link, passport deep link, and unique-gift slug lookup.
    let env = LiveTestEnv::boot(722_001, 722_002, 722_003);
    enqueue_live_stars_and_gifts_responses(&env.fake);
    env.fake.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &sample_tl_payment_form("Rust Handbook"),
    );
    env.fake.enqueue_ok(
        tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID,
        &tl_enums::account::AuthorizationForm::AuthorizationForm(
            tl_types::account::AuthorizationForm {
                required_types: Vec::new(),
                values: Vec::new(),
                errors: Vec::new(),
                users: Vec::new(),
                privacy_policy_url: None,
            },
        ),
    );
    env.fake.enqueue_ok(
        tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID,
        &sample_tl_unique_star_gift("PlushPepe-1"),
    );

    let ws = env.workspace;
    let fake = env.fake.clone();
    let mut harness = build_live_harness(ws);

    // 1. Invoice deep link
    harness
        .state_mut()
        .controller_mut()
        .open_link("tg://invoice?slug=live_inv_42");
    harness.step();
    assert!(harness.state().payments_open());
    assert_eq!(
        harness
            .state()
            .payments_host()
            .and_then(|h| h.active_invoice_target()),
        Some("live_inv_42")
    );
    assert_eq!(
        fake.invoked_count(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID),
        1
    );

    // 2. Passport deep link (legacy `scope=id` carries `payload=` nonce per tdesktop)
    harness
        .state_mut()
        .controller_mut()
        .open_link("tg://passport?bot_id=722099&scope=id&public_key=PUBKEY&payload=n42");
    harness.step();
    assert_eq!(
        harness.state().payments_host().map(|h| h.active_screen()),
        Some(PaymentsScreen::Passport)
    );
    assert_eq!(
        fake.invoked_count(tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID),
        1
    );

    // 3. Unique gift slug lookup (`t.me/nft/PlushPepe-1`)
    harness
        .state_mut()
        .controller_mut()
        .open_link("https://t.me/nft/PlushPepe-1");
    harness.step();
    assert_eq!(
        harness.state().payments_host().map(|h| h.active_screen()),
        Some(PaymentsScreen::StarGifts)
    );
    assert!(
        harness
            .state_mut()
            .payments_host_mut()
            .and_then(|h| h.gifts_view_mut())
            .is_some_and(|gv| matches!(gv.modal, Some(StarGiftModal::UniqueSlugDetail(_)))),
        "live unique gift deep link must open UniqueSlugDetail modal"
    );
    assert_eq!(
        fake.invoked_count(tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID),
        1
    );

    drop(harness);
    if let Ok(rt) = Arc::try_unwrap(env.runtime) {
        rt.shutdown();
    }

    // Demo mode
    let (ctrl, _tmp) = make_demo_controller();
    let mut demo = build_demo_harness(ctrl);
    demo.state_mut().open_link("tg://invoice?slug=demo_inv_99");
    demo.step();
    assert!(AppPaymentsHost::is_open(&demo.ctx));
    assert_eq!(
        AppPaymentsHost::active_screen(&demo.ctx),
        Some(PaymentsScreen::Checkout)
    );
    assert_eq!(
        AppPaymentsHost::active_invoice_target(&demo.ctx).as_deref(),
        Some("demo_inv_99")
    );

    demo.state_mut()
        .open_link("https://t.me/nft/CrystalPrism-42");
    demo.step();
    assert_eq!(
        AppPaymentsHost::active_screen(&demo.ctx),
        Some(PaymentsScreen::StarGifts)
    );
    assert!(AppPaymentsHost::has_unique_gift_modal_open(&demo.ctx));
}

#[test]
fn clicking_invoice_and_paid_media_bubbles_opens_checkout_and_unlock_in_demo_and_live() {
    // Demo mode
    {
        let (mut ctrl, _tmp) = make_demo_controller();
        let peer = session_peers::USER_MIRA;
        let mut inv_msg = make_demo_message(
            MsgId(901),
            peer,
            peer,
            "Mira Vance",
            now_unix_i32() - 20,
            "Pro License",
        );
        inv_msg.extra.link_preview = Some(LinkPreviewData {
            url: "tg://invoice?slug=bubble_inv_1".to_owned(),
            site_name: "Invoice".to_owned(),
            title: "Pro License — $5.00".to_owned(),
            description: "Annual plan".to_owned(),
            thumb_rgb: None,
            large_media: false,
            above_text: false,
        });
        let mut paid_msg = make_demo_message(
            MsgId(902),
            peer,
            peer,
            "Mira Vance",
            now_unix_i32() - 10,
            "Exclusive drop",
        );
        paid_msg.extra.link_preview = Some(LinkPreviewData {
            url: format!("paid_media:{}:902:50", peer.0),
            site_name: "Paid Media".to_owned(),
            title: "Unlock for 50 ⭐".to_owned(),
            description: "1 paid media item(s)".to_owned(),
            thumb_rgb: None,
            large_media: false,
            above_text: false,
        });
        ctrl.session_handle().push_history_message(peer, inv_msg);
        ctrl.session_handle().push_history_message(peer, paid_msg);
        ctrl.session_mut().expect("session").show_peer_history(
            peer,
            SectionShow::clear_stack(),
            MsgId::SHOW_AT_END,
        );

        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.step();
        harness.get_by_label("Pay: Pro License — $5.00").click();
        harness.step();
        harness.step();
        assert!(AppPaymentsHost::is_open(&harness.ctx));
        assert_eq!(
            AppPaymentsHost::active_screen(&harness.ctx),
            Some(PaymentsScreen::Checkout)
        );
        assert_eq!(
            AppPaymentsHost::active_invoice_target(&harness.ctx).as_deref(),
            Some("bubble_inv_1")
        );

        // Close overlay, then click Paid Media unlock bubble.
        AppPaymentsHost::close(&harness.ctx);
        for _ in 0..8 {
            harness.step();
        }
        harness.get_by_label("Unlock for 50 Stars").click();
        harness.step();
        harness.step();
        assert!(AppPaymentsHost::is_open(&harness.ctx));
        assert_eq!(
            AppPaymentsHost::active_screen(&harness.ctx),
            Some(PaymentsScreen::Stars)
        );
        assert!(AppPaymentsHost::has_paid_media_modal_open(&harness.ctx));
    }

    // Live mode
    {
        let env = LiveTestEnv::boot(723_001, 723_002, 723_003);
        enqueue_live_stars_and_gifts_responses(&env.fake);
        env.fake.enqueue_ok(
            tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
            &sample_tl_payment_form("Live Sticker Pack"),
        );

        let alice_data = DataPeerId::from_user(DataUserId(723_002));
        env.runtime.tokio_handle().block_on(async {
            let mut s = env.runtime.session().write().await;
            let mut inv = Message::new_text(
                alice_data,
                DataMsgId(20),
                Some(alice_data),
                now_unix_i32() - 15,
                DataText::plain(""),
            );
            inv.content = MessageContent::Regular {
                text: DataText::plain(""),
                media: MessageMedia::Invoice(Box::new(InvoiceData {
                    title: "Live Sticker Pack".to_owned(),
                    description: "Pack of 20".to_owned(),
                    currency: "USD".to_owned(),
                    total_amount: 499,
                    start_param: "live_bubble_slug".to_owned(),
                    test: true,
                    shipping_address_requested: false,
                    receipt_msg_id: None,
                    extended_media: None,
                })),
            };
            s.insert_message(inv);

            let mut paid = Message::new_text(
                alice_data,
                DataMsgId(21),
                Some(alice_data),
                now_unix_i32() - 5,
                DataText::plain(""),
            );
            paid.content = MessageContent::Regular {
                text: DataText::plain(""),
                media: MessageMedia::PaidMedia(Box::new(PaidMediaData {
                    stars_amount: 75,
                    extended_media: vec![ExtendedMedia::Preview {
                        width: Some(320),
                        height: Some(240),
                        thumb: None,
                        duration: None,
                    }],
                })),
            };
            s.insert_message(paid);
        });

        let fake = env.fake.clone();
        let alice = env.alice_peer;
        let mut ws = env.workspace;
        ws.controller_mut()
            .session_mut()
            .expect("session")
            .show_peer_history(alice, SectionShow::clear_stack(), MsgId::SHOW_AT_END);

        let mut harness = build_live_harness(ws);
        harness.step();
        harness.step();
        harness
            .get_by_label("Pay: Live Sticker Pack — USD 4.99")
            .click();
        harness.step();
        harness.step();
        assert!(harness.state().payments_open());
        assert_eq!(
            harness
                .state()
                .payments_host()
                .and_then(|h| h.active_invoice_target()),
            Some("live_bubble_slug")
        );
        assert_eq!(
            fake.invoked_count(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID),
            1
        );

        // Dismiss overlay and click the Paid Media unlock button.
        harness.state_mut().close_payments();
        harness.step();
        harness.step();
        harness.get_by_label("Unlock for 75 Stars").click();
        harness.step();
        harness.step();
        assert!(harness.state().payments_open());
        assert_eq!(
            harness.state().payments_host().map(|h| h.active_screen()),
            Some(PaymentsScreen::Stars)
        );
        assert!(
            harness
                .state_mut()
                .payments_host_mut()
                .and_then(|h| h.stars_view_mut())
                .is_some_and(|sv| matches!(sv.modal, Some(StarsModal::PaidMediaConfirm))),
            "live paid media bubble click must open StarsModal::PaidMediaConfirm"
        );
        drop(harness);
        if let Ok(rt) = Arc::try_unwrap(env.runtime) {
            rt.shutdown();
        }
    }
}

// =============================================================================
// 3. Downloads: Main Menu row + Ctrl+J shortcut (demo + live)
// =============================================================================

#[test]
fn downloads_reachable_via_main_menu_and_ctrl_j_in_demo_and_live_mode() {
    // Demo mode — Main Menu click
    {
        let (mut ctrl, _tmp) = make_demo_controller();
        ctrl.open_main_menu();
        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.step();
        harness.get_by_label("Downloads").click();
        harness.step();
        harness.step();
        assert!(
            matches!(
                harness.state().main_widget().main_info().current_section(),
                InfoSection::Downloads { .. }
            ),
            "Main Menu Downloads click must open InfoSection::Downloads in demo mode"
        );
    }

    // Demo mode — Ctrl+J shortcut
    {
        let (ctrl, _tmp) = make_demo_controller();
        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.key_press_modifiers(Modifiers::CTRL, Key::J);
        harness.step();
        harness.step();
        assert!(
            matches!(
                harness.state().main_widget().main_info().current_section(),
                InfoSection::Downloads { .. }
            ),
            "Ctrl+J must open InfoSection::Downloads in demo mode"
        );
    }

    // Live mode — Main Menu click & Ctrl+J
    {
        let env = LiveTestEnv::boot(724_001, 724_002, 724_003);
        let mut ws = env.workspace;
        ws.controller_mut().open_main_menu();
        let mut harness = build_live_harness(ws);
        harness.step();
        harness.step();
        harness.get_by_label("Downloads").click();
        harness.step();
        harness.step();
        assert!(
            matches!(
                harness
                    .state()
                    .controller()
                    .main_widget()
                    .main_info()
                    .current_section(),
                InfoSection::Downloads { .. }
            ),
            "Main Menu Downloads click must open InfoSection::Downloads in live mode"
        );
        // Navigate back, then verify Ctrl+J also opens Downloads in live mode.
        harness.key_press(Key::Escape);
        harness.step();
        harness.key_press_modifiers(Modifiers::CTRL, Key::J);
        harness.step();
        harness.step();
        assert!(
            matches!(
                harness
                    .state()
                    .controller()
                    .main_widget()
                    .main_info()
                    .current_section(),
                InfoSection::Downloads { .. }
            ),
            "Ctrl+J must open InfoSection::Downloads in live mode"
        );
        drop(harness);
        if let Ok(rt) = Arc::try_unwrap(env.runtime) {
            rt.shutdown();
        }
    }
}

// =============================================================================
// 4. Profile colour: "Your Color" row in Settings -> My Account (demo + live)
// =============================================================================

#[test]
fn your_color_row_opens_edit_peer_color_box_for_self_in_demo_and_live_mode() {
    // Demo mode
    {
        let (mut ctrl, _tmp) = make_demo_controller();
        ctrl.session_mut()
            .expect("session")
            .show_settings(SettingsPage::Account);
        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.step();
        harness.get_by_label("Your Color").click();
        harness.step();
        harness.step();
        match harness.state().box_host().top_kind() {
            Some(BoxKind::EditPeerColor(st)) => {
                assert_eq!(st.peer_id, session_peers::SELF);
            }
            other => panic!("expected EditPeerColor box in demo mode, got {other:?}"),
        }
    }

    // Live mode
    {
        let env = LiveTestEnv::boot(725_001, 725_002, 725_003);
        let self_peer = env.self_peer;
        let mut ws = env.workspace;
        ws.controller_mut()
            .session_mut()
            .expect("session")
            .show_settings(SettingsPage::Account);
        let mut harness = build_live_harness(ws);
        harness.step();
        harness.step();
        harness.get_by_label("Your Color").click();
        harness.step();
        harness.step();
        match harness.state().controller().box_host().top_kind() {
            Some(BoxKind::EditPeerColor(st)) => {
                assert_eq!(st.peer_id, self_peer);
            }
            other => panic!("expected EditPeerColor box in live mode, got {other:?}"),
        }
        drop(harness);
        if let Ok(rt) = Arc::try_unwrap(env.runtime) {
            rt.shutdown();
        }
    }
}

// =============================================================================
// 5. Checklists attach entry, Channel DMs button + ?direct link, Folder invite export
// =============================================================================

#[test]
fn checklist_channel_dms_and_folder_invite_export_work_in_demo_and_live_mode() {
    // Demo mode
    {
        let (mut ctrl, _tmp) = make_demo_controller();
        ctrl.session_mut().expect("session").show_peer_history(
            session_peers::USER_MIRA,
            SectionShow::clear_stack(),
            MsgId::SHOW_AT_END,
        );
        let mut harness = build_demo_harness(ctrl);
        harness.step();
        harness.step();

        // 5a. Compose attach menu -> Checklist
        harness.get_by_label("Attach").click();
        harness.step();
        harness.step();
        harness.get_by_label("Checklist").click();
        harness.step();
        harness.step();
        assert!(
            harness.state().main_widget().history().is_todo_box_open(),
            "clicking Checklist in demo attach menu must open EditTodoListBox"
        );
        harness.key_press(Key::Escape);
        harness.step();

        // 5b. Channel Direct Messages top-bar button + `?direct` deep link
        harness
            .state_mut()
            .session_mut()
            .expect("session")
            .show_peer_history(
                session_peers::CHANNEL_DEV_LOG,
                SectionShow::clear_stack(),
                MsgId::SHOW_AT_END,
            );
        harness.step();
        harness.step();
        harness.get_by_label("Direct Messages").click();
        harness.step();
        harness.step();
        assert!(
            harness.state().main_widget().history().is_monoforum_open(),
            "clicking Direct Messages button in demo channel must open monoforum section"
        );
        harness
            .state_mut()
            .main_widget_mut()
            .history_mut()
            .close_monoforum_section();
        assert!(!harness.state().main_widget().history().is_monoforum_open());

        harness
            .state_mut()
            .open_link("https://t.me/rustgram_log?direct");
        harness.step();
        assert!(
            harness.state().main_widget().history().is_monoforum_open(),
            "`?direct` deep link must open monoforum section in demo mode"
        );

        // 5c. Folder Export Invite Link
        let settings_backend = harness.state().session_backend().create_settings_backend();
        let embedded = harness.state().theme_manager().embedded_type();
        let accent = harness.state().theme_manager().accent();
        harness.state_mut().main_widget_mut().open_settings_section(
            SettingsSection::Folders,
            settings_backend,
            embedded,
            accent,
        );
        harness
            .state_mut()
            .session_mut()
            .expect("session")
            .show_settings(SettingsPage::Main);
        for _ in 0..40 {
            harness.step();
            if !harness.state().is_animating() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        harness.step();
        harness.get_by_label("Export Invite Link: Work").click();
        harness.step();
        harness.step();
        let exported = harness
            .state()
            .main_widget()
            .settings()
            .and_then(|s| s.folders.last_exported_link.clone());
        assert!(
            exported
                .as_ref()
                .is_some_and(|(_, url)| url.starts_with("https://t.me/addlist/")),
            "demo folder invite export should produce a t.me/addlist link, got {exported:?}"
        );
    }

    // Live mode
    {
        let env = LiveTestEnv::boot(726_001, 726_002, 726_003);
        let alice = env.alice_peer;
        let channel = env.channel_peer;
        let fake = env.fake.clone();
        let mut ws = env.workspace;
        ws.controller_mut()
            .session_mut()
            .expect("session")
            .show_peer_history(alice, SectionShow::clear_stack(), MsgId::SHOW_AT_END);

        let mut harness = build_live_harness(ws);
        harness.step();
        harness.step();

        // 5a. Compose attach menu -> Checklist
        harness.get_by_label("Attach").click();
        harness.step();
        harness.step();
        harness.get_by_label("Checklist").click();
        harness.step();
        harness.step();
        assert!(
            harness
                .state()
                .controller()
                .main_widget()
                .history()
                .is_todo_box_open(),
            "clicking Checklist in live attach menu must open EditTodoListBox"
        );
        harness.key_press(Key::Escape);
        harness.step();

        // 5b. Channel Direct Messages top-bar button + `?direct` deep link
        harness
            .state_mut()
            .controller_mut()
            .session_mut()
            .expect("session")
            .show_peer_history(channel, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
        harness.step();
        harness.step();
        harness.get_by_label("Direct Messages").click();
        harness.step();
        harness.step();
        assert!(
            harness
                .state()
                .controller()
                .main_widget()
                .history()
                .is_monoforum_open(),
            "clicking Direct Messages button in live channel must open monoforum section"
        );
        harness
            .state_mut()
            .controller_mut()
            .main_widget_mut()
            .history_mut()
            .close_monoforum_section();

        harness
            .state_mut()
            .controller_mut()
            .open_link("https://t.me/live_news_chan?direct");
        harness.step();
        assert!(
            harness
                .state()
                .controller()
                .main_widget()
                .history()
                .is_monoforum_open(),
            "`?direct` deep link must open monoforum section in live mode"
        );

        // 5c. Folder Export Invite Link -> `chatlists.exportChatlistInvite`
        fake.enqueue_ok(
            tl_fns::chatlists::ExportChatlistInvite::CONSTRUCTOR_ID,
            &tl_enums::chatlists::ExportedChatlistInvite::ExportedChatlistInvite(
                tl_types::chatlists::ExportedChatlistInvite {
                    filter: tl_enums::DialogFilter::Chatlist(tl_types::DialogFilterChatlist {
                        has_my_invites: true,
                        title_noanimate: false,
                        id: 2,
                        title: tl_enums::TextWithEntities::TextWithEntities(
                            tl_types::TextWithEntities {
                                text: "Work".to_owned(),
                                entities: Vec::new(),
                            },
                        ),
                        emoticon: None,
                        color: None,
                        pinned_peers: Vec::new(),
                        include_peers: Vec::new(),
                    }),
                    invite: tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
                        tl_types::ExportedChatlistInvite {
                            title: "Work".to_owned(),
                            url: "https://t.me/addlist/live_work_slug".to_owned(),
                            peers: Vec::new(),
                        },
                    ),
                },
            ),
        );
        let settings_backend = harness.state().live_session().create_settings_backend();
        let embedded = harness.state().controller().theme_manager().embedded_type();
        let accent = harness.state().controller().theme_manager().accent();
        harness
            .state_mut()
            .controller_mut()
            .main_widget_mut()
            .open_settings_section(SettingsSection::Folders, settings_backend, embedded, accent);
        harness
            .state_mut()
            .controller_mut()
            .session_mut()
            .expect("session")
            .show_settings(SettingsPage::Main);
        for _ in 0..40 {
            harness.step();
            if !harness.state().controller().is_animating() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        harness.step();
        harness.get_by_label("Export Invite Link: Work").click();
        harness.step();
        harness.step();
        assert_eq!(
            fake.invoked_count(tl_fns::chatlists::ExportChatlistInvite::CONSTRUCTOR_ID),
            1
        );
        let exported = harness
            .state()
            .controller()
            .main_widget()
            .settings()
            .and_then(|s| s.folders.last_exported_link.clone());
        assert_eq!(
            exported,
            Some((2, "https://t.me/addlist/live_work_slug".to_owned()))
        );

        drop(harness);
        if let Ok(rt) = Arc::try_unwrap(env.runtime) {
            rt.shutdown();
        }
    }
}

// =============================================================================
// 6. Live-host runtime bindings per account (tdata proxy, media cache, HWND protection)
// =============================================================================

#[test]
fn multi_account_host_binds_proxy_tdata_media_cache_and_screenshot_protection_per_account() {
    let tmp = TempDir::new().expect("tempdir");
    let tdata = tmp.path().join("tdata");
    std::fs::create_dir_all(&tdata).expect("create tdata dir");

    // Pre-persist a proxy into `tdata` so we verify `attach_proxy_tdata_path` actually loads it.
    let mut persisted_proxy = SettingsProxy::new();
    let sample_proxy = CoreProxyData {
        proxy_type: ProxyType::Socks5,
        host: "127.0.0.1".to_owned(),
        port: 1080,
        user: String::new(),
        password: String::new(),
    };
    persisted_proxy.add_to_list(sample_proxy.clone());
    persisted_proxy.set_selected(sample_proxy);
    persisted_proxy.set_settings(ProxySettingsMode::Enabled);
    save_proxy_settings_to_tdata(&tdata, &persisted_proxy).expect("save proxy to tdata");

    let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        tmp.path().join("settings.json"),
        NotificationSystem::headless(
            Default::default(),
            Default::default(),
            tmp.path().join("userpics"),
        ),
        Box::new(MockTrayPlatform::default()),
    );
    host.set_tdata_dir(tdata.clone());

    let fake0 = WiringFakeRpc::new(2);
    let rt0 = Arc::new(
        SessionRuntime::start(
            DataUserId(730_001),
            tg_app::live::ErasedRpcClient::new(fake0),
            no_bootstrap_runtime_config(),
        )
        .expect("rt0"),
    );
    let fake1 = WiringFakeRpc::new(2);
    let rt1 = Arc::new(
        SessionRuntime::start(
            DataUserId(730_002),
            tg_app::live::ErasedRpcClient::new(fake1),
            no_bootstrap_runtime_config(),
        )
        .expect("rt1"),
    );

    // Seed a protected (`noforwards`) channel photo in account 0 to test screenshot protection sync.
    let protected_chan_id = 730_050i64;
    let protected_peer = DataPeerId::from_channel(tg_data::ChannelId(protected_chan_id as u64));
    let photo_id = 880_001u64;
    rt0.tokio_handle().block_on(async {
        let mut s = rt0.session().write().await;
        let mut ch = synthetic_channel(protected_chan_id, "Protected Channel", "protected_ch");
        ch.noforwards = true;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(synthetic_user(
                730_001, "Acc0", "acc0_u", true,
            ))],
            &[tl_enums::Chat::Channel(ch)],
        );
        let mut msg = Message::new_text(
            protected_peer,
            DataMsgId(5),
            Some(protected_peer),
            now_unix_i32(),
            DataText::plain("Protected photo"),
        );
        msg.flags.insert(tg_data::MessageFlags::NO_FORWARDS);
        msg.content = MessageContent::Regular {
            text: DataText::plain("Protected photo"),
            media: MessageMedia::Photo(Box::new(PhotoData {
                id: PhotoId(photo_id),
                access_hash: 1234,
                file_reference: vec![1, 2, 3],
                date: now_unix_i32(),
                dc_id: 2,
                has_stickers: false,
                sizes: vec![PhotoSize {
                    kind: "y".to_owned(),
                    width: 200,
                    height: 150,
                    size: 4,
                }],
                ..PhotoData::default()
            })),
        };
        s.insert_message(msg);
    });
    rt1.tokio_handle().block_on(async {
        let mut s = rt1.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(synthetic_user(
                730_002, "Acc1", "acc1_u", true,
            ))],
            &[],
        );
    });

    host.add_account(0, DataUserId(730_001), false, false, rt0, None, true, false);
    host.add_account(
        1,
        DataUserId(730_002),
        false,
        false,
        rt1,
        None,
        false,
        false,
    );

    // Verify per-account `attach_proxy_tdata_path` and `set_media_cache_dir` inside `tdata`.
    let acc0_session = host.account(0).expect("acc0").workspace.live_session();
    let acc1_session = host.account(1).expect("acc1").workspace.live_session();

    assert_eq!(
        acc0_session.proxy_controller().tdata_path(),
        Some(tdata.clone())
    );
    assert_eq!(
        acc1_session.proxy_controller().tdata_path(),
        Some(tdata.clone())
    );
    assert_eq!(
        acc0_session.proxy_controller().proxy_settings().settings(),
        ProxySettingsMode::Enabled,
        "account 0 should have loaded persisted proxy settings from tdata"
    );
    assert_eq!(
        acc1_session.proxy_controller().proxy_settings().settings(),
        ProxySettingsMode::Enabled,
        "account 1 should have loaded persisted proxy settings from tdata"
    );

    let expected_cache0 = account_media_cache_dir(&tdata, 0);
    let expected_cache1 = account_media_cache_dir(&tdata, 1);
    assert_eq!(acc0_session.media_cache().root_dir(), expected_cache0);
    assert_eq!(acc1_session.media_cache().root_dir(), expected_cache1);
    assert_ne!(expected_cache0, expected_cache1);
    assert!(expected_cache0.starts_with(&tdata));
    assert!(expected_cache1.starts_with(&tdata));

    // Verify `MediaOverlayWidget::sync_screenshot_protection(hwnd)` wiring on account 0 + account switch.
    let fake_hwnd: isize = 0; // Safe cross-platform HWND (non-zero Win32 HWNDs require a real OS window).
    host.set_native_hwnd(fake_hwnd);
    {
        let ctrl0 = host
            .account_mut(0)
            .expect("acc0")
            .workspace
            .controller_mut();
        let backend_handle = ctrl0
            .session_backend_as::<LiveSession>()
            .expect("live session")
            .handle();
        ctrl0
            .media_overlay_mut()
            .open_item(
                &backend_handle,
                protected_peer.0,
                MediaItemId(photo_id),
                1_000,
            )
            .expect("open protected photo");
        assert!(
            ctrl0.media_overlay().content_needs_screenshot_protection(),
            "noforwards channel photo must request screenshot protection"
        );
        ctrl0.sync_screenshot_protection(fake_hwnd);
    }

    // Switching to account 1 clears slot 0's native HWND binding and binds slot 1.
    assert!(host.switch_to(1));
    assert_eq!(host.active_slot(), 1);
    assert!(
        !host
            .account(1)
            .expect("acc1")
            .workspace
            .controller()
            .media_overlay()
            .content_needs_screenshot_protection()
    );

    // Closing overlay in slot 0 clears screenshot protection requirement cleanly.
    assert!(host.switch_to(0));
    {
        let ctrl0 = host
            .account_mut(0)
            .expect("acc0")
            .workspace
            .controller_mut();
        ctrl0.media_overlay_mut().close();
        ctrl0.sync_screenshot_protection(fake_hwnd);
        assert!(!ctrl0.media_overlay().content_needs_screenshot_protection());
    }

    // Shut down both accounts cleanly.
    let _ = host.remove_account(0);
    let _ = host.remove_account(1);
    let _ = TEST_TIMEOUT;
}
