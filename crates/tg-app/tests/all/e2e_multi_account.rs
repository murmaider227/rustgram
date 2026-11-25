//! End-to-end `egui_kittest` and multi-account runtime tests for WP-08:
//! - Main Menu expandable accounts list (avatar, name, unread badge, active ring, "Add Account")
//! - Switching accounts via Main Menu and via system tray accounts menu
//! - Background `SessionRuntime` staying active while inactive (live background unread updates &
//!   cross-account notification click switching account and opening target chat)
//! - Account limit enforcement (3 accounts default, 4 with Premium, up to `PREMIUM_MAX_ACCOUNTS`)
//! - Summed tray & taskbar unread badge respecting per-account `include_muted_counter` /
//!   `count_unread_messages` settings
//! - Logging out the active account automatically switching to a remaining account
//! - Persistence across restart with tdesktop `tdata` layout (`data`, `data#2`, `data#3`)
//!
//! Uses only synthetic peers/messages, `FakeRpcClient`, `FakeTransportFactory`, and
//! `MockTrayPlatform` (zero network traffic, zero real accounts or credentials).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{PointerButton, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;
use tg_api::{ApiCredentials, DcId, InvocationError, RpcClient};
use tg_core::domain::Environment;
use tg_crypto::AuthKey;
use tg_data::UserId as DataUserId;
use tg_mtproto::FakeTransportFactory;
use tg_platform::tray::{TrayEvent, TrayMenuItem};
use tg_session::{AccountManager, SessionRuntime, SessionRuntimeConfig, WorkingDirOpenOptions};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tokio::sync::broadcast;

use tg_app::dialogs::filters_menu::SIDEBAR_MENU_BUTTON_HEIGHT;
use tg_app::live::{MultiAccountFrameAction, MultiAccountWorkspaceHost};
use tg_app::notifications::backend::NotifyBackend;
use tg_app::notifications::manager::NotificationSystem;
use tg_app::notifications::settings::{
    NotificationPreferences, NotifySettingsStore, UnreadBadgeResult,
};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::tray::MockTrayPlatform;
use tg_app::tray::accounts_menu::account_menu_id;

use crate::common::{TEST_STEP_DT, apply_deterministic_style, init_headless_env};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

fn synthetic_tl_user(
    id: i64,
    first_name: &str,
    last_name: &str,
    username: &str,
    phone: &str,
    is_self: bool,
    premium: bool,
) -> tl_types::User {
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
        premium,
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
        access_hash: Some(id * 1000 + 13),
        first_name: Some(first_name.to_owned()),
        last_name: Some(last_name.to_owned()),
        username: Some(username.to_owned()),
        phone: Some(phone.to_owned()),
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

fn synthetic_tl_dialog(
    peer_uid: i64,
    top_message: i32,
    unread_count: i32,
    muted: bool,
) -> tl_enums::Dialog {
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
                mute_until: Some(if muted { i32::MAX } else { 0 }),
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

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
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

struct FakeAccountFixture {
    user_id: i64,
    rpc: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    peer_uid: i64,
}

#[allow(clippy::too_many_arguments)]
fn start_fake_account(
    self_uid: i64,
    first_name: &str,
    last_name: &str,
    username: &str,
    phone: &str,
    premium: bool,
    peer_uid: i64,
    peer_name: &str,
    unread_count: i32,
    muted: bool,
) -> FakeAccountFixture {
    let now = now_unix_i32();
    let self_user = synthetic_tl_user(
        self_uid, first_name, last_name, username, phone, true, premium,
    );
    let peer_user = synthetic_tl_user(
        peer_uid,
        peer_name,
        "Contact",
        &format!("peer_{peer_uid}"),
        "+10009990000",
        false,
        false,
    );
    let peer_msg = synthetic_tl_message(
        10,
        peer_uid,
        peer_uid,
        false,
        now - 30,
        &format!("Hello from {peer_name}"),
    );
    let dialog = synthetic_tl_dialog(peer_uid, 10, unread_count, muted);

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
            unread_count,
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
            dialogs: vec![dialog],
            messages: vec![peer_msg.clone()],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user.clone())],
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
                unread_count,
            }),
        }),
    );
    for _ in 0..3 {
        fake.enqueue_ok(
            tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
            &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                messages: vec![peer_msg.clone()],
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![tl_enums::User::User(peer_user.clone())],
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
    }

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

    FakeAccountFixture {
        user_id: self_uid,
        rpc: fake,
        runtime,
        peer_uid,
    }
}

