//! Live-mode deep link routing and media-viewer action tests over [`FakeRpcClient`].
//!
//! Verifies end-to-end TL RPC dispatch for `contacts.resolveUsername`, `contacts.resolvePhone`,
//! `messages.checkChatInvite` / `importChatInvite`, `chatlists.checkChatlistInvite` /
//! `joinChatlistInvite`, `messages.getStickerSet`, `account.getTheme`, `account.getWallPaper`,
//! `account.sendConfirmPhoneCode`, `payments.checkGiftCode`, `payments.getUniqueStarGift`,
//! `account.resolveBusinessChatLink`, `help.getDeepLinkInfo`, live Instant View (`messages.getWebPage`),
//! `MediaBackend::forward_item`, and `MediaBackend::open_in_external_app`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::boxes::BoxKind;
use tg_app::boxes::confirm::ConfirmTag;
use tg_app::live::LiveSession;
use tg_app::live::media::launch_external_file_with;
use tg_app::media::backend::MediaItemId;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::controller::{WebViewHostRequest, WindowController};
use tg_app::window::deep_links::DeepLinkOverlayRequest;
use tg_app::window::section::{SectionMemento, SettingsPage};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct DeepLinkFakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    invoked_ctors: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl DeepLinkFakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            invoked_ctors: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn invoked_count(&self, ctor: u32) -> usize {
        self.invoked_ctors
            .lock()
            .expect("lock ctors")
            .iter()
            .filter(|&&c| c == ctor)
            .count()
    }
}

impl RpcClient for DeepLinkFakeRpc {
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
        premium: false,
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
        access_hash: Some(id * 100 + 3),
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

fn synthetic_channel(id: i64, title: &str, username: Option<&str>) -> tl_types::Channel {
    tl_types::Channel {
        creator: false,
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
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 17 + 9),
        title: title.to_owned(),
        username: username.map(ToOwned::to_owned),
        photo: tl_enums::ChatPhoto::Empty,
        date: now_unix_i32(),
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

fn boot_live_controller() -> (
    WindowController,
    DeepLinkFakeRpc,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    TempDir,
) {
    let now = now_unix_i32();
    let self_uid = 710_001i64;
    let fake = DeepLinkFakeRpc::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_user(
            self_uid,
            "Alice",
            "alice_self",
            true,
        ))],
    );
    fake.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 1,
            qts: 0,
            date: now,
            seq: 1,
            unread_count: 0,
        }),
    );
    fake.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: now,
            seq: 1,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 1,
                qts: 0,
                date: now,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_uid),
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("startup ready");

    let live_session = LiveSession::new(Arc::clone(&runtime), 1);
    let temp = TempDir::new().expect("tempdir");
    let ctrl = WindowController::with_session(
        live_session,
        ShellSettings::default(),
        temp.path().join("settings.json"),
    );
    (ctrl, fake, runtime, temp)
}

fn shutdown_runtime(
    ctrl: WindowController,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
) {
    drop(ctrl);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

#[test]
fn live_resolve_username_and_phone_route_to_chat_and_prefill_text() {
    let (mut ctrl, fake, runtime, _tmp) = boot_live_controller();

    // 1. Resolve uncached username via `contacts.resolveUsername` -> opens chat + jumps to post + prefills text.
    let dave_uid = 710_042i64;
    let dave = synthetic_user(dave_uid, "Dave", "dave_live", false);
    fake.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: dave_uid }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(dave)],
        }),
    );

    ctrl.open_link("https://t.me/dave_live/99?text=Hello%20Dave");
    assert_eq!(
        ctrl.session().expect("session").active_peer(),
        PeerId::from_user(dave_uid as u64)
    );
    assert_eq!(ctrl.session().expect("session").active_msg_id(), MsgId(99));
    assert_eq!(
        ctrl.main_widget_mut().history_mut().compose_mut().text,
        "Hello Dave"
    );
    assert_eq!(
        fake.invoked_count(tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID),
        1
    );

    // 2. Resolve phone via `contacts.resolvePhone`.
    let eva_uid = 710_055i64;
    let eva = synthetic_user(eva_uid, "Eva", "eva_live", false);
    fake.enqueue_ok(
        tl_fns::contacts::ResolvePhone::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: eva_uid }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(eva)],
        }),
    );

    ctrl.open_link("https://t.me/+9990001122");
    assert_eq!(
        ctrl.session().expect("session").active_peer(),
        PeerId::from_user(eva_uid as u64)
    );
    assert_eq!(
        fake.invoked_count(tl_fns::contacts::ResolvePhone::CONSTRUCTOR_ID),
        1
    );

    shutdown_runtime(ctrl, runtime);
}

