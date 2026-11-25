//! WP-32 live UI click-path and deep-link tests over [`egui_kittest`] + fake RPCs.
//!
//! Covers:
//! 1. Managed bot profile `"Edit Bot Info"` click path -> `BoxKind::EditBotInfo` -> `bots.getBotInfo` / `bots.setBotInfo`.
//! 2. Group/channel message context menu `"Moderate..."` click path -> `BoxKind::ModerateMessages`
//!    -> `channels.deleteMessages` / `channels.reportSpam` / `channels.editBanned`.
//! 3. Profile 3-dots menu `"Import Chat History"` click path -> `BoxKind::ChatImport` (`SelectFile`)
//!    -> file selection -> `messages.checkHistoryImport` / `checkHistoryImportPeer` / `initHistoryImport` / `startHistoryImport`.
//! 4. Deep links: `tg://addstyle` (`PreviewAiTone`), `tg://giftcode` (`payments.checkGiftCode` + `applyGiftCode`),
//!    `tg://nft` (`PaymentsScreen::StarGifts`), `tg://oauth` (`messages.requestUrlAuth` + `acceptUrlAuth` / `declineUrlAuth`).
//! 5. Phone (`tel:...`) and Bank Card (`bankcard:...`) entity link conversion & click handlers
//!    (`contacts.resolvePhone` + `payments.getBankCardData`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::text::TextEntityKind;
use tokio::sync::broadcast;

use tg_app::boxes::BoxKind;
use tg_app::boxes::chat_import::{ChatImportStep, ImportFilePicker, build_synthetic_export_zip};
use tg_app::boxes::confirm::ConfirmTag;
use tg_app::history::text_bridge::entities_to_rich_text;
use tg_app::live::LiveSession;
use tg_app::payments::PaymentsScreen;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::controller::{WebViewHostRequest, WindowController};
use tg_app::window::deep_links::{DeepLink, DeepLinkOverlayRequest, parse_deep_link};
use tg_app::window::section::SectionShow;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct Wp32FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    invoked_ctors: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp32FakeRpc {
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

    fn enqueue_err(&self, ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Err(err)));
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

impl RpcClient for Wp32FakeRpc {
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

fn synthetic_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    is_bot: bool,
    bot_can_edit: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
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
        bot_can_edit,
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
        last_name: Some("Demo".to_owned()),
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: is_self.then(|| "15550000001".to_owned()),
        photo: None,
        status: None,
        bot_info_version: is_bot.then_some(1),
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
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

fn synthetic_admin_supergroup(channel_id: i64, title: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast: false,
        verified: false,
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
        id: channel_id,
        access_hash: Some(channel_id * 13 + 5),
        title: title.to_owned(),
        username: Some("wp32_mod_group".to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: now_unix_i32(),
        restriction_reason: None,
        admin_rights: Some(tl_enums::ChatAdminRights::ChatAdminRights(
            tl_types::ChatAdminRights {
                change_info: true,
                post_messages: true,
                edit_messages: true,
                delete_messages: true,
                ban_users: true,
                invite_users: true,
                pin_messages: true,
                add_admins: true,
                anonymous: false,
                manage_call: true,
                other: true,
                manage_topics: true,
                post_stories: false,
                edit_stories: false,
                delete_stories: false,
                manage_direct_messages: false,
                manage_ranks: false,
                manage_linked_peers: false,
                manage_welcome_messages: false,
            },
        )),
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(25),
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

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: now_unix_i32(),
        seq: 1,
    })
}

fn boot_wp32_live_controller() -> (
    WindowController,
    Wp32FakeRpc,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    TempDir,
) {
    let now = now_unix_i32();
    let tmp = TempDir::new().expect("tempdir");
    let fake = Wp32FakeRpc::new(2);
    let self_uid = 820_001i64;
    let self_user = synthetic_user(self_uid, "Self", "self_wp32", true, false, false);

    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user)],
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

    let mut live = LiveSession::new(Arc::clone(&runtime), 1);
    live.refresh_cached_views();

    let ctrl = WindowController::with_session(
        live,
        ShellSettings::default(),
        tmp.path().join("shell.json"),
    );
    (ctrl, fake, runtime, tmp)
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

/// A profile in the side column only gets the `⋮` menu once the Info stack has history
/// (`WrapWidget::setupTopBarMenuToggle`: `wrap() != Side || hasStackHistory()`); give it one.
fn give_side_profile_stack_history(
    harness: &mut egui_kittest::Harness<'static, WindowController>,
    peer: PeerId,
) {
    let info = harness.state_mut().main_widget_mut().third_info_mut();
    info.set_peer(peer);
    // The same profile on another media tab is a new stack entry (identical ones are dropped).
    info.push_section(tg_app::info::wrap_widget::InfoSection::Profile {
        peer_id: peer,
        media_tab: tg_app::info::backend::SharedMediaTab::Files,
    });
    step_n(harness, 3);
}

