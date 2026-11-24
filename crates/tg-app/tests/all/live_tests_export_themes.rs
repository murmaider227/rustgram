//! Integration tests for WP-11: live data export (`TlExportSource<LiveExportRpc>`),
//! live cloud themes & wallpapers (`ThemesStudio` + `ThemesBackend`), suggested folders
//! (`messages.getSuggestedDialogFilters`), and shareable folder links (`chatlists.*`).
//!
//! All tests run exclusively against synthetic users/data, in-process [`FakeRpcClient`],
//! and temporary directories. No real Telegram accounts or network calls are used.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::UserId as DataUserId;
use tg_export::{ExportTypes, MediaTypes, OutputFormat, Settings};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_theme::{Color, EmbeddedType, WallPaper, WallPaperFlags};
use tg_tl::{
    Deserializable, Identifiable, RawObject, RemoteCall, Serializable, enums as tl_enums,
    functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::export::{
    ExportManager, ExportModal, ExportScreen, FolderChooser, FolderOpener, SettingsStore,
    SettingsViewAction,
};
use tg_app::live::chatlists::{ChatlistInviteInfo, extract_chatlist_slug};
use tg_app::live::export::{LiveExportOptions, LiveSessionExportExt};
use tg_app::live::session::LiveSession;
use tg_app::live::themes::{
    CLOUD_THEME_FORMAT, CLOUD_THEME_MIME_TYPE, deserialize_tl_color, serialize_tl_color,
};
use tg_app::settings::backend::SettingsBackend;
use tg_app::stand_in::types::PeerId;
use tg_app::themes::{
    BackgroundApplyScope, ChatBackground, PatternKind, ThemesStudio, WallPaperLookup,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

// =============================================================================
// Synthetic TL builders & FakeRpcClient (self-contained for clean branch merge)
// =============================================================================

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

fn synthetic_message(
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

fn synthetic_dialog(peer_uid: i64, top_message: i32) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message,
        read_inbox_max_id: top_message,
        read_outbox_max_id: top_message,
        unread_count: 0,
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

fn synthetic_user_full(user_id: i64, bio: &str) -> tl_enums::UserFull {
    tl_enums::UserFull::UserFull(tl_types::UserFull {
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
        sponsored_enabled: false,
        can_view_revenue: false,
        bot_can_manage_emoji_status: false,
        display_gifts_button: false,
        noforwards_my_enabled: false,
        noforwards_peer_enabled: false,
        unofficial_security_risk: false,
        id: user_id,
        about: Some(bio.to_owned()),
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
    })
}

fn synthetic_document(id: i64, access_hash: i64, mime: &str, size: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash,
        file_reference: vec![1, 2, 3, 4],
        date: 1_750_000_000,
        mime_type: mime.to_owned(),
        size,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: Vec::new(),
    })
}

type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCall = (u32, Vec<u8>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .map(|(c, _)| *c)
            .collect()
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
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
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes.clone()));

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
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

/// Unwraps `invokeWithTakeout` (and nested `invokeWithMessagesRange`) to inspect the inner TL ctor.
fn unwrap_export_request(ctor: u32, raw: &[u8]) -> (bool, u32, Vec<u8>) {
    let mut wrapped_in_takeout = false;
    let mut cur_ctor = ctor;
    let mut cur_bytes = raw.to_vec();

    loop {
        if cur_ctor == tl_fns::InvokeWithTakeout::<RawObject>::CONSTRUCTOR_ID {
            // 4 bytes ctor + 8 bytes takeout_id + inner query bytes (>= 4 bytes)
            if cur_bytes.len() >= 16 {
                wrapped_in_takeout = true;
                cur_bytes = cur_bytes[12..].to_vec();
                cur_ctor =
                    u32::from_le_bytes([cur_bytes[0], cur_bytes[1], cur_bytes[2], cur_bytes[3]]);
                continue;
            }
        }
        if cur_ctor == tl_fns::InvokeWithMessagesRange::<RawObject>::CONSTRUCTOR_ID {
            // 4 bytes ctor + MessageRange (ctor 4 + min_date 4 + max_date 4 = 12) + inner query (>= 4 bytes)
            if cur_bytes.len() >= 20 {
                cur_bytes = cur_bytes[16..].to_vec();
                cur_ctor =
                    u32::from_le_bytes([cur_bytes[0], cur_bytes[1], cur_bytes[2], cur_bytes[3]]);
                continue;
            }
        }
        break;
    }
    (wrapped_in_takeout, cur_ctor, cur_bytes)
}

#[derive(Default)]
struct NoopFolderChooser;
impl FolderChooser for NoopFolderChooser {
    fn pick_folder(&self, _title: &str, _initial_dir: &str) -> Option<PathBuf> {
        None
    }
}

#[derive(Default)]
struct NoopFolderOpener;
impl FolderOpener for NoopFolderOpener {
    fn show_in_folder(&self, _path: &Path) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default, Clone)]
struct MemorySettingsStore {
    bytes: Arc<Mutex<Option<Vec<u8>>>>,
}

impl SettingsStore for MemorySettingsStore {
    fn load(&self) -> Settings {
        self.bytes
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|b| Settings::deserialize_qt(b)))
            .unwrap_or_default()
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        if let Ok(mut g) = self.bytes.lock() {
            *g = Some(settings.serialize_qt());
        }
        Ok(())
    }
}

struct TestHarness {
    self_uid: i64,
    bob_uid: i64,
    bob_peer: PeerId,
    self_user: tl_types::User,
    bob_user: tl_types::User,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
}

impl TestHarness {
    fn start() -> Self {
        let self_uid = 610_001i64;
        let bob_uid = 610_002i64;
        let self_user = synthetic_user(self_uid, "Nova", "nova_self", true);
        let bob_user = synthetic_user(bob_uid, "Orbit", "orbit_peer", false);
        let bob_top_msg = synthetic_message(
            10,
            bob_uid,
            bob_uid,
            false,
            1_750_000_000,
            "Live export test message from Orbit!",
        );
        let bob_dialog = synthetic_dialog(bob_uid, 10);

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user.clone())],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 50,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            }),
        );
        fake.enqueue_ok(
            tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
            &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                date: 1_750_000_001,
                seq: 1,
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: vec![bob_dialog],
                messages: vec![bob_top_msg],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(bob_user.clone())],
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
                    pts: 50,
                    qts: 0,
                    date: 1_750_000_001,
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

        let live = LiveSession::new(Arc::clone(&runtime), 1);

        Self {
            self_uid,
            bob_uid,
            bob_peer: PeerId::from_user(bob_uid as u64),
            self_user,
            bob_user,
            fake,
            runtime,
            live,
        }
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