#[test]
fn live_join_invite_shows_preview_box_and_imports_chat() {
    let (mut ctrl, fake, runtime, _tmp) = boot_live_controller();

    fake.enqueue_ok(
        tl_fns::messages::CheckChatInvite::CONSTRUCTOR_ID,
        &tl_enums::ChatInvite::ChatInvite(tl_types::ChatInvite {
            channel: true,
            broadcast: false,
            public: false,
            megagroup: true,
            request_needed: false,
            verified: true,
            scam: false,
            fake: false,
            can_refulfill_subscription: false,
            title: "Rust Builders".to_owned(),
            about: Some("Systems programming discussions".to_owned()),
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            participants_count: 42,
            participants: Some(vec![tl_enums::User::User(synthetic_user(
                710_088, "Sam", "sam_dev", false,
            ))]),
            color: 3,
            subscription_pricing: None,
            subscription_form_id: None,
            bot_verification: None,
        }),
    );

    ctrl.open_link("https://t.me/+InviteHash42");
    match ctrl.box_host().top_kind() {
        Some(BoxKind::JoinInvite(inv)) => {
            assert_eq!(inv.hash, "InviteHash42");
            assert_eq!(inv.title, "Rust Builders");
            assert_eq!(inv.participants_count, 42);
            assert!(inv.verified);
            assert_eq!(inv.participant_names, vec!["Sam Tester".to_owned()]);
        }
        other => panic!("expected BoxKind::JoinInvite, got {other:?}"),
    }

    // Confirming the invite calls `messages.importChatInvite` and resolves the joined channel peer.
    let joined_chan = synthetic_channel(888_123, "Rust Builders", None);
    fake.enqueue_ok(
        tl_fns::messages::ImportChatInvite::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatInviteJoinResult::Ok(tl_types::messages::ChatInviteJoinResultOk {
            updates: tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: vec![tl_enums::Chat::Channel(joined_chan)],
                date: now_unix_i32(),
                seq: 2,
            }),
        }),
    );
    let joined_peer = ctrl
        .session_backend_mut()
        .import_chat_invite("InviteHash42")
        .expect("import_chat_invite");
    assert_eq!(joined_peer, PeerId::from_channel(888_123));

    shutdown_runtime(ctrl, runtime);
}

