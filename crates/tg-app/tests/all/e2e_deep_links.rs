//! End-to-end `egui_kittest` tests for the deep links router (`tg://`, `t.me/`),
//! Instant View live routing, Join Preview Box confirmation, and Media Viewer Forward -> ShareBox
//! over [`LiveActiveWorkspace`].
//!
//! Uses only synthetic peers/messages and an in-process fake RPC client (zero network traffic,
//! zero real credentials, headless audio, no OS shell opens).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tokio::sync::broadcast;

use tg_app::boxes::BoxKind;
use tg_app::live::{LiveActiveWorkspace, LiveSession};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::startup::ipc::IncomingActivation;
use tg_app::window::section::{SectionMemento, SettingsPage};

use crate::common::{TEST_STEP_DT, apply_deterministic_style, click_at, init_headless_env};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        access_hash: Some(id * 1000 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
        photo: None,
        status: Some(tl_enums::UserStatus::Recently(
            tl_types::UserStatusRecently { by_me: false },
        )),
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

fn synthetic_tl_channel(id: i64, title: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: false,
        left: false,
        broadcast: false,
        verified: true,
        megagroup: true,
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
        access_hash: Some(id * 19 + 5),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: now_unix_i32(),
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(54),
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

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    invoked_ctors: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            invoked_ctors: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
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

impl RpcClient for FakeRpcClient {
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

struct DeepLinksTestState {
    workspace: LiveActiveWorkspace,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    last_egui_time: f64,
    _temp_dir: TempDir,
}

impl DeepLinksTestState {
    fn start() -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();

        let now = now_unix_i32();
        let self_uid = 800_001i64;
        let fake = FakeRpcClient::new(2);

        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(synthetic_tl_user(
                self_uid,
                "Alice",
                "alice_e2e",
                true,
            ))],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 5,
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
                    pts: 5,
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
        let temp_dir = TempDir::new().expect("tempdir");
        let workspace = LiveActiveWorkspace::new_headless(
            live_session,
            ShellSettings::default(),
            temp_dir.path().join("settings.json"),
            false,
        );

        Self {
            workspace,
            fake,
            runtime,
            last_egui_time: 0.0,
            _temp_dir: temp_dir,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let egui_time = ctx.input(|i| i.time);
        let delta_secs = (egui_time - self.last_egui_time).max(0.0);
        self.last_egui_time = egui_time;
        if delta_secs > 0.0 {
            tg_app::clock::advance_test_now(Duration::from_secs_f64(delta_secs));
        }
        let _ = self.workspace.show(&ctx, ui);
    }
}

fn step_n(harness: &mut Harness<'static, DeepLinksTestState>, n: usize) {
    for _ in 0..n {
        harness.step();
    }
}

#[test]
fn e2e_deep_links_resolve_invite_preview_share_settings_and_instant_view() {
    let state = DeepLinksTestState::start();

    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(1280.0, 820.0))
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut DeepLinksTestState| {
                st.show(ui);
            },
            state,
        );

    step_n(&mut harness, 4);

    // 1. Incoming IPC `tg://resolve?domain=dave_e2e&post=42&text=Hi%20Dave`
    //    -> live `contacts.resolveUsername` -> opens Dave's chat & prefills composer.
    let dave_uid = 800_042i64;
    let dave_user = synthetic_tl_user(dave_uid, "Dave", "dave_e2e", false);
    harness.state().fake.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: dave_uid }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(dave_user)],
        }),
    );
    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .handle_incoming_activation(
            IncomingActivation {
                urls: vec!["tg://resolve?domain=dave_e2e&post=42&text=Hi%20Dave".to_owned()],
                send_paths: Vec::new(),
                show: true,
                quit: false,
            },
            &ctx,
        );
    step_n(&mut harness, 6);

    assert_eq!(
        harness
            .state()
            .workspace
            .controller()
            .session()
            .expect("session")
            .active_peer(),
        PeerId::from_user(dave_uid as u64)
    );
    assert_eq!(
        harness
            .state()
            .workspace
            .controller()
            .session()
            .expect("session")
            .active_msg_id(),
        MsgId(42)
    );
    assert_eq!(
        harness
            .state_mut()
            .workspace
            .controller_mut()
            .main_widget_mut()
            .history_mut()
            .compose_mut()
            .text,
        "Hi Dave"
    );

    // 2. Join invite link `https://t.me/+InviteHashE2E` -> `messages.checkChatInvite`
    //    -> renders Join Preview Box -> clicking "Join Group" invokes `messages.importChatInvite`
    //    and navigates into the joined supergroup.
    harness.state().fake.enqueue_ok(
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
            title: "Rustgram Engineers".to_owned(),
            about: Some("Core Rust desktop engineering".to_owned()),
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            participants_count: 54,
            participants: None,
            color: 4,
            subscription_pricing: None,
            subscription_form_id: None,
            bot_verification: None,
        }),
    );
    let joined_chan_id = 888_555i64;
    let joined_chan = synthetic_tl_channel(joined_chan_id, "Rustgram Engineers");
    harness.state().fake.enqueue_ok(
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

    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link("https://t.me/+InviteHashE2E");
    step_n(&mut harness, 6);

    assert!(matches!(
        harness
            .state()
            .workspace
            .controller()
            .box_host()
            .top_kind(),
        Some(BoxKind::JoinInvite(inv)) if inv.title == "Rustgram Engineers"
    ));
    assert!(
        harness.query_by_label("Join Group").is_some(),
        "Join Preview Box must render the 'Join Group' confirmation button"
    );
    harness.get_by_label("Join Group").click();
    step_n(&mut harness, 6);

    assert!(
        harness.state().workspace.controller().box_host().is_empty(),
        "clicking 'Join Group' must close the Join Preview Box"
    );
    assert_eq!(
        harness
            .state()
            .workspace
            .controller()
            .session()
            .expect("session")
            .active_peer(),
        PeerId::from_channel(joined_chan_id as u64),
        "confirming invite must open the newly joined supergroup chat"
    );

    // 3. Share URL `https://t.me/share/url?...` opens ShareBox modal.
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link("https://t.me/share/url?url=https%3A%2F%2Frust-lang.org&text=Check%20this");
    step_n(&mut harness, 4);
    assert!(matches!(
        harness
            .state()
            .workspace
            .controller()
            .box_host()
            .top_kind(),
        Some(BoxKind::Share(s)) if s.share_link.as_deref() == Some("https://rust-lang.org")
    ));
    harness.key_press(egui::Key::Escape);
    step_n(&mut harness, 4);

    // 4. Settings deep link `tg://settings/privacy` navigates to Privacy & Security.
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link("tg://settings/privacy");
    step_n(&mut harness, 6);
    assert_eq!(
        harness
            .state()
            .workspace
            .controller()
            .session()
            .expect("session")
            .main_section(),
        Some(&SectionMemento::Settings {
            page: SettingsPage::PrivacySecurity,
        })
    );

    // 5. Instant View link `https://t.me/iv?url=...` routes through `messages.getWebPage` in live mode.
    let iv_url = "https://t.me/iv?url=https%3A%2F%2Fexample.com%2Fguide&rhash=test123";
    harness.state().fake.enqueue_ok(
        tl_fns::messages::GetWebPage::CONSTRUCTOR_ID,
        &tl_enums::messages::WebPage::WebPage(tl_types::messages::WebPage {
            webpage: tl_enums::WebPage::WebPage(tl_types::WebPage {
                has_large_media: false,
                video_cover_photo: false,
                id: 99_001,
                url: iv_url.to_owned(),
                display_url: "example.com/guide".to_owned(),
                hash: 1,
                r#type: Some("article".to_owned()),
                site_name: Some("Example Docs".to_owned()),
                title: Some("Instant View Guide".to_owned()),
                description: None,
                photo: None,
                embed_url: None,
                embed_type: None,
                embed_width: None,
                embed_height: None,
                duration: None,
                author: None,
                document: None,
                cached_page: Some(tl_enums::Page::Page(tl_types::Page {
                    part: false,
                    rtl: false,
                    v2: true,
                    url: iv_url.to_owned(),
                    blocks: vec![tl_enums::PageBlock::Title(tl_types::PageBlockTitle {
                        text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: "Instant View Guide".to_owned(),
                        }),
                    })],
                    photos: Vec::new(),
                    documents: Vec::new(),
                    views: Some(10),
                })),
                attributes: None,
            }),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link(iv_url);
    step_n(&mut harness, 4);
    assert!(
        harness
            .state_mut()
            .workspace
            .webview_host_mut()
            .instant_view()
            .is_some(),
        "t.me/iv link must open live InstantViewWindow"
    );

    // 6. Unsupported settings subpath shows desktop unsupported fallback toast.
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link("tg://settings/unsupported_mobile_route");
    step_n(&mut harness, 3);
    assert!(
        harness
            .query_by_label("This link is not supported on Desktop.")
            .is_some(),
        "unrecognized settings deep link must display the unsupported-link toast"
    );

    // 7. `tg://setlanguage?lang=pirate` routes to SettingsController::open_set_language_link.
    harness.state().fake.enqueue_ok(
        tl_fns::langpack::GetLanguage::CONSTRUCTOR_ID,
        &tl_enums::LangPackLanguage::LangPackLanguage(tl_types::LangPackLanguage {
            official: false,
            rtl: false,
            beta: false,
            name: "Pirate English".to_owned(),
            native_name: "Pirate".to_owned(),
            lang_code: "pirate".to_owned(),
            base_lang_code: Some("en".to_owned()),
            plural_code: "en".to_owned(),
            strings_count: 1000,
            translated_count: 750,
            translations_url: "https://translations.telegram.org/pirate".to_owned(),
        }),
    );
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link("tg://setlanguage?lang=pirate");
    step_n(&mut harness, 4);
    let settings_ctrl = harness
        .state_mut()
        .workspace
        .controller_mut()
        .main_widget_mut()
        .settings_mut()
        .expect("SettingsController open");
    assert_eq!(
        settings_ctrl.current_section(),
        tg_app::settings::SettingsSection::Language
    );
    assert!(matches!(
        settings_ctrl.language.pending_prompt.as_ref(),
        Some(tg_app::settings::backend::LanguagePrompt::ConfirmLinkSwitch { info, .. })
            if info.lang_code == "pirate"
    ));

    // 8. `tg://socks?...` routes through `route_proxy_link` -> AddProxy box -> clicking
    //    "Connect Proxy" applies `ConfirmTag::ApplyProxy` via `apply_confirmed_proxy`.
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_link("tg://socks?server=127.0.0.1&port=1080&user=u&pass=p");
    step_n(&mut harness, 4);
    assert!(matches!(
        harness
            .state()
            .workspace
            .controller()
            .box_host()
            .top_kind(),
        Some(BoxKind::AddProxy(p)) if p.proxy.host == "127.0.0.1" && p.proxy.port == 1080
    ));
    assert!(
        harness.query_by_label("Connect Proxy").is_some(),
        "AddProxy box must render the 'Connect Proxy' button"
    );
    harness.get_by_label("Connect Proxy").click();
    step_n(&mut harness, 4);
    assert!(
        harness.state().workspace.controller().box_host().is_empty(),
        "clicking 'Connect Proxy' must close AddProxy box"
    );
    let applied_proxy = tg_app::settings::backend::SettingsBackend::proxy_settings(
        harness.state().workspace.live_session(),
    )
    .expect("live proxy settings");
    assert!(applied_proxy.is_enabled());
    assert_eq!(applied_proxy.selected().host, "127.0.0.1");
    assert_eq!(applied_proxy.selected().port, 1080);

    // 9. Media overlay Forward opens ShareBox bound to `media_item_id`, and confirming
    //    calls `MediaBackend::forward_item_to` -> `messages.forwardMessages`.
    let photo_id = 910_077u64;
    let photo_msg_id = 333i32;
    let dave_data_peer = tg_data::PeerId::from_user(DataUserId::from_tl(dave_uid));
    harness.state().runtime.tokio_handle().block_on(async {
        let mut s = harness.state().runtime.session().write().await;
        let mut msg = tg_data::Message::new_text(
            dave_data_peer,
            tg_data::MsgId(i64::from(photo_msg_id)),
            Some(dave_data_peer),
            now_unix_i32(),
            tg_data::TextWithEntities::plain("Live Photo"),
        );
        msg.content = tg_data::MessageContent::Regular {
            text: tg_data::TextWithEntities::plain("Live Photo"),
            media: tg_data::MessageMedia::Photo(Box::new(tg_data::PhotoData {
                id: tg_data::PhotoId(photo_id),
                access_hash: 1234,
                file_reference: vec![1, 2, 3],
                date: now_unix_i32(),
                dc_id: 2,
                has_stickers: false,
                sizes: vec![tg_data::PhotoSize {
                    kind: "y".to_owned(),
                    width: 64,
                    height: 64,
                    size: 4,
                }],
                ..tg_data::PhotoData::default()
            })),
        };
        s.insert_message(msg);
    });
    harness
        .state_mut()
        .workspace
        .controller_mut()
        .open_history_media(
            PeerId::from_user(dave_uid as u64),
            MsgId(photo_msg_id),
            None,
        );
    step_n(&mut harness, 3);
    assert!(
        harness
            .state()
            .workspace
            .controller()
            .media_overlay()
            .is_open(),
        "media overlay must open for live photo"
    );
    // Click the Forward icon button in the media overlay bottom-right toolbar
    // (`footer_h = 54`, `btn_sz = 38`, 3rd button from right in 1280x820 viewport).
    click_at(&mut harness, egui::Pos2::new(1157.0, 793.0));
    step_n(&mut harness, 4);
    assert!(
        !harness
            .state()
            .workspace
            .controller()
            .media_overlay()
            .is_open(),
        "clicking Forward in media overlay must close overlay and open ShareBox"
    );
    match harness
        .state_mut()
        .workspace
        .controller_mut()
        .box_host_mut()
        .top_kind_mut()
    {
        Some(BoxKind::Share(share)) => {
            assert_eq!(
                share.media_item_id,
                Some(tg_app::media::backend::MediaItemId(photo_id))
            );
            share.toggle_target(PeerId::from_user(dave_uid as u64));
        }
        other => panic!("expected ShareBox opened from media overlay Forward, got {other:?}"),
    }
    harness.state().fake.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 3,
        }),
    );
    step_n(&mut harness, 3);
    harness.get_by_label("Send (1)").click();
    step_n(&mut harness, 4);
    assert!(
        harness.state().workspace.controller().box_host().is_empty(),
        "clicking Send in ShareBox must close ShareBox"
    );
    assert_eq!(
        harness
            .state()
            .fake
            .invoked_count(tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID),
        1,
        "confirming ShareBox from media overlay Forward must call MediaBackend::forward_item_to"
    );

    let runtime = Arc::clone(&harness.state().runtime);
    drop(harness);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}
