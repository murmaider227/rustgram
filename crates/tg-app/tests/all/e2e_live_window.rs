//! End-to-end `egui_kittest` tests for the unified live-mode desktop window
//! ([`LiveActiveWorkspace`] running the shared [`WindowController`] + [`MainWidget`] +
//! [`AppCallsHost`] + [`AppStoriesHost`] over [`LiveSession`]).
//!
//! Uses only synthetic peers/messages and an in-process fake RPC client (zero network traffic,
//! zero real credentials, headless audio, no OS tray/toasts).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{PointerButton, Pos2, Vec2};
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

use tg_app::dialogs::filters_menu::SIDEBAR_MENU_BUTTON_HEIGHT;
use tg_app::history::backend::HistoryBackend;
use tg_app::live::{LiveActiveWorkspace, LiveSession};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::PeerId;
use tg_app::window::main_menu::MAIN_MENU_COVER_HEIGHT;
use tg_app::window::section::SectionMemento;

use crate::common::{TEST_STEP_DT, apply_deterministic_style, init_headless_env};

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

fn synthetic_tl_message(
    id: i32,
    peer_uid: i64,
    from_uid: i64,
    out: bool,
    date: i32,
    text: &str,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    })
}

fn synthetic_tl_dialog(peer_uid: i64, top_message: i32, unread_count: i32) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message,
        read_inbox_max_id: (top_message - unread_count).max(0),
        read_outbox_max_id: top_message,
        unread_count,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
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
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn synthetic_photo_story(
    id: i32,
    date: i32,
    expire_date: i32,
    caption: &str,
) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: false,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        id,
        date,
        from_id: None,
        fwd_from: None,
        expire_date,
        caption: Some(caption.to_owned()),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                has_stickers: false,
                id: 90_000 + i64::from(id),
                access_hash: 77_000 + i64::from(id),
                file_reference: vec![1, 2, 3],
                date,
                sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                    r#type: "y".to_owned(),
                    w: 320,
                    h: 568,
                    size: 1024,
                })],
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        })),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    })
}

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
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

struct LiveWindowTestState {
    workspace: LiveActiveWorkspace,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    bob_peer: PeerId,
    last_egui_time: f64,
    _temp_dir: TempDir,
}

impl LiveWindowTestState {
    fn start() -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();

        let self_uid = 600_001i64;
        let bob_uid = 600_002i64;
        let carol_uid = 600_003i64;
        let now = now_unix_i32();