#[test]
fn live_addlist_stickers_theme_bg_giftcode_nft_iv_and_unknown_fallback() {
    let (mut ctrl, fake, runtime, _tmp) = boot_live_controller();

    // 1. `t.me/addlist/work_folder` -> `chatlists.checkChatlistInvite` -> ConfirmTag::JoinChatlist
    let work_chan = synthetic_channel(900_001, "Work Updates", Some("work_updates"));
    fake.enqueue_ok(
        tl_fns::chatlists::CheckChatlistInvite::CONSTRUCTOR_ID,
        &tl_enums::chatlists::ChatlistInvite::ChatlistInvite(tl_types::chatlists::ChatlistInvite {
            title_noanimate: false,
            title: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Work".to_owned(),
                entities: Vec::new(),
            }),
            emoticon: Some("💼".to_owned()),
            peers: vec![tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 900_001,
            })],
            chats: vec![tl_enums::Chat::Channel(work_chan)],
            users: Vec::new(),
        }),
    );
    ctrl.open_link("https://t.me/addlist/work_folder");
    match ctrl.box_host().top_kind() {
        Some(BoxKind::Confirm(c)) => match &c.tag {
            ConfirmTag::JoinChatlist { slug, peers } => {
                assert_eq!(slug, "work_folder");
                assert_eq!(peers, &vec![PeerId::from_channel(900_001)]);
            }
            other => panic!("expected ConfirmTag::JoinChatlist, got {other:?}"),
        },
        other => panic!("expected Confirm box for addlist, got {other:?}"),
    }

    // 2. `tg://settings/privacy` -> Settings::PrivacySecurity
    ctrl.open_link("tg://settings/privacy");
    assert_eq!(
        ctrl.session().expect("session").main_section(),
        Some(&SectionMemento::Settings {
            page: SettingsPage::PrivacySecurity,
        })
    );

    // 3. `tg://invoice?slug=inv_42`, `tg://stars_topup?balance=500`, `tg://premium_offer?ref=top`
    ctrl.open_link("https://t.me/$inv_42");
    ctrl.open_link("tg://stars_topup?balance=500&purpose=bot");
    ctrl.open_link("tg://premium_offer?ref=top");
    let overlays = ctrl.take_deep_link_overlay_requests();
    assert_eq!(overlays.len(), 3);
    assert!(matches!(
        &overlays[0],
        DeepLinkOverlayRequest::OpenPayments { slug: Some(s), .. } if s == "inv_42"
    ));

    // 4. `tg://login?code=54321`
    ctrl.open_link("tg://login?code=54321");
    assert_eq!(ctrl.take_pending_login_code().as_deref(), Some("54321"));

    // 5. `https://t.me/iv?url=https%3A%2F%2Fexample.com%2Fpost&rhash=abc#sec1` -> OpenInstantView
    ctrl.open_link("https://t.me/iv?url=https%3A%2F%2Fexample.com%2Fpost&rhash=abc#sec1");
    let wv_reqs = ctrl.take_webview_requests();
    assert_eq!(
        wv_reqs,
        vec![WebViewHostRequest::OpenInstantView {
            url: "https://t.me/iv?url=https%3A%2F%2Fexample.com%2Fpost&rhash=abc".to_owned(),
            webpage_id: None,
            fragment: Some("sec1".to_owned()),
        }]
    );

    // 6. `addstickers`, `addtheme`, `bg/<slug>`, `giftcode`, `confirmphone`, `m/<slug>`
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: true,
                masks: false,
                emojis: false,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: None,
                id: 5001,
                access_hash: 5002,
                title: "Animals Pack".to_owned(),
                short_name: "AnimalsPack".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 20,
                hash: 1,
            }),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: Vec::new(),
        }),
    );
    ctrl.open_link("https://t.me/addstickers/AnimalsPack");
    assert_eq!(
        fake.invoked_count(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID),
        1
    );

    fake.enqueue_ok(
        tl_fns::account::GetTheme::CONSTRUCTOR_ID,
        &tl_enums::Theme::Theme(tl_types::Theme {
            creator: false,
            default: false,
            for_chat: false,
            id: 6001,
            access_hash: 6002,
            slug: "NordDark".to_owned(),
            title: "Nord Dark".to_owned(),
            document: None,
            settings: None,
            emoticon: None,
            installs_count: Some(42),
        }),
    );
    ctrl.open_link("https://t.me/addtheme/NordDark");
    assert_eq!(
        fake.invoked_count(tl_fns::account::GetTheme::CONSTRUCTOR_ID),
        1
    );

    fake.enqueue_ok(
        tl_fns::account::GetWallPaper::CONSTRUCTOR_ID,
        &tl_enums::WallPaper::NoFile(tl_types::WallPaperNoFile {
            id: 7001,
            default: false,
            dark: true,
            settings: None,
        }),
    );
    ctrl.open_link("https://t.me/bg/forest_wp");
    assert_eq!(
        fake.invoked_count(tl_fns::account::GetWallPaper::CONSTRUCTOR_ID),
        1
    );

    fake.enqueue_ok(
        tl_fns::payments::CheckGiftCode::CONSTRUCTOR_ID,
        &tl_enums::payments::CheckedGiftCode::CheckedGiftCode(
            tl_types::payments::CheckedGiftCode {
                via_giveaway: false,
                from_id: None,
                giveaway_msg_id: None,
                to_id: None,
                date: now_unix_i32(),
                days: 90,
                used_date: None,
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    ctrl.open_link("https://t.me/giftcode/GIFT_90D");
    assert_eq!(
        fake.invoked_count(tl_fns::payments::CheckGiftCode::CONSTRUCTOR_ID),
        1
    );

    let biz_uid = 710_099i64;
    let biz_user = synthetic_user(biz_uid, "Biz", "biz_shop", false);
    fake.enqueue_ok(
        tl_fns::account::ResolveBusinessChatLink::CONSTRUCTOR_ID,
        &tl_enums::account::ResolvedBusinessChatLinks::ResolvedBusinessChatLinks(
            tl_types::account::ResolvedBusinessChatLinks {
                peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: biz_uid }),
                message: "Order #42".to_owned(),
                entities: None,
                chats: Vec::new(),
                users: vec![tl_enums::User::User(biz_user)],
            },
        ),
    );
    ctrl.open_link("https://t.me/m/order_42");
    assert_eq!(
        ctrl.session().expect("session").active_peer(),
        PeerId::from_user(biz_uid as u64)
    );
    assert_eq!(
        ctrl.main_widget_mut().history_mut().compose_mut().text,
        "Order #42"
    );

    // 7. Unknown `tg://future_feature` -> `help.getDeepLinkInfo` -> ModalBox::Inform
    fake.enqueue_ok(
        tl_fns::help::GetDeepLinkInfo::CONSTRUCTOR_ID,
        &tl_enums::help::DeepLinkInfo::DeepLinkInfo(tl_types::help::DeepLinkInfo {
            update_app: true,
            message: "Please update Rustgram to open this link.".to_owned(),
            entities: None,
        }),
    );
    ctrl.open_link("tg://future_feature?x=1");
    assert!(ctrl.layer_stack().is_layer_shown());
    assert_eq!(
        fake.invoked_count(tl_fns::help::GetDeepLinkInfo::CONSTRUCTOR_ID),
        1
    );

    shutdown_runtime(ctrl, runtime);
}