struct MultiAccountTestState {
    host: MultiAccountWorkspaceHost,
    tray_mock: Arc<Mutex<MockTrayPlatform>>,
    last_action: MultiAccountFrameAction,
    actions_log: Vec<MultiAccountFrameAction>,
    last_egui_time: f64,
    _temp_dir: TempDir,
}

impl MultiAccountTestState {
    fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let egui_time = ctx.input(|i| i.time);
        let delta_secs = (egui_time - self.last_egui_time).max(0.0);
        self.last_egui_time = egui_time;
        if delta_secs > 0.0 {
            tg_app::clock::advance_test_now(Duration::from_secs_f64(delta_secs));
        }
        let act = self.host.show(&ctx, ui);
        self.last_action = act;
        if act != MultiAccountFrameAction::None {
            self.actions_log.push(act);
        }
    }
}

fn step_n(harness: &mut Harness<'static, MultiAccountTestState>, n: usize) {
    for _ in 0..n {
        harness.step();
    }
}

fn click_pos(harness: &mut Harness<'static, MultiAccountTestState>, pos: Pos2) {
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

#[test]
fn multi_account_menu_switch_background_unread_tray_and_logout() {
    init_headless_env();
    tg_app::clock::init_test_clock();

    let temp_dir = TempDir::new().expect("temp dir");
    let settings_path = temp_dir.path().join("multi_shell_settings.json");

    // Start 3 fake accounts running concurrently in separate SessionRuntimes:
    // - Slot 0: Alice One (0 unread initially, active)
    // - Slot 1: Beta Two (2 unmuted unread messages in 1 chat)
    // - Slot 2: Gamma Three (3 muted unread messages in 1 chat)
    let acc0 = start_fake_account(
        700_001,
        "Alice",
        "One",
        "alice_one",
        "+10000001001",
        false,
        710_001,
        "Dan",
        0,
        false,
    );
    let acc1 = start_fake_account(
        700_002,
        "Beta",
        "Two",
        "beta_two",
        "+10000001002",
        false,
        710_002,
        "Erin",
        2,
        false,
    );
    let acc2 = start_fake_account(
        700_003,
        "Gamma",
        "Three",
        "gamma_three",
        "+10000001003",
        false,
        710_003,
        "Frank",
        3,
        true,
    );

    let tray_mock = Arc::new(Mutex::new(MockTrayPlatform::default()));
    let notify_sys = NotificationSystem::headless(
        NotificationPreferences::default(),
        NotifySettingsStore::default(),
        temp_dir.path().join("notify_cache"),
    );

    let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        settings_path,
        notify_sys,
        Box::new(Arc::clone(&tray_mock)),
    );
    host.add_account(
        0,
        DataUserId::from_tl(acc0.user_id),
        false,
        false,
        Arc::clone(&acc0.runtime),
        None,
        true,
        false,
    );
    host.add_account(
        1,
        DataUserId::from_tl(acc1.user_id),
        false,
        false,
        Arc::clone(&acc1.runtime),
        None,
        false,
        false,
    );
    host.add_account(
        2,
        DataUserId::from_tl(acc2.user_id),
        false,
        false,
        Arc::clone(&acc2.runtime),
        None,
        false,
        false,
    );

    let state = MultiAccountTestState {
        host,
        tray_mock: Arc::clone(&tray_mock),
        last_action: MultiAccountFrameAction::None,
        actions_log: Vec::new(),
        last_egui_time: 0.0,
        _temp_dir: temp_dir,
    };

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
            |ui, st: &mut MultiAccountTestState| {
                st.show(ui);
            },
            state,
        );

    step_n(&mut harness, 6);

    // 1. Verify initial tray badge sums across all 3 accounts (0 + 2 unmuted + 3 muted = 5, unmuted)
    //    and tray menu lists all 3 accounts with slot 0 checked.
    {
        let mock = harness.state().tray_mock.lock().expect("tray mock lock");
        assert_eq!(mock.tray_unread_count, 5);
        assert!(!mock.tray_muted);
        let account_items: Vec<_> = mock
            .menu
            .iter()
            .filter_map(|item| match item {
                TrayMenuItem::Action {
                    id, label, checked, ..
                } if *id >= 1000 => Some((*id, label.clone(), *checked)),
                _ => None,
            })
            .collect();
        assert_eq!(account_items.len(), 3, "tray menu must list all 3 accounts");
        assert!(account_items[0].1.contains("Alice One"));
        assert!(account_items[0].2, "slot 0 must be checked as active");
        assert!(account_items[1].1.contains("Beta Two (2)"));
        assert!(!account_items[1].2);
        assert!(account_items[2].1.contains("Gamma Three (3)"));
        assert!(!account_items[2].2);
    }

    // 2. Verify per-account unread counting settings (include_muted_counter & count_unread_messages):
    //    - Exclude muted on slot 2 -> badge drops from 5 to 2 (only slot 1's 2 unmuted messages).
    //    - Count chats instead of messages on slot 1 -> badge drops from 2 to 1 (1 unread chat).
    harness
        .state_mut()
        .host
        .set_account_unread_prefs(2, false, true);
    step_n(&mut harness, 2);
    assert_eq!(
        harness.state().host.total_unread_badge(),
        UnreadBadgeResult {
            count: 2,
            muted: false
        }
    );
    assert_eq!(
        harness
            .state()
            .tray_mock
            .lock()
            .expect("lock")
            .tray_unread_count,
        2
    );

    harness
        .state_mut()
        .host
        .set_account_unread_prefs(1, true, false);
    step_n(&mut harness, 2);
    assert_eq!(
        harness.state().host.total_unread_badge(),
        UnreadBadgeResult {
            count: 1,
            muted: false
        }
    );

    // Restore default counting on slots 1 & 2 (include_muted=true, count_messages=true).
    harness
        .state_mut()
        .host
        .set_account_unread_prefs(1, true, true);
    harness
        .state_mut()
        .host
        .set_account_unread_prefs(2, true, true);
    step_n(&mut harness, 2);
    assert_eq!(
        harness.state().host.total_unread_badge(),
        UnreadBadgeResult {
            count: 5,
            muted: false
        }
    );

    // 3. Open Main Menu -> expand accounts list via profile cover -> verify avatars/names/badges
    //    and "Add Account" button via egui_kittest accessibility queries!
    click_pos(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    step_n(&mut harness, 4);
    assert!(
        harness.query_by_label("Rustgram Desktop").is_some(),
        "Main Menu drawer must open"
    );
    // Click profile cover to expand the accounts switcher.
    harness.get_by_label("Alice One (+10000001001)").click();
    step_n(&mut harness, 4);

    assert!(
        harness.query_by_label("Alice One").is_some(),
        "expanded Main Menu must show active account 'Alice One'"
    );
    assert!(
        harness.query_by_label("Beta Two (2 unread)").is_some(),
        "expanded Main Menu must show inactive account 'Beta Two (2 unread)'"
    );
    assert!(
        harness.query_by_label("Gamma Three (3 unread)").is_some(),
        "expanded Main Menu must show inactive account 'Gamma Three (3 unread)'"
    );
    assert!(
        harness.query_by_label("Add Account").is_some(),
        "expanded Main Menu must show 'Add Account' button"
    );

    // 4. At 3 non-premium accounts, clicking "Add Account" must NOT emit AddAccount (limit = 3).
    assert_eq!(harness.state().host.max_accounts(), 3);
    assert!(!harness.state().host.can_add_account());
    harness.get_by_label("Add Account").click();
    step_n(&mut harness, 4);
    assert!(
        !harness
            .state()
            .actions_log
            .contains(&MultiAccountFrameAction::AddAccount),
        "Add Account must be blocked at limit 3 without Premium"
    );

    // 5. Reopen Main Menu -> click "Beta Two (2 unread)" to switch from slot 0 to slot 1.
    click_pos(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    step_n(&mut harness, 4);
    harness.get_by_label("Beta Two (2 unread)").click();
    step_n(&mut harness, 6);

    assert_eq!(
        harness.state().host.active_slot(),
        1,
        "clicking Beta Two in Main Menu must switch active slot to 1"
    );
    assert!(
        harness
            .state()
            .actions_log
            .contains(&MultiAccountFrameAction::SwitchedAccount(1))
    );

    // Switching to slot 1 opens no chat (tdesktop's primary window clears the section stack,
    // `SessionNavigation::showByInitialId`), so Erin's 2 unread stay unread next to slot 2's
    // (Gamma Three) 3 muted unread messages: the tray total is 5.
    let tray_before_bg = harness
        .state()
        .tray_mock
        .lock()
        .expect("lock")
        .tray_unread_count;
    assert_eq!(
        tray_before_bg, 5,
        "after switching to slot 1, Erin's 2 unread and slot 2's 3 muted unread remain"
    );

    // 6. Background unread & notification on INACTIVE slot 0 (Alice One):
    //    Push a real TL UpdateNewMessage onto acc0's FakeRpcClient while slot 1 is active!
    let incoming_msg_id = 42i32;
    let now = now_unix_i32();
    acc0.rpc
        .push_update(tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: synthetic_tl_message(
                    incoming_msg_id,
                    acc0.peer_uid,
                    acc0.peer_uid,
                    false,
                    now,
                    "Background ping for Alice!",
                ),
                pts: 11,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: now,
            seq: 2,
        }));

    // Poll with a deadline until acc0's background SessionRuntime actor processes the update
    // and MultiAccountWorkspaceHost propagates the unread increment to slot 0 & the tray badge.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut slot0_unread = 0;
    let mut tray_unread = 0;
    while Instant::now() < deadline {
        harness.step();
        slot0_unread = harness
            .state()
            .host
            .account_summaries()
            .into_iter()
            .find(|s| s.slot_index == 0)
            .expect("slot 0 summary")
            .unread_badge
            .count;
        tray_unread = harness
            .state()
            .tray_mock
            .lock()
            .expect("lock")
            .tray_unread_count;
        if slot0_unread >= 1 && tray_unread > tray_before_bg {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Verify slot 0's unread count incremented in the background and summed tray badge rose by 1!
    assert!(
        slot0_unread >= 1,
        "inactive slot 0 must receive background unread updates, got {slot0_unread}"
    );
    assert!(
        tray_unread > tray_before_bg,
        "summed tray badge must include background unread from inactive slot 0, got {tray_unread}"
    );

    // Verify clicking a notification from inactive slot 0 switches active slot back to 0 and opens Dan's chat!
    let alice_session_id = harness.state().host.account(0).expect("slot 0").session_id;
    let dan_peer = PeerId::from_user(acc0.peer_uid as u64);
    let mut notify_handle = harness.state().host.notify_backend_handle();
    assert!(notify_handle.has_multiple_accounts());
    assert_eq!(
        notify_handle.account_name(alice_session_id).as_deref(),
        Some("Alice One")
    );
    notify_handle.open_chat(
        alice_session_id,
        dan_peer,
        MsgId(0),
        MsgId(incoming_msg_id),
        None,
    );
    step_n(&mut harness, 6);

    assert_eq!(
        harness.state().host.active_slot(),
        0,
        "clicking notification from inactive account 0 must switch back to slot 0"
    );
    assert_eq!(
        harness
            .state()
            .host
            .active_workspace()
            .expect("active ws")
            .controller()
            .session()
            .expect("active session")
            .active_peer(),
        dan_peer,
        "clicking notification from inactive account 0 must open sender peer chat"
    );
    assert_eq!(
        harness
            .state()
            .host
            .active_workspace()
            .expect("active ws")
            .controller()
            .session()
            .expect("active session")
            .active_msg_id(),
        MsgId(incoming_msg_id),
        "clicking notification must target the notified message ID"
    );

    // 7. Tray accounts menu click switches directly to slot 2 (Gamma Three).
    let slot2_menu_id = account_menu_id(2).expect("menu id for slot 2");
    harness
        .state()
        .tray_mock
        .lock()
        .expect("lock")
        .push_event(TrayEvent::MenuItem(slot2_menu_id));
    step_n(&mut harness, 4);
    assert_eq!(
        harness.state().host.active_slot(),
        2,
        "clicking Gamma Three in tray accounts menu must switch active slot to 2"
    );

    // 8. Premium expands account cap from 3 -> 4, allowing "Add Account" to succeed!
    harness.state_mut().host.set_account_premium(1, true);
    assert_eq!(harness.state().host.max_accounts(), 4);
    assert!(harness.state().host.can_add_account());
    step_n(&mut harness, 2);

    click_pos(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    step_n(&mut harness, 4);
    // Slot 2's MainMenu starts collapsed, so expand its cover first.
    harness.get_by_label("Gamma Three (+10000001003)").click();
    step_n(&mut harness, 4);
    harness.get_by_label("Add Account").click();
    step_n(&mut harness, 4);
    assert!(
        harness
            .state()
            .actions_log
            .contains(&MultiAccountFrameAction::AddAccount),
        "with a Premium account (cap 4), clicking Add Account at 3 accounts must emit AddAccount"
    );

    // 9. While temporarily in Add Account intro, poll_background_only keeps background accounts
    //    and tray/notification clicks alive and returns the target slot to jump back to.
    {
        let beta_session_id = harness.state().host.account(1).expect("slot 1").session_id;
        let erin_peer = PeerId::from_user(acc1.peer_uid as u64);
        let mut notify_handle = harness.state().host.notify_backend_handle();
        notify_handle.open_chat(beta_session_id, erin_peer, MsgId(0), MsgId(10), None);
        let ctx = harness.ctx.clone();
        let switch_slot = harness.state_mut().host.poll_background_only(&ctx);
        assert_eq!(
            switch_slot,
            Some(1),
            "background notification click during Add Account must return target slot 1"
        );
    }

    // 10. Logging out active slot 2 switches automatically to another remaining account.
    let next_active = harness
        .state_mut()
        .host
        .remove_account(2)
        .expect("remaining accounts must keep active session");
    assert_ne!(next_active, 2);
    assert_eq!(harness.state().host.active_slot(), next_active);
    assert_eq!(harness.state().host.account_count(), 2);

    // Clean shutdown of all runtimes.
    harness.state_mut().host.on_exit();
    drop(harness);
    drop(acc0);
    drop(acc1);
    drop(acc2);
}

#[test]
fn multi_account_tdata_persistence_roundtrip_across_restart() {
    init_headless_env();

    let tmp = TempDir::new().expect("tempdir");
    let tdata = tmp.path().join("tdata");
    let creds = ApiCredentials {
        api_id: 99_003,
        api_hash: "11223344556677889900aabbccddeeff".to_owned(),
    };
    let make_opts = || {
        WorkingDirOpenOptions::new(tmp.path(), creds.clone())
            .with_transport_factory(Arc::new(FakeTransportFactory::new()))
            .with_pfs(false)
            .with_logout_timeout(Duration::from_millis(120))
            .with_destroy_keys_timeout(Duration::from_millis(120))
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Create 3 accounts (data, data#2, data#3), switch to slot 1, save, shut down.
    let mut expected_stems = Vec::new();
    {
        let mut mgr = AccountManager::open_and_unlock(make_opts(), b"").expect("open & unlock");
        mgr.account_mut(0)
            .expect("slot 0")
            .client
            .import_persistent_keys(vec![(2, AuthKey::new([0xA1u8; 256]))]);
        mgr.complete_login(
            0,
            &synthetic_tl_user(
                800_001,
                "First",
                "Acct",
                "first_acct",
                "+10000002001",
                true,
                false,
            ),
        )
        .expect("login 0");

        let idx1 = mgr.add_account(Environment::Production).expect("add 1");
        mgr.account_mut(idx1)
            .expect("slot 1")
            .client
            .import_persistent_keys(vec![(2, AuthKey::new([0xB2u8; 256]))]);
        mgr.complete_login(
            idx1,
            &synthetic_tl_user(
                800_002,
                "Second",
                "Acct",
                "second_acct",
                "+10000002002",
                true,
                true,
            ),
        )
        .expect("login 1");

        let idx2 = mgr.add_account(Environment::Production).expect("add 2");
        mgr.account_mut(idx2)
            .expect("slot 2")
            .client
            .import_persistent_keys(vec![(4, AuthKey::new([0xC3u8; 256]))]);
        mgr.complete_login(
            idx2,
            &synthetic_tl_user(
                800_003,
                "Third",
                "Acct",
                "third_acct",
                "+10000002003",
                true,
                false,
            ),
        )
        .expect("login 2");

        for (idx, expected_name) in [(0, "data"), (1, "data#2"), (2, "data#3")] {
            assert_eq!(mgr.account_data_name(idx), expected_name);
            expected_stems.push(mgr.account_key_file_part(idx));
        }

        mgr.switch_account(idx1).expect("switch to slot 1");
        mgr.save_all_accounts().expect("save all");
        mgr.shutdown();
    }

    // Confirm tdesktop `data`, `data#2`, `data#3` key files exist on disk.
    assert_eq!(expected_stems[0], "D877F783D5D3EF8C");
    for stem in &expected_stems {
        assert!(tdata.join(format!("{stem}s")).exists());
        assert!(tdata.join(stem).join("maps").exists());
    }

    // Restart: reopen AccountManager, confirm all 3 accounts + active slot + Premium limit (4),
    // then log out active slot 1 and verify switch to another remaining account persists!
    {
        let mut reopened =
            AccountManager::open_and_unlock(make_opts(), b"").expect("reopen after restart");
        assert_eq!(reopened.active_index(), Some(1));
        assert_eq!(reopened.accounts_authed_count(), 3);
        assert_eq!(reopened.max_accounts(), 4);

        rt.block_on(async { reopened.logout_account(1).await })
            .expect("logout active slot 1");
        let after_logout = reopened.active_index().expect("remaining active slot");
        assert_ne!(after_logout, 1);
        assert_eq!(reopened.accounts_authed_count(), 2);
        reopened.shutdown();
    }
}