        let self_user = synthetic_tl_user(self_uid, "Alice", "alice_live", true);
        let bob_user = synthetic_tl_user(bob_uid, "Bob", "bob_live", false);
        let carol_user = synthetic_tl_user(carol_uid, "Carol", "carol_live", false);
        let bob_msg = synthetic_tl_message(
            10,
            bob_uid,
            bob_uid,
            false,
            now - 30,
            "Hello from Bob in live mode!",
        );
        let carol_msg = synthetic_tl_message(
            20,
            carol_uid,
            carol_uid,
            false,
            now - 90,
            "Hello from Carol in live mode!",
        );
        let bob_dialog = synthetic_tl_dialog(bob_uid, 10, 1);
        let carol_dialog = synthetic_tl_dialog(carol_uid, 20, 2);

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user)],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: now,
                seq: 1,
                unread_count: 3,
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
                dialogs: vec![bob_dialog, carol_dialog],
                messages: vec![bob_msg.clone(), carol_msg.clone()],
                chats: Vec::new(),
                users: vec![
                    tl_enums::User::User(bob_user.clone()),
                    tl_enums::User::User(carol_user.clone()),
                ],
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
                    pts: 10,
                    qts: 0,
                    date: now,
                    seq: 1,
                    unread_count: 3,
                }),
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
            &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                messages: vec![carol_msg],
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![tl_enums::User::User(carol_user)],
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
            &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                messages: vec![bob_msg],
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![tl_enums::User::User(bob_user.clone())],
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
            &tl_enums::messages::AffectedMessages::AffectedMessages(
                tl_types::messages::AffectedMessages {
                    pts: 11,
                    pts_count: 1,
                },
            ),
        );

        // Stories bootstrap responses: read tills, NotHidden page (Bob's story), Hidden page (empty).
        fake.enqueue_ok(
            tl_fns::stories::GetAllReadPeerStories::CONSTRUCTOR_ID,
            &tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: now,
                seq: 1,
            }),
        );
        fake.enqueue_ok(
            tl_fns::stories::GetAllStories::CONSTRUCTOR_ID,
            &tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
                has_more: false,
                count: 1,
                state: "live_stories_v1".to_owned(),
                peer_stories: vec![tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    max_read_id: None,
                    stories: vec![synthetic_photo_story(
                        77,
                        now - 120,
                        now + 3600,
                        "Live Bob Story",
                    )],
                })],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(bob_user)],
                stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                    tl_types::StoriesStealthMode {
                        active_until_date: None,
                        cooldown_until_date: None,
                    },
                ),
            }),
        );
        fake.enqueue_ok(
            tl_fns::stories::GetAllStories::CONSTRUCTOR_ID,
            &tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
                has_more: false,
                count: 0,
                state: "live_hidden_empty".to_owned(),
                peer_stories: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                    tl_types::StoriesStealthMode {
                        active_until_date: None,
                        cooldown_until_date: None,
                    },
                ),
            }),
        );

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("startup ready");

        let mut live_session = LiveSession::new(Arc::clone(&runtime), 1);

        // Verify LiveSession exposes itself as BotsBackend via HistoryBackend.
        assert!(
            HistoryBackend::as_bots_backend(&live_session).is_some(),
            "LiveSession must return Some(self) from HistoryBackend::as_bots_backend()"
        );
        assert!(
            HistoryBackend::as_bots_backend_mut(&mut live_session).is_some(),
            "LiveSession must return Some(self) from HistoryBackend::as_bots_backend_mut()"
        );

        let temp_dir = TempDir::new().expect("create temp settings dir");
        let settings_path = temp_dir.path().join("live_shell_settings.json");
        let workspace = LiveActiveWorkspace::new_headless(
            live_session,
            ShellSettings::default(),
            settings_path,
            true,
        );

        Self {
            workspace,
            runtime,
            bob_peer: PeerId::from_user(bob_uid as u64),
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

fn step_n(harness: &mut Harness<'static, LiveWindowTestState>, n: usize) {
    for _ in 0..n {
        harness.step();
    }
}

fn click_pos(harness: &mut Harness<'static, LiveWindowTestState>, pos: Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
}

/// Computes the vertical center of row index `idx` after the separator in `MainMenu`:
/// 0=New Group, 1=New Channel, 2=Contacts, 3=Calls, 4=Saved Messages, 5=Downloads, 6=Settings, 7=Night Mode.
fn main_menu_item_pos(row_after_sep: usize) -> Pos2 {
    // Cover, `st::mainMenuSkip`, the My Profile row, then the separator (skip + 1 px + skip).
    let base_y = MAIN_MENU_COVER_HEIGHT + 6.0 + 40.0 + 13.0;
    Pos2::new(120.0, base_y + (row_after_sep as f32 + 0.5) * 40.0)
}

#[test]
fn live_active_window_renders_chat_settings_calls_stories_and_boxes() {
    let state = LiveWindowTestState::start();
    let bob_peer = state.bob_peer;

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
            |ui, st: &mut LiveWindowTestState| {
                st.show(ui);
            },
            state,
        );

    step_n(&mut harness, 8);

    // 1. Live window shows dialogs (Bob and Carol) and clicking Bob opens Bob's live chat.
    assert_eq!(
        harness
            .state()
            .workspace
            .controller()
            .session()
            .expect("active SessionController")
            .active_peer(),
        PeerId::ZERO,
        "a live primary window opens with no chat, like tdesktop's showByInitialId"
    );
    assert!(
        harness.query_by_label("Bob Tester (1 unread)").is_some(),
        "dialogs list must show live peer 'Bob Tester (1 unread)'"
    );
    harness.get_by_label("Bob Tester (1 unread)").click();
    step_n(&mut harness, 8);

    assert_eq!(
        harness
            .state()
            .workspace
            .controller()
            .session()
            .expect("active SessionController")
            .active_peer(),
        bob_peer,
        "clicking Bob Tester must open Bob's live chat"
    );
    assert!(
        harness.query_by_label("Chat info: Bob Tester").is_some(),
        "live history header must show 'Chat info: Bob Tester'"
    );
    assert!(
        harness
            .state_mut()
            .workspace
            .controller_mut()
            .main_widget_mut()
            .history_mut()
            .inner()
            .messages()
            .iter()
            .any(|m| m.text.text == "Hello from Bob in live mode!"),
        "live HistoryInnerWidget must load and render Bob's message from FakeRpcClient"
    );

    // 2. Live Stories strip is populated from GetAllStories and its userpics are drawn at the
    //    right end of the chat-list search field (TGB-3008).
    assert!(
        harness.state().workspace.stories_host().has_stories(),
        "live AppStoriesHost must have Bob's story from GetAllStories"
    );
    let strip_rect = harness
        .state()
        .workspace
        .stories_host()
        .last_strip_rect()
        .expect("live StoriesStrip must be rendered in dialogs column");
    assert!(
        strip_rect.height() >= 20.0 && strip_rect.width() >= 30.0,
        "rendered StoriesStrip must have positive bounds, got {strip_rect:?}"
    );
    let search_field = harness
        .state_mut()
        .workspace
        .controller_mut()
        .main_widget_mut()
        .dialogs_mut()
        .search_field_rect()
        .expect("the chat-list search field is shown");
    assert!(
        search_field.contains_rect(strip_rect),
        "collapsed stories sit inside the search field {search_field:?}, got {strip_rect:?}"
    );
    click_pos(&mut harness, strip_rect.center());
    step_n(&mut harness, 12);
    assert!(
        harness
            .state()
            .workspace
            .stories_host()
            .strip()
            .is_expanded(),
        "clicking the live StoriesStrip must expand it"
    );
    assert!(
        harness
            .state_mut()
            .workspace
            .stories_host_mut()
            .open_first_story(),
        "opening the first live story must succeed"
    );
    step_n(&mut harness, 4);
    assert!(
        harness.state().workspace.stories_host().viewer().is_open(),
        "live StoryViewerOverlay must be open"
    );
    harness.key_press(egui::Key::Escape);
    step_n(&mut harness, 4);
    assert!(
        !harness.state().workspace.stories_host().viewer().is_open(),
        "Escape must close the live StoryViewerOverlay"
    );

    // 3. Open Main Menu -> Settings and verify Settings UI elements exist in live mode.
    click_pos(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    step_n(&mut harness, 4);
    assert!(
        harness.query_by_label("Rustgram Desktop").is_some(),
        "Main Menu drawer must open in live mode"
    );
    click_pos(&mut harness, main_menu_item_pos(6));
    step_n(&mut harness, 10);
    assert!(
        matches!(
            harness
                .state()
                .workspace
                .controller()
                .session()
                .and_then(|s| s.main_section()),
            Some(SectionMemento::Settings { .. })
        ),
        "Settings section must be active on WindowController"
    );
    assert!(
        harness.query_by_label("Settings").is_some(),
        "Settings screen heading must be visible in live mode"
    );
    assert!(
        harness
            .query_by_label_contains("Privacy and Security")
            .is_some(),
        "Settings items must be visible in live mode"
    );

    // 4. Close the Settings layer (it sits over the window at this width), then open
    //    Main Menu -> Calls entry point and verify live Recent Calls box opens.
    harness.key_press(egui::Key::Escape);
    step_n(&mut harness, 4);
    click_pos(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    step_n(&mut harness, 4);
    assert!(
        harness.query_by_label("Rustgram Desktop").is_some(),
        "Main Menu drawer must reopen for Calls entry point"
    );
    click_pos(&mut harness, main_menu_item_pos(3));
    step_n(&mut harness, 8);

    assert!(
        harness
            .state()
            .workspace
            .calls_host()
            .is_recent_calls_open(),
        "clicking Calls in Main Menu must open live AppCallsHost's Recent Calls dialog"
    );
    assert!(
        harness.query_by_label_contains("Recent Calls").is_some(),
        "Recent Calls heading must be visible in live mode"
    );
    // Close Recent Calls box before checking modal boxes.
    harness
        .state_mut()
        .workspace
        .calls_host_mut()
        .close_recent_calls();
    step_n(&mut harness, 2);

    // 5. Calls also opened Settings -> Calls as a layer; close it (Escape pops the section,
    //    then the layer), then open Main Menu -> New Group box and verify unified BoxHost
    //    works in live mode.
    for _ in 0..3 {
        let settings_open = matches!(
            harness
                .state()
                .workspace
                .controller()
                .session()
                .and_then(|s| s.main_section()),
            Some(SectionMemento::Settings { .. })
        );
        if !settings_open {
            break;
        }
        harness.key_press(egui::Key::Escape);
        step_n(&mut harness, 3);
    }
    click_pos(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    step_n(&mut harness, 4);
    click_pos(&mut harness, main_menu_item_pos(0));
    step_n(&mut harness, 6);
    assert!(
        !harness.state().workspace.controller().box_host().is_empty(),
        "clicking New Group in Main Menu must push CreateGroup box onto unified BoxHost"
    );

    // Cleanly shut down the Tokio runtime owned by the test state.
    let runtime = Arc::clone(&harness.state().runtime);
    drop(harness);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}