#[test]
fn live_media_forward_item_and_open_in_external_app_work() {
    let (ctrl, fake, runtime, tmp) = boot_live_controller();

    // Seed a forwardable photo message in the live tg-data session along with its peer.
    let photo_id = 555_001u64;
    let peer_uid = 710_002i64;
    let peer_id = tg_data::PeerId::from_user(tg_data::UserId(peer_uid as u64));
    let self_uid = 710_001i64;
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(synthetic_user(
                peer_uid, "Bob", "bob_live", false,
            ))],
            &[],
        );
        let mut msg = tg_data::Message::new_text(
            peer_id,
            tg_data::MsgId(77),
            Some(peer_id),
            now_unix_i32(),
            tg_data::TextWithEntities::plain("Photo"),
        );
        msg.content = tg_data::MessageContent::Regular {
            text: tg_data::TextWithEntities::plain("Photo"),
            media: tg_data::MessageMedia::Photo(Box::new(tg_data::PhotoData {
                id: tg_data::PhotoId(photo_id),
                access_hash: 999,
                file_reference: vec![1, 2, 3],
                date: now_unix_i32(),
                dc_id: 2,
                has_stickers: false,
                sizes: vec![tg_data::PhotoSize {
                    kind: "y".to_owned(),
                    width: 200,
                    height: 150,
                    size: 4,
                }],
                ..tg_data::PhotoData::default()
            })),
        };
        s.insert_message(msg);
    });

    // `forward_item_to` invokes `messages.forwardMessages` to the target peer.
    let fwd_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: now_unix_i32(),
        seq: 3,
    });
    fake.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
        &fwd_updates,
    );
    ctrl.session_backend()
        .forward_item_to(MediaItemId(photo_id), PeerId::from_user(self_uid as u64).0)
        .expect("forward_item_to should succeed for forwardable media");
    assert_eq!(
        fake.invoked_count(tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID),
        1
    );

    // `open_in_external_app` downloads via `upload.getFile` and writes the temp file.
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileJpeg,
            mtime: now_unix_i32(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xD9],
        }),
    );

    // Verify `launch_external_file_with` invokes the shell launcher with the saved path.
    let file_path = tmp.path().join("photo_555001.jpg");
    ctrl.session_backend()
        .save_to_disk(MediaItemId(photo_id), &file_path)
        .expect("save_to_disk");
    let launched = Arc::new(Mutex::new(None));
    let launched_clone = Arc::clone(&launched);
    launch_external_file_with(&file_path, move |p| {
        *launched_clone.lock().expect("lock") = Some(p.to_path_buf());
        Ok(())
    })
    .expect("launch_external_file_with");
    assert_eq!(
        launched.lock().expect("lock").as_deref(),
        Some(file_path.as_path())
    );

    shutdown_runtime(ctrl, runtime);
}