fn install_takeout_handler(
    fake: &FakeRpcClient,
    self_user: tl_types::User,
    bob_user: tl_types::User,
    bob_messages: Vec<tl_enums::Message>,
    takeout_delay_once: Option<Arc<Mutex<bool>>>,
) -> Arc<Mutex<Vec<(bool, u32)>>> {
    let seen = Arc::new(Mutex::new(Vec::<(bool, u32)>::new()));
    let seen_cb = Arc::clone(&seen);
    let self_uid = self_user.id;
    let bob_uid = bob_user.id;
    let top_msg_id = bob_messages
        .iter()
        .filter_map(|m| match m {
            tl_enums::Message::Message(mm) => Some(mm.id),
            _ => None,
        })
        .max()
        .unwrap_or(1);

    fake.set_dynamic_handler(Arc::new(move |ctor, bytes| {
        let (in_takeout, inner_ctor, inner_bytes) = unwrap_export_request(ctor, bytes);
        seen_cb
            .lock()
            .expect("lock seen")
            .push((in_takeout, inner_ctor));

        if inner_ctor == tl_fns::account::InitTakeoutSession::CONSTRUCTOR_ID {
            if let Some(ref flag) = takeout_delay_once {
                let mut guard = flag.lock().expect("lock delay flag");
                if *guard {
                    *guard = false;
                    return Some(Err(InvocationError::Rpc(RpcError {
                        code: 420,
                        name: "TAKEOUT_INIT_DELAY_86400".to_owned(),
                        value: Some(86_400),
                    })));
                }
            }
            return Some(Ok(tl_enums::account::Takeout::Takeout(
                tl_types::account::Takeout { id: 912_345 },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::account::FinishTakeoutSession::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if inner_ctor == tl_fns::users::GetUsers::CONSTRUCTOR_ID {
            // Return both self and bob if requested; TlExportSource checks for self or single peer user.
            let users_req = tl_fns::users::GetUsers::from_bytes(&inner_bytes)
                .ok()
                .map(|r| r.id);
            let resp = match users_req.as_deref() {
                Some([tl_enums::InputUser::InputUser(u)]) if u.user_id == bob_uid => {
                    vec![tl_enums::User::User(bob_user.clone())]
                }
                _ => vec![tl_enums::User::User(self_user.clone())],
            };
            return Some(Ok(resp.to_bytes()));
        }
        if inner_ctor == tl_fns::users::GetFullUser::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::users::UserFull::UserFull(
                tl_types::users::UserFull {
                    full_user: synthetic_user_full(self_uid, "Live export synthetic bio"),
                    chats: Vec::new(),
                    users: vec![tl_enums::User::User(self_user.clone())],
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::contacts::GetSaved::CONSTRUCTOR_ID {
            return Some(Ok(Vec::<tl_enums::SavedContact>::new().to_bytes()));
        }
        if inner_ctor == tl_fns::contacts::GetContacts::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::contacts::Contacts::Contacts(
                tl_types::contacts::Contacts {
                    contacts: Vec::new(),
                    saved_count: 0,
                    users: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::contacts::GetTopPeers::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::contacts::TopPeers::TopPeers(
                tl_types::contacts::TopPeers {
                    categories: Vec::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::messages::GetSplitRanges::CONSTRUCTOR_ID {
            return Some(Ok(Vec::<tl_enums::MessageRange>::new().to_bytes()));
        }
        if inner_ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::messages::Dialogs::Dialogs(
                tl_types::messages::Dialogs {
                    dialogs: vec![synthetic_dialog(bob_uid, top_msg_id)],
                    messages: bob_messages.clone(),
                    chats: Vec::new(),
                    users: vec![
                        tl_enums::User::User(self_user.clone()),
                        tl_enums::User::User(bob_user.clone()),
                    ],
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::messages::GetHistory::CONSTRUCTOR_ID {
            // Decode offset_id to stop pagination after delivering the batch once.
            let mut returned_msgs = bob_messages.clone();
            if let Ok(req) = tl_fns::messages::GetHistory::from_bytes(&inner_bytes) {
                if req.offset_id > 0 && req.add_offset < 0 {
                    // Forward scan from offset_id: return messages strictly newer than offset_id
                    returned_msgs.retain(|m| match m {
                        tl_enums::Message::Message(mm) => mm.id > req.offset_id,
                        _ => false,
                    });
                } else if req.offset_id > 0 {
                    returned_msgs.retain(|m| match m {
                        tl_enums::Message::Message(mm) => mm.id < req.offset_id,
                        _ => false,
                    });
                }
            }
            return Some(Ok(tl_enums::messages::Messages::Messages(
                tl_types::messages::Messages {
                    messages: returned_msgs,
                    topics: Vec::new(),
                    chats: Vec::new(),
                    users: vec![
                        tl_enums::User::User(self_user.clone()),
                        tl_enums::User::User(bob_user.clone()),
                    ],
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::channels::GetLeftChannels::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::messages::Chats::Chats(
                tl_types::messages::Chats { chats: Vec::new() },
            )
            .to_bytes()));
        }
        None
    }));

    seen
}

fn poll_manager_until_stopped(manager: &mut ExportManager, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut now = 0.0_f64;
    while Instant::now() < deadline {
        manager.poll(now);
        now += 0.05;
        if manager.finished_state().is_some()
            || matches!(manager.screen(), ExportScreen::CriticalError(_))
            || manager.modal().is_some()
            || !manager.is_export_running()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

// =============================================================================
// 1. Live ExportManager -> TlExportSource<LiveExportRpc> (Full HTML+JSON, Single-Chat, Delay/Resume, Cancel)
// =============================================================================

#[test]
fn live_export_manager_runs_takeout_flow_writes_html_and_json_and_supports_single_chat_from_info() {
    let h = TestHarness::start();
    let export_dir = TempDir::new().expect("create temp dir for full export");
    let single_dir = TempDir::new().expect("create temp dir for single chat export");

    let bob_msgs = vec![
        synthetic_message(
            101,
            h.bob_uid,
            h.bob_uid,
            false,
            1_750_000_100,
            "Hello from live takeout test!",
        ),
        synthetic_message(
            102,
            h.bob_uid,
            h.self_uid,
            true,
            1_750_000_200,
            "Reply exported in HTML and JSON.",
        ),
    ];
    let seen = install_takeout_handler(
        &h.fake,
        h.self_user.clone(),
        h.bob_user.clone(),
        bob_msgs,
        None,
    );

    let store = MemorySettingsStore::default();
    let mut manager = ExportManager::with_dependencies_and_runtime(
        Box::new(NoopFolderChooser),
        Box::new(NoopFolderOpener),
        Box::new(store),
        export_dir.path().to_string_lossy().into_owned(),
        Duration::ZERO,
        Some(h.runtime.tokio_handle().clone()),
    );
    manager.set_live_export_starter(h.live.live_export_starter_with_options(LiveExportOptions {
        utc_offset_secs: 0,
        skip_path_normalization: true,
        internal_links_domain: "https://t.me/".to_owned(),
    }));
    assert!(manager.is_live_mode());

    // 1A. Full account export with HTML + JSON output
    manager.start_full_export();
    {
        let s = manager.settings_mut();
        s.path = export_dir.path().to_string_lossy().into_owned();
        s.force_sub_path = false;
        s.format = OutputFormat::HtmlAndJson;
        s.types = ExportTypes::PERSONAL_INFO | ExportTypes::PERSONAL_CHATS;
        s.media.types = MediaTypes::empty();
    }
    manager.handle_settings_actions(vec![SettingsViewAction::Start]);
    poll_manager_until_stopped(&mut manager, TEST_TIMEOUT);

    let finished = manager
        .finished_state()
        .expect("full live export should finish")
        .clone();
    assert!(
        Path::new(&finished.path).exists(),
        "expected finished entry file at {}",
        finished.path
    );
    let html_path = export_dir.path().join("export_results.html");
    let json_path = export_dir.path().join("result.json");
    assert!(html_path.exists(), "missing HTML export at {html_path:?}");
    assert!(json_path.exists(), "missing JSON export at {json_path:?}");
    let json_text = std::fs::read_to_string(&json_path).expect("read result.json");
    assert!(
        json_text.contains("Hello from live takeout test!"),
        "result.json missing live message text: {json_text}"
    );
    assert!(
        json_text.contains("Reply exported in HTML and JSON."),
        "result.json missing live reply text"
    );

    // Verify takeout RPC lifecycle (`initTakeoutSession` -> `invokeWithTakeout` -> `finishTakeoutSession`)
    let calls = seen.lock().expect("lock seen").clone();
    assert!(
        calls
            .iter()
            .any(|&(_, c)| c == tl_fns::account::InitTakeoutSession::CONSTRUCTOR_ID),
        "missing account.initTakeoutSession"
    );
    assert!(
        calls
            .iter()
            .any(|&(in_to, c)| in_to && c == tl_fns::messages::GetHistory::CONSTRUCTOR_ID),
        "messages.getHistory must be wrapped in invokeWithTakeout"
    );
    assert!(
        calls
            .iter()
            .any(|&(_, c)| c == tl_fns::account::FinishTakeoutSession::CONSTRUCTOR_ID),
        "missing account.finishTakeoutSession"
    );

    // Close finished full export panel before launching single-chat export from Info.
    manager.stop();
    assert!(!manager.is_active());

    // 1B. Single-chat export resolved from Info peer ID
    let (resolved_peer, resolved_title) = h
        .live
        .resolve_export_peer(h.bob_peer)
        .expect("resolve Bob peer for Info single-chat export");
    assert!(matches!(resolved_peer, tl_enums::InputPeer::User(_)));
    assert!(resolved_title.contains("Orbit"));

    h.live
        .open_single_chat_export_in_manager(&mut manager, h.bob_peer)
        .expect("open single chat export");
    // Re-apply skip_path_normalization so files land directly in single_dir.
    manager.set_live_export_starter(h.live.live_export_starter_with_options(LiveExportOptions {
        utc_offset_secs: 0,
        skip_path_normalization: true,
        internal_links_domain: "https://t.me/".to_owned(),
    }));
    {
        let s = manager.settings_mut();
        s.path = single_dir.path().to_string_lossy().into_owned();
        s.force_sub_path = false;
        s.format = OutputFormat::Json;
        s.media.types = MediaTypes::empty();
    }
    assert!(manager.settings().only_single_peer());
    assert_eq!(manager.panel_title(), "Chat export settings");
    manager.handle_settings_actions(vec![SettingsViewAction::Start]);
    poll_manager_until_stopped(&mut manager, TEST_TIMEOUT);

    let single_done = manager
        .finished_state()
        .expect("single-chat export should finish")
        .clone();
    assert!(Path::new(&single_done.path).exists());
    let single_json = std::fs::read_to_string(single_dir.path().join("result.json"))
        .expect("read single-chat result.json");
    assert!(single_json.contains("Hello from live takeout test!"));

    h.shutdown();
}

#[test]
fn live_export_takeout_delay_persists_and_resumes_cleanly_and_cancel_stops() {
    let h = TestHarness::start();
    let export_dir = TempDir::new().expect("create temp dir for delay/resume export");

    let delay_once = Arc::new(Mutex::new(true));
    let _seen = install_takeout_handler(
        &h.fake,
        h.self_user.clone(),
        h.bob_user.clone(),
        vec![synthetic_message(
            201,
            h.bob_uid,
            h.bob_uid,
            false,
            1_750_000_500,
            "Resumed after 24h takeout delay!",
        )],
        Some(delay_once),
    );

    let store = MemorySettingsStore::default();
    let mut manager = ExportManager::with_dependencies_and_runtime(
        Box::new(NoopFolderChooser),
        Box::new(NoopFolderOpener),
        Box::new(store.clone()),
        export_dir.path().to_string_lossy().into_owned(),
        Duration::ZERO,
        Some(h.runtime.tokio_handle().clone()),
    );
    manager.set_live_export_starter(h.live.live_export_starter_with_options(LiveExportOptions {
        utc_offset_secs: 0,
        skip_path_normalization: true,
        internal_links_domain: "https://t.me/".to_owned(),
    }));

    // Attempt 1 hits TAKEOUT_INIT_DELAY_86400
    manager.start_full_export();
    {
        let s = manager.settings_mut();
        s.path = export_dir.path().to_string_lossy().into_owned();
        s.force_sub_path = false;
        s.format = OutputFormat::Json;
        s.types = ExportTypes::PERSONAL_INFO;
        s.media.types = MediaTypes::empty();
    }
    manager.handle_settings_actions(vec![SettingsViewAction::Start]);
    poll_manager_until_stopped(&mut manager, TEST_TIMEOUT);

    let delay = manager
        .takeout_delay()
        .expect("expected TakeoutDelayInfo on TAKEOUT_INIT_DELAY_86400")
        .clone();
    assert_eq!(delay.delay_seconds, 86_400);
    assert_eq!(delay.hours, 24);
    assert!(matches!(
        manager.modal(),
        Some(ExportModal::Inform {
            close_panel_on_ok: true,
            ..
        })
    ));

    // User clicks OK -> panel closes, available_at remains persisted in SettingsStore
    manager.dismiss_inform_modal();
    assert!(!manager.is_active());
    assert!(store.load().available_at >= delay.available_at);

    // 24 hours elapse -> check_takeout_suggestion opens SuggestStart modal -> user resumes!
    assert!(manager.check_takeout_suggestion(delay.available_at + 5));
    assert!(matches!(
        manager.modal(),
        Some(ExportModal::SuggestStart { .. })
    ));
    manager.accept_suggest_start();
    assert!(manager.is_active());
    assert_eq!(*manager.screen(), ExportScreen::Settings);

    // User starts again -> succeeds on second attempt!
    {
        let s = manager.settings_mut();
        s.path = export_dir.path().to_string_lossy().into_owned();
        s.force_sub_path = false;
        s.format = OutputFormat::Json;
        s.types = ExportTypes::PERSONAL_INFO;
        s.media.types = MediaTypes::empty();
    }
    manager.handle_settings_actions(vec![SettingsViewAction::Start]);
    poll_manager_until_stopped(&mut manager, TEST_TIMEOUT);
    assert!(
        manager.finished_state().is_some(),
        "resumed live export should finish cleanly"
    );

    // Verify stop_with_confirmation + confirm_stop cleanly cancels an active run
    manager.stop();
    manager.start_full_export();
    manager.handle_settings_actions(vec![SettingsViewAction::Start]);
    manager.stop_with_confirmation();
    manager.confirm_stop();
    assert!(!manager.is_active());
    assert!(!manager.is_export_running());

    h.shutdown();
}

// =============================================================================
// 2. Live Cloud Themes & Wallpapers (`ThemesStudio` + `ThemesBackend`)
// =============================================================================

#[test]
fn live_cloud_themes_and_chat_themes_rpcs_work_through_themes_studio() {
    let h = TestHarness::start();

    // Build a valid minimal `.tdesktop-theme` payload so install_cloud_theme can download & parse it.
    let sample_palette_bytes = b"windowBg: #f7f4ec;\nwindowBgActive: #3366cc;\n".to_vec();

    let cloud_theme_tl = tl_enums::Theme::Theme(tl_types::Theme {
        creator: true,
        default: false,
        for_chat: false,
        id: 7001,
        access_hash: 77_001,
        slug: "sunset_aurora".to_owned(),
        title: "Sunset Aurora".to_owned(),
        document: Some(synthetic_document(
            8001,
            88_001,
            CLOUD_THEME_MIME_TYPE,
            sample_palette_bytes.len() as i64,
        )),
        settings: None,
        emoticon: None,
        installs_count: Some(12),
    });

    let chat_theme_tl = tl_enums::Theme::Theme(tl_types::Theme {
        creator: false,
        default: true,
        for_chat: true,
        id: 7002,
        access_hash: 77_002,
        slug: "ocean_chat".to_owned(),
        title: "Ocean".to_owned(),
        document: None,
        settings: Some(vec![
            tl_enums::ThemeSettings::ThemeSettings(tl_types::ThemeSettings {
                message_colors_animated: true,
                base_theme: tl_enums::BaseTheme::Day,
                accent_color: serialize_tl_color(Color::rgb(0x22, 0x88, 0xDD)),
                outbox_accent_color: Some(serialize_tl_color(Color::rgb(0xD8, 0xEE, 0xFF))),
                message_colors: Some(vec![serialize_tl_color(Color::rgb(0xC8, 0xE6, 0xFF))]),
                wallpaper: Some(tl_enums::WallPaper::NoFile(tl_types::WallPaperNoFile {
                    id: 9001,
                    default: false,
                    dark: false,
                    settings: Some(tl_enums::WallPaperSettings::WallPaperSettings(
                        tl_types::WallPaperSettings {
                            blur: false,
                            motion: true,
                            background_color: Some(serialize_tl_color(Color::rgb(
                                0xC6, 0xE6, 0xFA,
                            ))),
                            second_background_color: Some(serialize_tl_color(Color::rgb(
                                0x8E, 0xCA, 0xF0,
                            ))),
                            third_background_color: Some(serialize_tl_color(Color::rgb(
                                0xDF, 0xF2, 0xFE,
                            ))),
                            fourth_background_color: None,
                            intensity: Some(38),
                            rotation: Some(90),
                            emoticon: None,
                        },
                    )),
                })),
            }),
            tl_enums::ThemeSettings::ThemeSettings(tl_types::ThemeSettings {
                message_colors_animated: true,
                base_theme: tl_enums::BaseTheme::Night,
                accent_color: serialize_tl_color(Color::rgb(0x44, 0x99, 0xEE)),
                outbox_accent_color: Some(serialize_tl_color(Color::rgb(0x1E, 0x3A, 0x56))),
                message_colors: None,
                wallpaper: None,
            }),
        ]),
        emoticon: Some("🌊".to_owned()),
        installs_count: None,
    });

    let captured_format = Arc::new(Mutex::new(String::new()));
    let captured_format_cb = Arc::clone(&captured_format);
    let captured_upload_mime = Arc::new(Mutex::new(String::new()));
    let captured_upload_mime_cb = Arc::clone(&captured_upload_mime);
    let get_themes_calls = Arc::new(Mutex::new(0u32));
    let get_themes_calls_cb = Arc::clone(&get_themes_calls);
    let theme_bytes_for_dl = sample_palette_bytes.clone();

    h.fake.set_dynamic_handler(Arc::new(move |ctor, bytes| {
        if ctor == tl_fns::account::GetThemes::CONSTRUCTOR_ID {
            let req = tl_fns::account::GetThemes::from_bytes(bytes).expect("decode GetThemes");
            *captured_format_cb.lock().expect("lock format") = req.format;
            let mut n = get_themes_calls_cb.lock().expect("lock count");
            *n += 1;
            if req.hash == 4242 {
                return Some(Ok(tl_enums::account::Themes::NotModified.to_bytes()));
            }
            return Some(Ok(tl_enums::account::Themes::Themes(
                tl_types::account::Themes {
                    hash: 4242,
                    themes: vec![cloud_theme_tl.clone()],
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::account::GetChatThemes::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::account::Themes::Themes(
                tl_types::account::Themes {
                    hash: 5151,
                    themes: vec![chat_theme_tl.clone()],
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::account::GetWallPapers::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::account::WallPapers::WallPapers(
                tl_types::account::WallPapers {
                    hash: 1111,
                    wallpapers: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::account::UploadTheme::CONSTRUCTOR_ID {
            let req = tl_fns::account::UploadTheme::from_bytes(bytes).expect("decode UploadTheme");
            *captured_upload_mime_cb.lock().expect("lock mime") = req.mime_type;
            return Some(Ok(synthetic_document(
                8099,
                88_099,
                CLOUD_THEME_MIME_TYPE,
                2048,
            )
            .to_bytes()));
        }
        if ctor == tl_fns::account::CreateTheme::CONSTRUCTOR_ID {
            let req = tl_fns::account::CreateTheme::from_bytes(bytes).expect("decode CreateTheme");
            return Some(Ok(tl_enums::Theme::Theme(tl_types::Theme {
                creator: true,
                default: false,
                for_chat: false,
                id: 7050,
                access_hash: 77_050,
                slug: req.slug,
                title: req.title,
                document: Some(synthetic_document(
                    8099,
                    88_099,
                    CLOUD_THEME_MIME_TYPE,
                    2048,
                )),
                settings: None,
                emoticon: None,
                installs_count: Some(1),
            })
            .to_bytes()));
        }
        if ctor == tl_fns::account::UpdateTheme::CONSTRUCTOR_ID {
            let req = tl_fns::account::UpdateTheme::from_bytes(bytes).expect("decode UpdateTheme");
            assert_eq!(req.format, CLOUD_THEME_FORMAT);
            return Some(Ok(tl_enums::Theme::Theme(tl_types::Theme {
                creator: true,
                default: false,
                for_chat: false,
                id: 7050,
                access_hash: 77_050,
                slug: req.slug.unwrap_or_else(|| "updated_slug".to_owned()),
                title: req.title.unwrap_or_else(|| "Updated Title".to_owned()),
                document: Some(synthetic_document(
                    8099,
                    88_099,
                    CLOUD_THEME_MIME_TYPE,
                    2048,
                )),
                settings: None,
                emoticon: None,
                installs_count: Some(2),
            })
            .to_bytes()));
        }
        if ctor == tl_fns::account::InstallTheme::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FilePartial,
                mtime: 1_750_000_000,
                bytes: theme_bytes_for_dl.clone(),
            })
            .to_bytes()));
        }
        if ctor == tl_fns::messages::SetChatTheme::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_750_000_100,
                seq: 1,
            })
            .to_bytes()));
        }
        None
    }));

    let mut studio = ThemesStudio::new();
    studio.set_backend(h.live.themes_backend());
    studio.set_target_peer(h.bob_peer.0, "Orbit Tester");

    // 1. Refresh catalogs: getThemes ("tdesktop") + getChatThemes + getWallPapers
    studio
        .refresh_cloud_catalogs()
        .expect("refresh cloud catalogs");
    assert_eq!(*captured_format.lock().expect("lock"), CLOUD_THEME_FORMAT);
    assert_eq!(studio.cloud_themes().len(), 1);
    assert_eq!(studio.cloud_themes()[0].theme.slug, "sunset_aurora");
    assert!(studio.chat_theme_picker.cloud_synced);
    assert!(
        studio
            .chat_theme_picker
            .themes()
            .iter()
            .any(|t| t.token == "🌊" && t.name == "Ocean")
    );

    // Second refresh sends cached hash 4242 -> receives ThemesNotModified cleanly
    studio
        .refresh_cloud_catalogs()
        .expect("second refresh handles NotModified");
    assert_eq!(*get_themes_calls.lock().expect("lock"), 2);

    // 2. Install cloud theme (installTheme + download_theme_bytes + apply palette)
    let first_cloud = studio.cloud_themes()[0].clone();
    studio
        .install_cloud_theme(&first_cloud)
        .expect("install cloud theme");
    assert_eq!(
        studio.theme_manager.palette().window_bg,
        Color::rgb(0xF7, 0xF4, 0xEC)
    );

    // 3. Create new cloud theme (uploadTheme + createTheme)
    studio.editor.set_cloud(tg_theme::CloudTheme::default());
    studio.editor.cloud_save.title = "Nordic Breeze".to_owned();
    studio.editor.cloud_save.slug = "nordic_breeze_theme".to_owned();
    let created = studio
        .save_current_cloud_theme()
        .expect("create cloud theme");
    assert_eq!(created.theme.id, 7050);
    assert_eq!(
        *captured_upload_mime.lock().expect("lock"),
        CLOUD_THEME_MIME_TYPE
    );

    // 4. Update existing cloud theme (uploadTheme + updateTheme)
    studio.editor.cloud_save.title = "Nordic Breeze v2".to_owned();
    let updated = studio
        .save_current_cloud_theme()
        .expect("update cloud theme");
    assert_eq!(updated.theme.id, 7050);
    assert_eq!(updated.theme.title, "Nordic Breeze v2");

    // 5. Install embedded base theme (installTheme with base_theme)
    studio
        .install_embedded_theme(EmbeddedType::Night)
        .expect("install embedded night theme");

    // 6. Apply & clear per-chat theme (`messages.setChatTheme`)
    studio
        .apply_chat_theme_live(h.bob_peer.0, Some("🌊"))
        .expect("set chat theme");
    studio
        .apply_chat_theme_live(h.bob_peer.0, None)
        .expect("clear chat theme");

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::account::GetThemes::CONSTRUCTOR_ID,
        tl_fns::account::GetChatThemes::CONSTRUCTOR_ID,
        tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID,
        tl_fns::account::UploadTheme::CONSTRUCTOR_ID,
        tl_fns::account::CreateTheme::CONSTRUCTOR_ID,
        tl_fns::account::UpdateTheme::CONSTRUCTOR_ID,
        tl_fns::account::InstallTheme::CONSTRUCTOR_ID,
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        tl_fns::messages::SetChatTheme::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "missing expected cloud theme ctor 0x{expected:08x}"
        );
    }

    h.shutdown();
}

#[test]
fn live_wallpapers_rpcs_preserve_pattern_blur_motion_and_per_chat_overrides() {
    let h = TestHarness::start();

    let c1 = Color::rgb(0x14, 0x28, 0x3D);
    let c2 = Color::rgb(0x1E, 0x4D, 0x6B);
    let c3 = Color::rgb(0x12, 0x22, 0x33);
    assert_eq!(deserialize_tl_color(serialize_tl_color(c1)), c1);

    let pattern_wp_tl = tl_enums::WallPaper::WallPaper(tl_types::WallPaper {
        id: 500_001,
        creator: false,
        default: false,
        pattern: true,
        dark: true,
        access_hash: 999_001,
        slug: "midnight_waves".to_owned(),
        document: synthetic_document(600_001, 700_001, "application/x-tgwallpattern", 4096),
        settings: Some(tl_enums::WallPaperSettings::WallPaperSettings(
            tl_types::WallPaperSettings {
                blur: true,
                motion: true,
                background_color: Some(serialize_tl_color(c1)),
                second_background_color: Some(serialize_tl_color(c2)),
                third_background_color: Some(serialize_tl_color(c3)),
                fourth_background_color: None,
                intensity: Some(-45),
                rotation: Some(135),
                emoticon: None,
            },
        )),
    });

    let multi_wp_tl = tl_enums::WallPaper::NoFile(tl_types::WallPaperNoFile {
        id: 500_002,
        default: false,
        dark: false,
        settings: Some(tl_enums::WallPaperSettings::WallPaperSettings(
            tl_types::WallPaperSettings {
                blur: false,
                motion: false,
                background_color: Some(serialize_tl_color(Color::rgb(0xF3, 0xEC, 0xDF))),
                second_background_color: None,
                third_background_color: None,
                fourth_background_color: None,
                intensity: Some(0),
                rotation: None,
                emoticon: None,
            },
        )),
    });

    let uploaded_wp_tl = tl_enums::WallPaper::WallPaper(tl_types::WallPaper {
        id: 500_009,
        creator: true,
        default: false,
        pattern: false,
        dark: false,
        access_hash: 999_009,
        slug: "uploaded_custom_photo".to_owned(),
        document: synthetic_document(600_009, 700_009, "image/jpeg", 1024),
        settings: Some(tl_enums::WallPaperSettings::WallPaperSettings(
            tl_types::WallPaperSettings {
                blur: true,
                motion: false,
                background_color: None,
                second_background_color: None,
                third_background_color: None,
                fourth_background_color: None,
                intensity: Some(50),
                rotation: None,
                emoticon: None,
            },
        )),
    });

    let last_install_settings = Arc::new(Mutex::new(None::<tl_types::WallPaperSettings>));
    let last_install_settings_cb = Arc::clone(&last_install_settings);
    let last_set_chat_wp = Arc::new(Mutex::new(Vec::<(bool, bool)>::new()));
    let last_set_chat_wp_cb = Arc::clone(&last_set_chat_wp);
    let last_save_unsave = Arc::new(Mutex::new(Vec::<bool>::new()));
    let last_save_unsave_cb = Arc::clone(&last_save_unsave);

    h.fake.set_dynamic_handler(Arc::new(move |ctor, bytes| {
        if ctor == tl_fns::account::GetThemes::CONSTRUCTOR_ID
            || ctor == tl_fns::account::GetChatThemes::CONSTRUCTOR_ID
        {
            return Some(Ok(tl_enums::account::Themes::Themes(
                tl_types::account::Themes {
                    hash: 1,
                    themes: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::account::GetWallPapers::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::account::WallPapers::WallPapers(
                tl_types::account::WallPapers {
                    hash: 7777,
                    wallpapers: vec![pattern_wp_tl.clone()],
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::account::GetMultiWallPapers::CONSTRUCTOR_ID {
            return Some(Ok(vec![multi_wp_tl.clone()].to_bytes()));
        }
        if ctor == tl_fns::account::SaveWallPaper::CONSTRUCTOR_ID {
            let req =
                tl_fns::account::SaveWallPaper::from_bytes(bytes).expect("decode SaveWallPaper");
            last_save_unsave_cb
                .lock()
                .expect("lock save")
                .push(req.unsave);
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::account::InstallWallPaper::CONSTRUCTOR_ID {
            let req = tl_fns::account::InstallWallPaper::from_bytes(bytes)
                .expect("decode InstallWallPaper");
            let tl_enums::WallPaperSettings::WallPaperSettings(s) = req.settings;
            *last_install_settings_cb.lock().expect("lock install") = Some(s);
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::account::UploadWallPaper::CONSTRUCTOR_ID {
            return Some(Ok(uploaded_wp_tl.clone().to_bytes()));
        }
        if ctor == tl_fns::messages::SetChatWallPaper::CONSTRUCTOR_ID {
            let req = tl_fns::messages::SetChatWallPaper::from_bytes(bytes)
                .expect("decode SetChatWallPaper");
            last_set_chat_wp_cb
                .lock()
                .expect("lock set_chat_wp")
                .push((req.for_both, req.revert));
            return Some(Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_750_000_200,
                seq: 1,
            })
            .to_bytes()));
        }
        None
    }));

    let mut studio = ThemesStudio::new();
    studio.set_backend(h.live.themes_backend());
    studio.set_target_peer(h.bob_peer.0, "Orbit Tester");

    // 1. Fetch wallpapers (`account.getWallPapers`) and verify pattern/blur/motion/colors preserved
    studio.refresh_cloud_catalogs().expect("refresh wallpapers");
    assert!(studio.bg_picker.select_by_id("cloud_wp_500001"));
    assert!(studio.bg_picker.preview.config.wallpaper.is_pattern());
    assert!(studio.bg_picker.preview.config.wallpaper.blurred);
    assert!(studio.bg_picker.preview.config.motion);
    assert_eq!(studio.bg_picker.preview.config.pattern, PatternKind::Waves);
    assert_eq!(studio.bg_picker.preview.config.wallpaper.intensity, -45);
    assert_eq!(studio.bg_picker.preview.config.wallpaper.rotation, 135);
    assert_eq!(
        studio.bg_picker.preview.config.wallpaper.background_colors,
        vec![c1, c2, c3]
    );

    // 2. Install wallpaper globally (`account.installWallPaper`) with updated pattern/blur/motion
    studio.bg_picker.preview.set_blurred(true);
    studio.bg_picker.preview.set_motion(true);
    studio.bg_picker.preview.set_pattern_intensity(-30);
    studio
        .apply_preview_background_live(BackgroundApplyScope::AllChats)
        .expect("install wallpaper globally");
    let sent_settings = last_install_settings
        .lock()
        .expect("lock install")
        .clone()
        .expect("installWallPaper settings recorded");
    assert!(sent_settings.blur);
    assert!(sent_settings.motion);
    assert_eq!(sent_settings.intensity, Some(-30));

    // 3. Save & unsave wallpaper (`account.saveWallPaper`)
    studio
        .save_preview_wallpaper_live(false)
        .expect("save wallpaper");
    studio
        .save_preview_wallpaper_live(true)
        .expect("unsave wallpaper");
    assert_eq!(
        *last_save_unsave.lock().expect("lock save"),
        vec![false, true]
    );

    // 4. Resolve multiple wallpapers (`account.getMultiWallPapers`)
    let multi = studio
        .fetch_multi_wallpapers_live(&[
            WallPaperLookup::NoFile(500_002),
            WallPaperLookup::Slug("midnight_waves".to_owned()),
        ])
        .expect("getMultiWallPapers");
    assert_eq!(multi.len(), 1);
    assert!(multi[0].is_no_file);
    assert!(studio.bg_picker.select_by_id("cloud_wp_500002"));

    // 5. Upload custom image + apply per-chat (`upload.saveFilePart` + `account.uploadWallPaper` + `messages.setChatWallPaper`)
    let sample_img = image::RgbaImage::from_pixel(32, 32, image::Rgba([64, 128, 220, 255]));
    let mut png_buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(sample_img)
        .write_to(&mut png_buf, image::ImageFormat::Png)
        .expect("encode sample png");
    let mut custom_cfg = ChatBackground::load_config_from_bytes(&png_buf.into_inner())
        .expect("load custom photo background");
    custom_cfg.wallpaper = custom_cfg.wallpaper.with_blurred(true);
    studio.bg_picker.preview.set_config(custom_cfg);
    studio
        .apply_preview_background_live(BackgroundApplyScope::SingleChat {
            peer_id: h.bob_peer.0,
            for_both: true,
        })
        .expect("upload & set chat wallpaper");
    assert_eq!(
        studio
            .bg_picker
            .preview
            .cloud_record
            .as_ref()
            .map(|r| r.wallpaper.id),
        Some(500_009)
    );

    // 6. Revert per-chat wallpaper (`messages.setChatWallPaper(revert: true)`)
    studio
        .reset_chat_wallpaper_live(h.bob_peer.0)
        .expect("reset chat wallpaper");
    assert_eq!(
        *last_set_chat_wp.lock().expect("lock set_chat_wp"),
        vec![(true, false), (false, true)]
    );

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::account::GetWallPapers::CONSTRUCTOR_ID,
        tl_fns::account::GetMultiWallPapers::CONSTRUCTOR_ID,
        tl_fns::account::SaveWallPaper::CONSTRUCTOR_ID,
        tl_fns::account::InstallWallPaper::CONSTRUCTOR_ID,
        tl_fns::account::UploadWallPaper::CONSTRUCTOR_ID,
        tl_fns::messages::SetChatWallPaper::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "missing expected wallpaper ctor 0x{expected:08x}"
        );
    }

    h.shutdown();
}

// =============================================================================
// 3. Live Suggested Folders & Shareable Folder Links (`chatlists.*`)
// =============================================================================

#[test]
fn live_suggested_folders_and_chatlist_shareable_links_work_end_to_end() {
    let mut h = TestHarness::start();
    let bob_uid = h.bob_uid;
    let bob_user = h.bob_user.clone();

    assert_eq!(
        extract_chatlist_slug("https://t.me/addlist/abcDEF123?foo=1"),
        "abcDEF123"
    );
    assert_eq!(
        extract_chatlist_slug("tg://addlist?slug=xyz987&ref=app"),
        "xyz987"
    );
    assert_eq!(extract_chatlist_slug("rawSlug42"), "rawSlug42");

    let check_already = Arc::new(Mutex::new(false));
    let check_already_cb = Arc::clone(&check_already);

    h.fake.set_dynamic_handler(Arc::new(move |ctor, _bytes| {
        if ctor == tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::messages::DialogFilters::DialogFilters(
                tl_types::messages::DialogFilters {
                    tags_enabled: false,
                    filters: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::messages::GetSuggestedDialogFilters::CONSTRUCTOR_ID {
            let suggested = vec![tl_enums::DialogFilterSuggested::DialogFilterSuggested(
                tl_types::DialogFilterSuggested {
                    filter: tl_enums::DialogFilter::DialogFilter(tl_types::DialogFilter {
                        contacts: true,
                        non_contacts: true,
                        groups: false,
                        broadcasts: false,
                        bots: false,
                        exclude_muted: false,
                        exclude_read: true,
                        exclude_archived: true,
                        title_noanimate: false,
                        id: 0,
                        title: tl_enums::TextWithEntities::TextWithEntities(
                            tl_types::TextWithEntities {
                                text: "Unread".to_owned(),
                                entities: Vec::new(),
                            },
                        ),
                        emoticon: None,
                        color: None,
                        pinned_peers: Vec::new(),
                        include_peers: Vec::new(),
                        exclude_peers: Vec::new(),
                    }),
                    description: "All unread personal chats.".to_owned(),
                },
            )];
            return Some(Ok(suggested.to_bytes()));
        }
        if ctor == tl_fns::messages::UpdateDialogFilter::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::chatlists::ExportChatlistInvite::CONSTRUCTOR_ID {
            let resp = tl_enums::chatlists::ExportedChatlistInvite::ExportedChatlistInvite(
                tl_types::chatlists::ExportedChatlistInvite {
                    filter: tl_enums::DialogFilter::Chatlist(tl_types::DialogFilterChatlist {
                        has_my_invites: true,
                        title_noanimate: false,
                        id: 2,
                        title: tl_enums::TextWithEntities::TextWithEntities(
                            tl_types::TextWithEntities {
                                text: "Unread".to_owned(),
                                entities: Vec::new(),
                            },
                        ),
                        emoticon: None,
                        color: None,
                        pinned_peers: Vec::new(),
                        include_peers: vec![tl_enums::InputPeer::User(tl_types::InputPeerUser {
                            user_id: bob_uid,
                            access_hash: bob_uid * 1000 + 7,
                        })],
                    }),
                    invite: tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
                        tl_types::ExportedChatlistInvite {
                            title: "Team Links".to_owned(),
                            url: "https://t.me/addlist/teamShare99".to_owned(),
                            peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                                user_id: bob_uid,
                            })],
                        },
                    ),
                },
            );
            return Some(Ok(resp.to_bytes()));
        }
        if ctor == tl_fns::chatlists::CheckChatlistInvite::CONSTRUCTOR_ID {
            let is_already = *check_already_cb.lock().expect("lock check_already");
            if is_already {
                let resp = tl_enums::chatlists::ChatlistInvite::Already(
                    tl_types::chatlists::ChatlistInviteAlready {
                        filter_id: 2,
                        missing_peers: Vec::new(),
                        already_peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: bob_uid,
                        })],
                        chats: Vec::new(),
                        users: vec![tl_enums::User::User(bob_user.clone())],
                    },
                );
                return Some(Ok(resp.to_bytes()));
            }
            let resp = tl_enums::chatlists::ChatlistInvite::ChatlistInvite(
                tl_types::chatlists::ChatlistInvite {
                    title_noanimate: false,
                    title: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Shared Folder".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    emoticon: Some("📁".to_owned()),
                    peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: bob_uid,
                    })],
                    chats: Vec::new(),
                    users: vec![tl_enums::User::User(bob_user.clone())],
                },
            );
            return Some(Ok(resp.to_bytes()));
        }
        if ctor == tl_fns::chatlists::JoinChatlistInvite::CONSTRUCTOR_ID
            || ctor == tl_fns::chatlists::JoinChatlistUpdates::CONSTRUCTOR_ID
            || ctor == tl_fns::chatlists::LeaveChatlist::CONSTRUCTOR_ID
        {
            return Some(Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_750_000_300,
                seq: 1,
            })
            .to_bytes()));
        }
        if ctor == tl_fns::chatlists::GetChatlistUpdates::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::chatlists::ChatlistUpdates::ChatlistUpdates(
                tl_types::chatlists::ChatlistUpdates {
                    missing_peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: bob_uid,
                    })],
                    chats: Vec::new(),
                    users: vec![tl_enums::User::User(bob_user.clone())],
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::chatlists::HideChatlistUpdates::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    // 1. Suggested folders via SettingsBackend (`messages.getSuggestedDialogFilters`)
    let suggested = h.live.suggested_folders();
    assert_eq!(suggested.len(), 1);
    assert_eq!(suggested[0].folder.title, "Unread");
    assert_eq!(suggested[0].description, "All unread personal chats.");

    // Accept suggested folder -> persists via `messages.updateDialogFilter` and disappears from suggestions
    let accepted = h
        .live
        .accept_suggested_folder(0)
        .expect("accept suggested folder");
    assert_eq!(accepted.title, "Unread");
    assert!(accepted.id >= 2);
    assert!(h.live.suggested_folders().is_empty());

    // 2. Export shareable folder link (`chatlists.exportChatlistInvite`)
    let exported = h
        .live
        .export_chatlist_invite(accepted.id, "Team Links", &[h.bob_peer])
        .expect("export chatlist invite");
    assert_eq!(exported.slug, "teamShare99");
    assert_eq!(exported.peers, vec![h.bob_peer]);
    assert_eq!(h.live.cached_chatlist_links(accepted.id).len(), 1);

    // 3. Check new chatlist invite (`chatlists.checkChatlistInvite`)
    match h
        .live
        .check_chatlist_invite("https://t.me/addlist/teamShare99")
        .expect("check new chatlist invite")
    {
        ChatlistInviteInfo::New {
            title,
            emoticon,
            peers,
            ..
        } => {
            assert_eq!(title, "Shared Folder");
            assert_eq!(emoticon.as_deref(), Some("📁"));
            assert_eq!(peers, vec![h.bob_peer]);
        }
        other => panic!("expected ChatlistInviteInfo::New, got {other:?}"),
    }

    // 4. Join invite (`chatlists.joinChatlistInvite`)
    h.live
        .join_chatlist_invite("https://t.me/addlist/teamShare99", &[h.bob_peer])
        .expect("join chatlist invite");

    // 5. Check already-joined invite (`chatlists.checkChatlistInvite` -> Already)
    *check_already.lock().expect("lock") = true;
    match h
        .live
        .check_chatlist_invite("teamShare99")
        .expect("check already-joined invite")
    {
        ChatlistInviteInfo::Already {
            filter_id,
            already_peers,
            ..
        } => {
            assert_eq!(filter_id, 2);
            assert_eq!(already_peers, vec![h.bob_peer]);
        }
        other => panic!("expected ChatlistInviteInfo::Already, got {other:?}"),
    }

    // 6. Get & join/hide chatlist updates (`chatlists.getChatlistUpdates`)
    let updates = h
        .live
        .get_chatlist_updates(2)
        .expect("get chatlist updates");
    assert_eq!(updates.filter_id, 2);
    assert_eq!(updates.missing_peers, vec![h.bob_peer]);
    h.live
        .join_chatlist_updates(2, &[h.bob_peer])
        .expect("join chatlist updates");
    assert!(
        h.live
            .hide_chatlist_updates(2)
            .expect("hide chatlist updates")
    );

    // 7. Leave chatlist (`chatlists.leaveChatlist`)
    h.live
        .leave_chatlist(2, &[h.bob_peer])
        .expect("leave chatlist");
    assert!(h.live.cached_chatlist_links(2).is_empty());

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::messages::GetSuggestedDialogFilters::CONSTRUCTOR_ID,
        tl_fns::chatlists::ExportChatlistInvite::CONSTRUCTOR_ID,
        tl_fns::chatlists::CheckChatlistInvite::CONSTRUCTOR_ID,
        tl_fns::chatlists::JoinChatlistInvite::CONSTRUCTOR_ID,
        tl_fns::chatlists::GetChatlistUpdates::CONSTRUCTOR_ID,
        tl_fns::chatlists::JoinChatlistUpdates::CONSTRUCTOR_ID,
        tl_fns::chatlists::HideChatlistUpdates::CONSTRUCTOR_ID,
        tl_fns::chatlists::LeaveChatlist::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "missing expected chatlist/suggested folder ctor 0x{expected:08x}"
        );
    }

    // Verify all recorded payloads were valid non-empty serialized TL requests.
    assert!(h.fake.recorded_calls().iter().all(|(_, b)| b.len() >= 4));

    h.shutdown();
}

// Keep unused imports warning-free for WallPaperFlags reference check
#[test]
fn wallpaper_flags_and_color_serialization_round_trip() {
    let mut wp = WallPaper::new(12345);
    wp.flags = WallPaperFlags::PATTERN.union(WallPaperFlags::DARK);
    assert!(wp.is_pattern());
    assert!(wp.is_dark());
    let sample = Color::rgb(0xAB, 0xCD, 0xEF);
    assert_eq!(serialize_tl_color(sample), 0x00AB_CDEF);
    assert_eq!(deserialize_tl_color(0x00AB_CDEF), sample);
}