fn make_live_harness(ctrl: WindowController) -> egui_kittest::Harness<'static, WindowController> {
    HarnessBuilder::default()
        .with_size(Vec2::new(1280.0, 820.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, ctrl: &mut WindowController| {
                ctrl.show(ui);
            },
            ctrl,
        )
}

fn step_n<S>(harness: &mut egui_kittest::Harness<'_, S>, count: usize) {
    for _ in 0..count {
        harness.step();
    }
}

#[test]
fn kittest_edit_bot_info_click_path_and_rpc() {
    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    let editable_bot_uid = 820_101i64;
    let readonly_bot_uid = 820_102i64;
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[
                tl_enums::User::User(synthetic_user(
                    editable_bot_uid,
                    "Managed Bot",
                    "managed_demo_bot",
                    false,
                    true,
                    true,
                )),
                tl_enums::User::User(synthetic_user(
                    readonly_bot_uid,
                    "Other Bot",
                    "other_demo_bot",
                    false,
                    true,
                    false,
                )),
            ],
            &[],
        );
    });
    ctrl.session_backend_as_mut::<LiveSession>()
        .expect("live session")
        .refresh_cached_views();

    let editable_bot_peer = PeerId::from_user(editable_bot_uid as u64);
    let readonly_bot_peer = PeerId::from_user(readonly_bot_uid as u64);

    // Non-editable bot should NOT have `can_edit`.
    let ro_profile =
        tg_app::info::InfoBackend::peer_profile(ctrl.session_backend(), readonly_bot_peer)
            .expect("readonly bot profile");
    assert!(!ro_profile.can_edit);

    // Editable managed bot MUST have `can_edit == true` from `User.bot_can_edit`.
    let ed_profile =
        tg_app::info::InfoBackend::peer_profile(ctrl.session_backend(), editable_bot_peer)
            .expect("editable bot profile");
    assert!(ed_profile.can_edit);

    // Open managed bot profile in Info section.
    ctrl.session_mut()
        .expect("session")
        .show_peer_info(editable_bot_peer, SectionShow::clear_stack());

    // Enqueue `bots.getBotInfo` and `bots.setBotInfo` responses.
    fake.enqueue_ok(
        tl_fns::bots::GetBotInfo::CONSTRUCTOR_ID,
        &tl_enums::bots::BotInfo::BotInfo(tl_types::bots::BotInfo {
            name: "Managed Bot".to_owned(),
            about: "Initial short bio".to_owned(),
            description: "Initial bot description".to_owned(),
        }),
    );
    fake.enqueue_ok(tl_fns::bots::SetBotInfo::CONSTRUCTOR_ID, &true);

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 3);
    give_side_profile_stack_history(&mut harness, editable_bot_peer);

    // "Edit Bot Info" lives in the profile `⋮` menu (rightmost "More actions" button).
    let more = harness
        .get_all_by_label("More actions")
        .max_by(|a, b| {
            a.rect()
                .min
                .x
                .partial_cmp(&b.rect().min.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("profile More actions button");
    more.click();
    step_n(&mut harness, 3);
    harness.get_by_label("Edit Bot Info").click();
    step_n(&mut harness, 3);

    assert!(
        matches!(
            harness.state().box_host().top_kind(),
            Some(BoxKind::EditBotInfo(_))
        ),
        "expected BoxKind::EditBotInfo after clicking Edit Bot Info, got {:?}",
        harness
            .state()
            .box_host()
            .top_kind()
            .map(std::mem::discriminant)
    );
    assert_eq!(
        fake.invoked_count(tl_fns::bots::GetBotInfo::CONSTRUCTOR_ID),
        1
    );

    // Mutate the loaded default language draft and click Save.
    if let Some(BoxKind::EditBotInfo(state)) = harness.state_mut().box_host_mut().top_kind_mut() {
        state.draft.current_mut("").about = "Updated managed bio".to_owned();
    }
    step_n(&mut harness, 2);

    harness
        .get_by_label(&tg_lang::tr::lng_settings_save())
        .click();
    step_n(&mut harness, 3);

    assert_eq!(
        fake.invoked_count(tl_fns::bots::SetBotInfo::CONSTRUCTOR_ID),
        1
    );
    assert!(harness.state().box_host().is_empty());

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn kittest_moderate_messages_context_menu_and_rpc() {
    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    let chan_id = 820_500i64;
    let spammer_uid = 820_501i64;
    let chan_data_peer = tg_data::PeerId::from_channel(tg_data::ChannelId(chan_id as u64));
    let spammer_data_peer = tg_data::PeerId::from_user(tg_data::UserId(spammer_uid as u64));
    let chan_peer = PeerId::from_channel(chan_id as u64);

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(synthetic_user(
                spammer_uid,
                "Spammer",
                "spammer_demo",
                false,
                false,
                false,
            ))],
            &[tl_enums::Chat::Channel(synthetic_admin_supergroup(
                chan_id,
                "Moderators Test Group",
            ))],
        );
        let msg = tg_data::Message::new_text(
            chan_data_peer,
            tg_data::MsgId(77),
            Some(spammer_data_peer),
            now_unix_i32(),
            tg_data::TextWithEntities::plain("Unwanted promo message to moderate"),
        );
        s.insert_message(msg);
    });
    ctrl.session_backend_as_mut::<LiveSession>()
        .expect("live session")
        .refresh_cached_views();

    ctrl.session_mut().expect("session").show_peer_history(
        chan_peer,
        SectionShow::clear_stack(),
        MsgId::SHOW_AT_END,
    );

    // Enqueue moderation RPCs: deleteMessages, reportSpam, editBanned.
    fake.enqueue_ok(
        tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 10,
                pts_count: 1,
            },
        ),
    );
    fake.enqueue_ok(tl_fns::channels::ReportSpam::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::channels::EditBanned::CONSTRUCTOR_ID,
        &empty_updates(),
    );

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 4);

    // Right-click the incoming message bubble to open the live context menu.
    let bubble_node = harness.get_by_label_contains("Unwanted promo message to moderate");
    bubble_node.click_secondary();
    step_n(&mut harness, 3);

    // Click "Moderate..." in the context menu!
    harness.get_by_label("Moderate...").click();
    step_n(&mut harness, 3);

    match harness.state().box_host().top_kind() {
        Some(BoxKind::ModerateMessages(state)) => {
            assert_eq!(state.peer_id, chan_peer);
            assert_eq!(state.message_ids, vec![77]);
            assert_eq!(state.participants.len(), 1);
            assert_eq!(
                state.participants[0].peer_id,
                PeerId::from_user(spammer_uid as u64)
            );
            assert_eq!(
                state.participants[0].username.as_deref(),
                Some("spammer_demo")
            );
        }
        other => panic!("expected BoxKind::ModerateMessages, got {other:?}"),
    }

    // Enable Report spam and Ban in the modal and click Delete (`moderate_confirm`).
    if let Some(BoxKind::ModerateMessages(state)) =
        harness.state_mut().box_host_mut().top_kind_mut()
    {
        state.report_spam = true;
        state.ban_or_restrict = true;
    }
    step_n(&mut harness, 2);

    harness.get_by_label(&tg_lang::tr::lng_box_delete()).click();
    step_n(&mut harness, 3);

    assert_eq!(
        fake.invoked_count(tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        fake.invoked_count(tl_fns::channels::ReportSpam::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        fake.invoked_count(tl_fns::channels::EditBanned::CONSTRUCTOR_ID),
        1
    );
    assert!(harness.state().box_host().is_empty());

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn kittest_chat_import_menu_select_file_and_rpc() {
    let (mut ctrl, fake, runtime, tmp) = boot_wp32_live_controller();

    let friend_uid = 820_201i64;
    let friend_peer = PeerId::from_user(friend_uid as u64);
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(synthetic_user(
                friend_uid,
                "Clara",
                "clara_import",
                false,
                false,
                false,
            ))],
            &[],
        );
    });
    ctrl.session_backend_as_mut::<LiveSession>()
        .expect("live session")
        .refresh_cached_views();

    // Prepare a synthetic WhatsApp export ZIP on disk (no real personal data).
    let chat_txt = b"[23/11/2026, 10:00:00] Clara: Hello from imported history\n[23/11/2026, 10:01:00] Self: Welcome!\n";
    let zip_bytes =
        build_synthetic_export_zip(&[("_chat.txt", chat_txt.as_slice())]).expect("build zip");
    let zip_path = tmp.path().join("whatsapp_demo.zip");
    std::fs::write(&zip_path, &zip_bytes).expect("write synthetic zip");

    // Open Clara's profile in Info section.
    ctrl.session_mut()
        .expect("session")
        .show_peer_info(friend_peer, SectionShow::clear_stack());

    // Enqueue history-import RPCs.
    fake.enqueue_ok(
        tl_fns::messages::CheckHistoryImport::CONSTRUCTOR_ID,
        &tl_enums::messages::HistoryImportParsed::HistoryImportParsed(
            tl_types::messages::HistoryImportParsed {
                pm: true,
                group: false,
                title: Some("Clara".to_owned()),
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::messages::CheckHistoryImportPeer::CONSTRUCTOR_ID,
        &tl_enums::messages::CheckedHistoryImportPeer::CheckedHistoryImportPeer(
            tl_types::messages::CheckedHistoryImportPeer {
                confirm_text: "Import messages into chat with Clara?".to_owned(),
            },
        ),
    );
    fake.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::messages::InitHistoryImport::CONSTRUCTOR_ID,
        &tl_enums::messages::HistoryImport::HistoryImport(tl_types::messages::HistoryImport {
            id: 4242,
        }),
    );
    fake.enqueue_ok(tl_fns::messages::StartHistoryImport::CONSTRUCTOR_ID, &true);

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 3);
    give_side_profile_stack_history(&mut harness, friend_peer);

    // Open the Info top-bar 3-dots menu and click "Import Chat History".
    harness.get_by_label("More actions").click();
    step_n(&mut harness, 2);
    harness.get_by_label("Import Chat History").click();
    step_n(&mut harness, 3);

    match harness.state_mut().box_host_mut().top_kind_mut() {
        Some(BoxKind::ChatImport(state)) => {
            assert_eq!(state.step, ChatImportStep::SelectFile);
            assert_eq!(
                state.preferred_peer.as_ref().map(|(id, _)| *id),
                Some(friend_peer)
            );
            // Inject headless file picker preset so clicking the button doesn't open an OS dialog.
            state.file_picker = ImportFilePicker::PresetPath(zip_path);
        }
        other => panic!("expected BoxKind::ChatImport(SelectFile), got {other:?}"),
    }
    step_n(&mut harness, 2);

    // Click "Choose Export File (.zip / .txt)..." inside ChatImportStep::SelectFile.
    harness
        .get_by_label("Choose Export File (.zip / .txt)...")
        .click();
    step_n(&mut harness, 3);

    assert_eq!(
        fake.invoked_count(tl_fns::messages::CheckHistoryImport::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        fake.invoked_count(tl_fns::messages::CheckHistoryImportPeer::CONSTRUCTOR_ID),
        1
    );
    match harness.state().box_host().top_kind() {
        Some(BoxKind::ChatImport(state)) => {
            assert_eq!(state.step, ChatImportStep::Confirm);
            assert_eq!(state.selected_peer, Some(friend_peer));
        }
        other => panic!("expected ChatImportStep::Confirm, got {other:?}"),
    }

    // Click "Import" to run upload + initHistoryImport + startHistoryImport.
    harness.get_by_label("Import").click();
    step_n(&mut harness, 3);

    assert_eq!(
        fake.invoked_count(tl_fns::messages::InitHistoryImport::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        fake.invoked_count(tl_fns::messages::StartHistoryImport::CONSTRUCTOR_ID),
        1
    );
    match harness.state().box_host().top_kind() {
        Some(BoxKind::ChatImport(state)) => {
            assert_eq!(state.step, ChatImportStep::Completed);
            assert_eq!(state.summary.as_ref().map(|s| s.import_id), Some(4242));
        }
        other => panic!("expected ChatImportStep::Completed, got {other:?}"),
    }

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn kittest_addstyle_deeplink_opens_preview_ai_tone_and_saves() {
    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    assert_eq!(
        parse_deep_link("https://t.me/addstyle/pirate_mode"),
        Some(DeepLink::AddStyle {
            slug: "pirate_mode".to_owned(),
        })
    );

    fake.enqueue_ok(
        tl_fns::aicompose::GetTone::CONSTRUCTOR_ID,
        &tl_enums::aicompose::Tones::Tones(tl_types::aicompose::Tones {
            hash: 101,
            tones: vec![tl_enums::AiComposeTone::AiComposeTone(
                tl_types::AiComposeTone {
                    creator: false,
                    id: 7701,
                    access_hash: 770_199,
                    slug: "pirate_mode".to_owned(),
                    title: "Pirate Mode".to_owned(),
                    emoji_id: Some(5368324170671202287),
                    prompt: Some("Rewrite messages in playful nautical phrasing.".to_owned()),
                    installs_count: Some(15),
                    author_id: Some(9001),
                    example_english: Some(tl_enums::AiComposeToneExample::AiComposeToneExample(
                        tl_types::AiComposeToneExample {
                            from: tl_enums::TextWithEntities::TextWithEntities(
                                tl_types::TextWithEntities {
                                    text: "Hello team!".to_owned(),
                                    entities: Vec::new(),
                                },
                            ),
                            to: tl_enums::TextWithEntities::TextWithEntities(
                                tl_types::TextWithEntities {
                                    text: "Ahoy crew!".to_owned(),
                                    entities: Vec::new(),
                                },
                            ),
                        },
                    )),
                },
            )],
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::aicompose::GetToneExample::CONSTRUCTOR_ID,
        &tl_enums::AiComposeToneExample::AiComposeToneExample(tl_types::AiComposeToneExample {
            from: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Ship it!".to_owned(),
                entities: Vec::new(),
            }),
            to: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Hoist the sails and ship it!".to_owned(),
                entities: Vec::new(),
            }),
        }),
    );
    fake.enqueue_ok(tl_fns::aicompose::SaveTone::CONSTRUCTOR_ID, &true);

    ctrl.open_link("tg://addstyle?slug=pirate_mode");
    assert_eq!(
        fake.invoked_count(tl_fns::aicompose::GetTone::CONSTRUCTOR_ID),
        1
    );
    match ctrl.box_host().top_kind() {
        Some(BoxKind::PreviewAiTone(preview)) => {
            assert_eq!(preview.tone.slug, "pirate_mode");
            assert_eq!(preview.tone.title, "Pirate Mode");
            assert!(!preview.installed);
        }
        other => panic!("expected BoxKind::PreviewAiTone, got {other:?}"),
    }

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 3);

    // Click "Next Example" -> invokes `aicompose.getToneExample`.
    harness.get_by_label("Next Example").click();
    step_n(&mut harness, 3);
    assert_eq!(
        fake.invoked_count(tl_fns::aicompose::GetToneExample::CONSTRUCTOR_ID),
        1
    );
    if let Some(BoxKind::PreviewAiTone(preview)) = harness.state().box_host().top_kind() {
        assert_eq!(
            preview.example.as_ref().map(|e| e.to.text.as_str()),
            Some("Hoist the sails and ship it!")
        );
    }

    // Click "Add Style" -> invokes `aicompose.saveTone` and pops modal.
    harness.get_by_label("Add Style").click();
    step_n(&mut harness, 3);
    assert_eq!(
        fake.invoked_count(tl_fns::aicompose::SaveTone::CONSTRUCTOR_ID),
        1
    );
    assert!(harness.state().box_host().is_empty());

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn kittest_giftcode_deeplink_confirm_and_apply_rpc() {
    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    assert_eq!(
        parse_deep_link("tg://giftcode?slug=TEST_GIFT_90"),
        Some(DeepLink::GiftCode {
            slug: "TEST_GIFT_90".to_owned(),
        })
    );

    // 1. Unused gift code -> BoxKind::Confirm(ApplyGiftCode) -> Redeem invokes `payments.applyGiftCode`.
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
    fake.enqueue_ok(
        tl_fns::payments::ApplyGiftCode::CONSTRUCTOR_ID,
        &empty_updates(),
    );

    ctrl.open_link("https://t.me/giftcode/TEST_GIFT_90");
    assert_eq!(
        fake.invoked_count(tl_fns::payments::CheckGiftCode::CONSTRUCTOR_ID),
        1
    );
    match ctrl.box_host().top_kind() {
        Some(BoxKind::Confirm(c)) => {
            assert_eq!(
                c.tag,
                ConfirmTag::ApplyGiftCode {
                    slug: "TEST_GIFT_90".to_owned(),
                    days: 90,
                }
            );
        }
        other => panic!("expected BoxKind::Confirm(ApplyGiftCode), got {other:?}"),
    }

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 3);

    harness.get_by_label("Redeem Gift").click();
    step_n(&mut harness, 3);
    assert_eq!(
        fake.invoked_count(tl_fns::payments::ApplyGiftCode::CONSTRUCTOR_ID),
        1
    );
    assert!(harness.state().box_host().is_empty());

    // 2. Already redeemed gift code -> informational modal only, no applyGiftCode.
    fake.enqueue_ok(
        tl_fns::payments::CheckGiftCode::CONSTRUCTOR_ID,
        &tl_enums::payments::CheckedGiftCode::CheckedGiftCode(
            tl_types::payments::CheckedGiftCode {
                via_giveaway: false,
                from_id: None,
                giveaway_msg_id: None,
                to_id: None,
                date: now_unix_i32() - 1000,
                days: 30,
                used_date: Some(now_unix_i32() - 500),
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    harness
        .state_mut()
        .open_link("https://t.me/giftcode/ALREADY_USED");
    assert!(harness.state().box_host().is_empty());
    assert!(harness.state().layer_stack().is_layer_shown());
    assert_eq!(
        fake.invoked_count(tl_fns::payments::ApplyGiftCode::CONSTRUCTOR_ID),
        1
    );

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn unique_gift_deeplink_routes_to_star_gifts_overlay() {
    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    fake.enqueue_ok(
        tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID,
        &tl_enums::payments::UniqueStarGift::UniqueStarGift(tl_types::payments::UniqueStarGift {
            gift: tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
                require_premium: false,
                resale_ton_only: false,
                theme_available: false,
                burned: false,
                crafted: false,
                id: 900_001,
                gift_id: 500_001,
                title: "Plush Pepe".to_owned(),
                slug: "PlushPepe-42".to_owned(),
                num: 42,
                owner_id: None,
                owner_name: Some("Collector".to_owned()),
                owner_address: None,
                attributes: Vec::new(),
                availability_issued: 100,
                availability_total: 1000,
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
        }),
    );

    ctrl.open_link("https://t.me/nft/PlushPepe-42");
    // Dispatch only queues the overlay; the collectible is fetched once by the payments host.
    assert_eq!(
        fake.invoked_count(tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID),
        0
    );
    // Must route to PaymentsScreen::StarGifts overlay with slug (NOT ModalBox::Inform).
    assert!(!ctrl.layer_stack().is_layer_shown());
    let reqs = ctrl.take_deep_link_overlay_requests();
    assert_eq!(
        reqs,
        vec![DeepLinkOverlayRequest::OpenPayments {
            screen: PaymentsScreen::StarGifts,
            slug: Some("PlushPepe-42".to_owned()),
        }]
    );

    // Same path as `LiveAppHost::dispatch_deep_link_overlay_requests`: a live-backed
    // PaymentsScreenHost opens the collectible detail via `payments.getUniqueStarGift`.
    let payments = ctrl.session_backend().create_payments_backend();
    let passport = ctrl.session_backend().create_passport_backend();
    let mut payments_host = tg_app::payments::PaymentsScreenHost::new(payments, passport);
    payments_host
        .open_deep_link_request(PaymentsScreen::StarGifts, Some("PlushPepe-42"))
        .expect("unique gift opens");
    assert_eq!(
        fake.invoked_count(tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(payments_host.active_screen(), PaymentsScreen::StarGifts);
    let gifts_view = payments_host
        .gifts_view_mut()
        .expect("StarGiftsView loaded");
    assert_eq!(gifts_view.slug_lookup_input, "PlushPepe-42");
    assert!(matches!(
        gifts_view.modal,
        Some(tg_app::payments::star_gifts::StarGiftModal::UniqueSlugDetail(_))
    ));

    shutdown_runtime(ctrl, runtime);
}

#[test]
fn kittest_oauth_deeplink_request_accept_and_decline_rpc() {
    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    let bot_uid = 820_301i64;
    let bot_user = synthetic_user(bot_uid, "LoginBot", "login_demo_bot", false, true, false);

    // 1. Accept flow (`requestUrlAuth` -> BoxKind::UrlAuth -> click "Log In" -> `acceptUrlAuth`).
    fake.enqueue_ok(
        tl_fns::messages::RequestUrlAuth::CONSTRUCTOR_ID,
        &tl_enums::UrlAuthResult::Request(tl_types::UrlAuthResultRequest {
            request_write_access: true,
            request_phone_number: false,
            match_codes_first: false,
            is_app: false,
            request_wallet: false,
            bot: tl_enums::User::User(bot_user.clone()),
            domain: "example.invalid".to_owned(),
            browser: None,
            platform: None,
            ip: None,
            region: None,
            match_codes: None,
            user_id_hint: None,
            verified_app_name: None,
            tc_session_id: None,
            tc_client_id: None,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::AcceptUrlAuth::CONSTRUCTOR_ID,
        &tl_enums::UrlAuthResult::Accepted(tl_types::UrlAuthResultAccepted {
            url: Some("https://example.invalid/callback?ok=1".to_owned()),
        }),
    );

    ctrl.open_link("tg://oauth?token=fake_oauth_tok_1");
    assert_eq!(
        fake.invoked_count(tl_fns::messages::RequestUrlAuth::CONSTRUCTOR_ID),
        1
    );
    assert!(matches!(
        ctrl.box_host().top_kind(),
        Some(BoxKind::UrlAuth(_))
    ));

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 3);

    harness.get_by_label("Log In").click();
    step_n(&mut harness, 3);

    assert_eq!(
        fake.invoked_count(tl_fns::messages::AcceptUrlAuth::CONSTRUCTOR_ID),
        1
    );
    assert!(harness.state().box_host().is_empty());
    assert!(harness.state_mut().take_webview_requests().contains(
        &WebViewHostRequest::OpenExternalUrl("https://example.invalid/callback?ok=1".to_owned())
    ));

    // 2. Cancel / decline flow (`requestUrlAuth` -> click Cancel -> `declineUrlAuth`).
    fake.enqueue_ok(
        tl_fns::messages::RequestUrlAuth::CONSTRUCTOR_ID,
        &tl_enums::UrlAuthResult::Request(tl_types::UrlAuthResultRequest {
            request_write_access: false,
            request_phone_number: false,
            match_codes_first: false,
            is_app: false,
            request_wallet: false,
            bot: tl_enums::User::User(bot_user),
            domain: "example.invalid".to_owned(),
            browser: None,
            platform: None,
            ip: None,
            region: None,
            match_codes: None,
            user_id_hint: None,
            verified_app_name: None,
            tc_session_id: None,
            tc_client_id: None,
        }),
    );
    fake.enqueue_ok(tl_fns::messages::DeclineUrlAuth::CONSTRUCTOR_ID, &true);

    harness
        .state_mut()
        .open_link("https://t.me/oauth?startapp=fake_oauth_tok_2");
    step_n(&mut harness, 3);
    assert!(matches!(
        harness.state().box_host().top_kind(),
        Some(BoxKind::UrlAuth(_))
    ));

    harness.get_by_label(&tg_lang::tr::lng_cancel()).click();
    step_n(&mut harness, 3);

    assert_eq!(
        fake.invoked_count(tl_fns::messages::DeclineUrlAuth::CONSTRUCTOR_ID),
        1
    );
    assert!(harness.state().box_host().is_empty());

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn kittest_phone_and_bank_card_click_handlers() {
    // 0. Verify text_bridge converts Phone and BankCard entities to clickable links.
    let sample = TextWithEntities {
        text: "Call +1 555 010 1234 or card 4000 1234 5678 9010".to_owned(),
        entities: vec![
            EntityInText::new(EntityType::Phone, 5, 15),
            EntityInText::new(EntityType::BankCard, 29, 19),
        ],
    };
    let rich = entities_to_rich_text(&sample);
    assert!(rich.spans.iter().any(|s| matches!(
        &s.kind,
        TextEntityKind::Link(url) if url == "tel:+1 555 010 1234"
    )));
    assert!(rich.spans.iter().any(|s| matches!(
        &s.kind,
        TextEntityKind::Link(url) if url == "bankcard:4000 1234 5678 9010"
    )));

    assert_eq!(
        parse_deep_link("tel:+1 555 010 1234"),
        Some(DeepLink::Phone {
            phone: "+1 555 010 1234".to_owned(),
        })
    );
    assert_eq!(
        parse_deep_link("bankcard:4000 1234 5678 9010"),
        Some(DeepLink::BankCard {
            number: "4000 1234 5678 9010".to_owned(),
        })
    );

    let (mut ctrl, fake, runtime, _tmp) = boot_wp32_live_controller();

    // 1. Phone click handler -> `contacts.resolvePhone` -> PhoneActionBox -> View Profile & Add to Contacts.
    let resolved_uid = 820_401i64;
    let resolved_user = synthetic_user(resolved_uid, "Devon", "devon_phone", false, false, false);
    fake.enqueue_ok(
        tl_fns::contacts::ResolvePhone::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser {
                user_id: resolved_uid,
            }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(resolved_user)],
        }),
    );

    ctrl.open_link("tel:+1 555 010 1234");
    assert_eq!(
        fake.invoked_count(tl_fns::contacts::ResolvePhone::CONSTRUCTOR_ID),
        1
    );
    match ctrl.box_host().top_kind() {
        Some(BoxKind::PhoneAction(state)) => {
            assert_eq!(state.phone, "+1 555 010 1234");
            assert_eq!(state.normalized_digits, "15550101234");
            assert_eq!(
                state.resolved_peer.as_ref().map(|(pid, _)| *pid),
                Some(PeerId::from_user(resolved_uid as u64))
            );
            assert!(!state.is_self_phone);
        }
        other => panic!("expected BoxKind::PhoneAction, got {other:?}"),
    }

    let mut harness = make_live_harness(ctrl);
    step_n(&mut harness, 3);

    // Click "Add to Contacts" -> pushes pre-filled AddContactBox.
    harness.get_by_label("Add to Contacts").click();
    step_n(&mut harness, 3);
    match harness.state().box_host().top_kind() {
        Some(BoxKind::AddContact(ac)) => {
            assert_eq!(ac.first_name, "Devon");
            assert_eq!(ac.last_name, "Demo");
            assert_eq!(ac.phone, "+1 555 010 1234");
        }
        other => panic!("expected prefilled BoxKind::AddContact, got {other:?}"),
    }

    // Close AddContact back to PhoneAction, then click "View Profile".
    harness.get_by_label(&tg_lang::tr::lng_cancel()).click();
    step_n(&mut harness, 3);
    harness.get_by_label("View Profile").click();
    step_n(&mut harness, 3);
    assert!(harness.state().box_host().is_empty());

    // 2. Bank card click handler -> `payments.getBankCardData` -> BankCardActionBox -> Open URL.
    fake.enqueue_ok(
        tl_fns::payments::GetBankCardData::CONSTRUCTOR_ID,
        &tl_enums::payments::BankCardData::BankCardData(tl_types::payments::BankCardData {
            title: "Demo Bank".to_owned(),
            open_urls: vec![tl_enums::BankCardOpenUrl::BankCardOpenUrl(
                tl_types::BankCardOpenUrl {
                    url: "https://example.invalid/pay-card".to_owned(),
                    name: "Demo Bank App".to_owned(),
                },
            )],
        }),
    );

    harness
        .state_mut()
        .open_link("bankcard:4000 1234 5678 9010");
    assert_eq!(
        fake.invoked_count(tl_fns::payments::GetBankCardData::CONSTRUCTOR_ID),
        1
    );
    match harness.state().box_host().top_kind() {
        Some(BoxKind::BankCardAction(state)) => {
            assert_eq!(state.number, "4000 1234 5678 9010");
            assert_eq!(state.title.as_deref(), Some("Demo Bank"));
            assert_eq!(state.open_urls.len(), 1);
        }
        other => panic!("expected BoxKind::BankCardAction, got {other:?}"),
    }

    step_n(&mut harness, 3);
    harness.get_by_label("Open in Demo Bank App").click();
    step_n(&mut harness, 3);
    assert!(harness.state().box_host().is_empty());
    assert!(harness.state_mut().take_webview_requests().contains(
        &WebViewHostRequest::OpenExternalUrl("https://example.invalid/pay-card".to_owned())
    ));

    // 3. Bank card lookup failure still opens BankCardActionBox with Copy available.
    fake.enqueue_err(
        tl_fns::payments::GetBankCardData::CONSTRUCTOR_ID,
        InvocationError::Dropped,
    );
    harness
        .state_mut()
        .open_link("bankcard:5100 0000 0000 0000");
    step_n(&mut harness, 3);
    assert!(matches!(
        harness.state().box_host().top_kind(),
        Some(BoxKind::BankCardAction(s)) if s.lookup_error.is_some()
    ));

    let ctrl = harness.into_state();
    shutdown_runtime(ctrl, runtime);
}

#[test]
fn moderate_action_only_for_group_participant_messages() {
    use tg_app::history::backend::{HistoryMessage, HistoryPeerInfo, MessageExtra};
    use tg_app::history::inner::can_moderate_message;
    use tg_app::stand_in::types::{PeerKind, PeerSummary};

    let summary = |id: PeerId, kind: PeerKind| PeerSummary {
        id,
        kind,
        title: "Sample Chat".to_owned(),
        status: String::new(),
        username: None,
        phone_or_link: None,
        about: String::new(),
        initials: "SC".to_owned(),
        avatar_rgb: [0x40, 0x80, 0xC0],
        muted: false,
        can_send: true,
        is_saved_messages: false,
    };
    let message = |peer_id: PeerId, from_id: PeerId, outgoing: bool| HistoryMessage {
        id: MsgId(5),
        peer_id,
        from_id,
        sender_name: "Sample Member".to_owned(),
        date_unix: 1_700_000_000,
        outgoing,
        is_post: false,
        text: tg_text::TextWithEntities::simple("hello"),
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
    };

    let group = PeerId::from_channel(830_001);
    let member = PeerId::from_user(830_002);
    let group_info = HistoryPeerInfo::from_summary(summary(group, PeerKind::Group));
    assert!(can_moderate_message(
        &message(group, member, false),
        &group_info
    ));
    // Own messages, posts authored by the group itself and broadcast channels: no Moderate.
    assert!(!can_moderate_message(
        &message(group, member, true),
        &group_info
    ));
    assert!(!can_moderate_message(
        &message(group, group, false),
        &group_info
    ));
    let channel = PeerId::from_channel(830_003);
    let channel_info = HistoryPeerInfo::from_summary(summary(channel, PeerKind::Channel));
    assert!(!can_moderate_message(
        &message(channel, member, false),
        &channel_info
    ));
    let mut no_rights = group_info;
    no_rights.can_delete = false;
    assert!(!can_moderate_message(
        &message(group, member, false),
        &no_rights
    ));
}