#[test]
fn live_setlanguage_and_proxy_links_route_to_settings_and_add_proxy_box() {
    let (mut ctrl, fake, runtime, _tmp) = boot_live_controller();

    // 1. `https://t.me/setlanguage/klingon_dl` routes through `SettingsController::open_set_language_link`
    //    -> `langpack.getLanguage` -> SettingsSection::Language with ConfirmLinkSwitch prompt.
    fake.enqueue_ok(
        tl_fns::langpack::GetLanguage::CONSTRUCTOR_ID,
        &tl_enums::LangPackLanguage::LangPackLanguage(tl_types::LangPackLanguage {
            official: false,
            rtl: false,
            beta: false,
            name: "Klingon DeepLink".to_owned(),
            native_name: "tlhIngan".to_owned(),
            lang_code: "klingon_dl".to_owned(),
            base_lang_code: Some("en".to_owned()),
            plural_code: "en".to_owned(),
            strings_count: 1000,
            translated_count: 800,
            translations_url: "https://translations.telegram.org/klingon_dl".to_owned(),
        }),
    );
    ctrl.open_link("https://t.me/setlanguage/klingon_dl");
    assert_eq!(
        fake.invoked_count(tl_fns::langpack::GetLanguage::CONSTRUCTOR_ID),
        1
    );
    let settings = ctrl
        .main_widget_mut()
        .settings_mut()
        .expect("SettingsController initialized by setlanguage link");
    assert_eq!(
        settings.current_section(),
        tg_app::settings::SettingsSection::Language
    );
    assert!(matches!(
        settings.language.pending_prompt.as_ref(),
        Some(tg_app::settings::backend::LanguagePrompt::ConfirmLinkSwitch { info, .. })
            if info.lang_code == "klingon_dl"
    ));

    // 2. `tg://proxy?...` and `https://t.me/socks?...` route through `route_proxy_link` -> `BoxKind::AddProxy`.
    ctrl.open_link(
        "tg://proxy?server=127.0.0.1&port=443&secret=dd0123456789abcdef0123456789abcdef",
    );
    match ctrl.box_host().top_kind() {
        Some(BoxKind::AddProxy(p)) => {
            assert!(p.is_valid());
            assert_eq!(p.proxy.proxy_type, tg_core::ProxyType::Mtproto);
            assert_eq!(p.proxy.host, "127.0.0.1");
            assert_eq!(p.proxy.port, 443);
        }
        other => panic!("expected BoxKind::AddProxy for tg://proxy, got {other:?}"),
    }

    ctrl.open_link("https://t.me/socks?server=127.0.0.1&port=1080&user=u&pass=p");
    match ctrl.box_host().top_kind() {
        Some(BoxKind::AddProxy(p)) => {
            assert!(p.is_valid());
            assert_eq!(p.proxy.proxy_type, tg_core::ProxyType::Socks5);
            assert_eq!(p.proxy.host, "127.0.0.1");
            assert_eq!(p.proxy.port, 1080);
            assert_eq!(p.proxy.user, "u");
            assert_eq!(p.proxy.password, "p");
        }
        other => panic!("expected BoxKind::AddProxy for t.me/socks, got {other:?}"),
    }

    shutdown_runtime(ctrl, runtime);
}
