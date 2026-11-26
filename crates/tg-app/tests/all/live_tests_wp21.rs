//! WP-21 integration tests:
//! - `FakeRpcClient` verification of `messages.sendMedia(InputMediaDocument)` request shape
//!   for stickers and GIFs (including `id`, `access_hash`, `file_reference`, `update_stickersets_order`,
//!   `reply_to`, `silent`), plus mock-mode compatibility;
//! - Live export & cloud themes host wiring on session start, account switch, and logout;
//! - Settings → Advanced → Export, Info 3-dots "Export chat history", Settings → Chat
//!   "Theme Studio / Cloud Wallpapers" entry points;
//! - `StickerSetBox` preview grid + Install/Remove RPCs for `tg://addstickers` / `tg://addemoji`,
//!   and `tg://addtheme` / `tg://bg` routing into `AppThemesHost`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{Pos2, RawInput, Rect, Vec2};
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_chat::{DocumentId as ChatDocId, StickerSetId};
use tg_core::{CoreSettings, GlobalConfig};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::theme::DayPalette;
use tokio::sync::broadcast;

use tg_app::boxes::BoxKind;
use tg_app::boxes::sticker_set::StickerSetBoxState;
use tg_app::chat_helpers::backend::ChatHelpersBackend;
use tg_app::export::{AppExportHost, ExportTarget};
use tg_app::info::{InfoWrapEvent, InfoWrapWidget};
use tg_app::live::export::{
    clear_live_export_hooks, ensure_live_export_hooks, install_live_export_hooks,
    sync_live_export_hooks,
};
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::live::session::LiveSession;
use tg_app::live::themes::{
    clear_live_themes_hooks, dispatch_themes_deep_link_overlay, ensure_live_themes_hooks,
    install_live_themes_hooks, sync_live_themes_hooks,
};
use tg_app::session_api::AppSession;
use tg_app::settings::advanced::{AdvancedState, AdvancedSubPanel, render_advanced_section};
use tg_app::settings::chat::{ChatSettingsState, render_chat_settings_section};
use tg_app::settings::codes::DebugOptions;
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::theme::ThemeMode;
use tg_app::stand_in::types::{ConnectionStatus, MsgId, PeerId};
use tg_app::startup::updater::UpdateSnapshot;
use tg_app::themes::{AppThemesHost, ThemesTab};
use tg_app::window::adaptive::{ChatLayout, ColumnLayout, WindowLayout};
use tg_app::window::controller::WindowController;
use tg_app::window::deep_links::DeepLinkOverlayRequest;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);
type CapturedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

/// In-process fake RPC client that records raw serialized request bytes for every call
/// so tests can deserialize and assert exact TL request shapes.
#[derive(Clone)]
struct RecordingFakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    captured_requests: CapturedRequests,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl RecordingFakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            captured_requests: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn captured_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.captured_requests
            .lock()
            .expect("lock captured")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }

    fn invoked_count(&self, ctor: u32) -> usize {
        self.captured_for(ctor).len()
    }
}

impl RpcClient for RecordingFakeRpc {
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
        self.captured_requests
            .lock()
            .expect("lock captured")
            .push((ctor, bytes));

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

fn synthetic_sticker_document(
    doc_id: i64,
    access_hash: i64,
    file_ref: Vec<u8>,
    alt: &str,
    set_id: i64,
    set_hash: i64,
    is_emoji: bool,
) -> tl_enums::Document {
    let attr = if is_emoji {
        tl_enums::DocumentAttribute::CustomEmoji(tl_types::DocumentAttributeCustomEmoji {
            free: true,
            text_color: false,
            alt: alt.to_owned(),
            stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id,
                access_hash: set_hash,
            }),
        })
    } else {
        tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
            mask: false,
            alt: alt.to_owned(),
            stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id,
                access_hash: set_hash,
            }),
            mask_coords: None,
        })
    };
    tl_enums::Document::Document(tl_types::Document {
        id: doc_id,
        access_hash,
        file_reference: file_ref,
        date: now_unix_i32(),
        mime_type: "image/webp".to_owned(),
        size: 16_384,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 512,
                h: 512,
            }),
            attr,
        ],
    })
}

fn synthetic_gif_document(
    doc_id: i64,
    access_hash: i64,
    file_ref: Vec<u8>,
    file_name: &str,
) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id: doc_id,
        access_hash,
        file_reference: file_ref,
        date: now_unix_i32(),
        mime_type: "video/mp4".to_owned(),
        size: 64_000,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
                round_message: false,
                supports_streaming: true,
                nosound: true,
                duration: 2.5,
                w: 320,
                h: 240,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            }),
            tl_enums::DocumentAttribute::Animated,
            tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
                file_name: file_name.to_owned(),
            }),
        ],
    })
}

fn boot_live_session(
    self_uid: i64,
    peer_uid: i64,
) -> (
    LiveSession,
    RecordingFakeRpc,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
) {
    let now = now_unix_i32();
    let fake = RecordingFakeRpc::new(2);
    let self_user = synthetic_user(self_uid, "Self", "self_wp21", true);
    let peer_user = synthetic_user(peer_uid, "Peer", "peer_wp21", false);

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

    // Seed peer in session store so `input_peer_for` resolves with real `access_hash`.
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(&mut s, &[tl_enums::User::User(peer_user)], &[]);
    });

    let live = LiveSession::new(Arc::clone(&runtime), self_uid as u64);
    (live, fake, runtime)
}

fn shutdown_rt(runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>) {
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

fn sent_updates_response(peer_uid: i64, self_uid: i64, msg_id: i32) -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: tl_enums::Message::Message(tl_types::Message {
                out: true,
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
                id: msg_id,
                from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: self_uid,
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
                date: now_unix_i32(),
                message: String::new(),
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
            }),
            pts: 10,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: now_unix_i32(),
        seq: 2,
    })
}

// =============================================================================
// 1. Real Sticker & GIF sending via messages.sendMedia(InputMediaDocument)
// =============================================================================

#[test]
fn live_send_sticker_and_gif_emit_send_media_input_document_with_file_reference() {
    let self_uid = 810_001i64;
    let peer_uid = 810_002i64;
    let peer_id = PeerId::from_user(peer_uid as u64);
    let (mut live, fake, runtime) = boot_live_session(self_uid, peer_uid);

    // 1. Load a sticker pack containing document #9001 with explicit access_hash + file_reference.
    let sticker_doc_id = 9_001i64;
    let sticker_access_hash = 123_456_789i64;
    let sticker_file_ref = vec![0xAA, 0xBB, 0xCC, 0xDD];
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
                installed_date: Some(now_unix_i32()),
                id: 50_001,
                access_hash: 50_002,
                title: "Orbit Stickers".to_owned(),
                short_name: "OrbitStickers".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 1,
                hash: 11,
            }),
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "🚀".to_owned(),
                documents: vec![sticker_doc_id],
            })],
            keywords: Vec::new(),
            documents: vec![synthetic_sticker_document(
                sticker_doc_id,
                sticker_access_hash,
                sticker_file_ref.clone(),
                "🚀",
                50_001,
                50_002,
                false,
            )],
        }),
    );
    live.fetch_sticker_set(
        tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
            short_name: "OrbitStickers".to_owned(),
        }),
        0,
    )
    .expect("fetch_sticker_set");

    // 2. Send sticker through AppSession::send_sticker and inspect exact SendMedia wire shape.
    fake.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &sent_updates_response(peer_uid, self_uid, 501),
    );
    let sent_sticker = AppSession::send_sticker(
        &mut live,
        peer_id,
        ChatDocId(sticker_doc_id as u64),
        Some(MsgId(42)),
        true,
    )
    .expect("send_sticker should succeed");
    assert_eq!(sent_sticker.id, MsgId(501));

    let send_media_calls = fake.captured_for(tl_fns::messages::SendMedia::CONSTRUCTOR_ID);
    assert_eq!(send_media_calls.len(), 1);
    let sticker_req = tl_fns::messages::SendMedia::from_bytes(&send_media_calls[0])
        .expect("deserialize sticker SendMedia");
    assert!(sticker_req.silent);
    assert!(sticker_req.update_stickersets_order);
    assert!(sticker_req.message.is_empty());
    assert_ne!(sticker_req.random_id, 0);
    match sticker_req.peer {
        tl_enums::InputPeer::User(u) => {
            assert_eq!(u.user_id, peer_uid);
            assert_eq!(u.access_hash, peer_uid * 100 + 7);
        }
        other => panic!("expected InputPeer::User, got {other:?}"),
    }
    match sticker_req.reply_to {
        Some(tl_enums::InputReplyTo::Message(r)) => assert_eq!(r.reply_to_msg_id, 42),
        other => panic!("expected InputReplyTo::Message(42), got {other:?}"),
    }
    match sticker_req.media {
        tl_enums::InputMedia::Document(doc) => {
            assert!(!doc.spoiler);
            match doc.id {
                tl_enums::InputDocument::InputDocument(id) => {
                    assert_eq!(id.id, sticker_doc_id);
                    assert_eq!(id.access_hash, sticker_access_hash);
                    assert_eq!(id.file_reference, sticker_file_ref);
                }
                other => panic!("expected InputDocument::InputDocument, got {other:?}"),
            }
        }
        other => panic!("expected InputMedia::Document for sticker, got {other:?}"),
    }

    // 3. Load saved GIFs containing document #9002 with explicit access_hash + file_reference.
    let gif_doc_id = 9_002i64;
    let gif_access_hash = 987_654_321i64;
    let gif_file_ref = vec![0x11, 0x22, 0x33, 0x44, 0x55];
    fake.enqueue_ok(
        tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedGifs::SavedGifs(tl_types::messages::SavedGifs {
            hash: 77,
            gifs: vec![synthetic_gif_document(
                gif_doc_id,
                gif_access_hash,
                gif_file_ref.clone(),
                "celebrate.mp4",
            )],
        }),
    );
    // The first call starts the background refresh; the list is there once it finishes.
    let _ = ChatHelpersBackend::saved_gifs(&live);
    assert!(live.wait_panel_background_idle(std::time::Duration::from_secs(10)));
    let gifs = ChatHelpersBackend::saved_gifs(&live);
    assert_eq!(gifs.len(), 1);
    assert_eq!(gifs[0].id, ChatDocId(gif_doc_id as u64));

    // 4. Send GIF through AppSession::send_gif and inspect exact SendMedia wire shape.
    fake.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &sent_updates_response(peer_uid, self_uid, 502),
    );
    let sent_gif = AppSession::send_gif(
        &mut live,
        peer_id,
        ChatDocId(gif_doc_id as u64),
        "celebrate",
        None,
        false,
    )
    .expect("send_gif should succeed");
    assert_eq!(sent_gif.id, MsgId(502));

    let send_media_calls = fake.captured_for(tl_fns::messages::SendMedia::CONSTRUCTOR_ID);
    assert_eq!(send_media_calls.len(), 2);
    let gif_req = tl_fns::messages::SendMedia::from_bytes(&send_media_calls[1])
        .expect("deserialize gif SendMedia");
    assert!(!gif_req.silent);
    assert!(!gif_req.update_stickersets_order);
    assert!(gif_req.reply_to.is_none());
    assert!(gif_req.message.is_empty());
    match gif_req.media {
        tl_enums::InputMedia::Document(doc) => {
            assert!(!doc.spoiler);
            match doc.id {
                tl_enums::InputDocument::InputDocument(id) => {
                    assert_eq!(id.id, gif_doc_id);
                    assert_eq!(id.access_hash, gif_access_hash);
                    assert_eq!(id.file_reference, gif_file_ref);
                }
                other => panic!("expected InputDocument::InputDocument, got {other:?}"),
            }
        }
        other => panic!("expected InputMedia::Document for GIF, got {other:?}"),
    }

    // Plain-text SendMessage must NEVER have been invoked for stickers or GIFs.
    assert_eq!(
        fake.invoked_count(tl_fns::messages::SendMessage::CONSTRUCTOR_ID),
        0
    );

    drop(live);
    shutdown_rt(runtime);
}

#[test]
fn mock_mode_sticker_and_gif_sending_keeps_working() {
    let mut mock = MockSession::new(true);
    let peer = tg_app::stand_in::mock_session::session_peers::USER_MIRA;
    let sticker_msg = AppSession::send_sticker(&mut mock, peer, ChatDocId(101), None, false)
        .expect("mock send_sticker");
    assert!(!sticker_msg.text.text.is_empty());
    let gif_msg = AppSession::send_gif(&mut mock, peer, ChatDocId(202), "Wave", None, false)
        .expect("mock send_gif");
    assert!(gif_msg.text.text.contains("Wave"));
}

// =============================================================================
// 2. Live Export & Cloud Themes Host Lifecycle + UI Entry Points
// =============================================================================

#[test]
fn live_export_and_themes_hooks_install_rebind_on_switch_and_clear_on_logout() {
    let (live_a, _fake_a, rt_a) = boot_live_session(820_001, 820_002);
    let (live_b, _fake_b, rt_b) = boot_live_session(830_001, 830_002);

    let ctx = egui::Context::default();
    let mode = ThemeMode::Day;

    // Initially offline/demo.
    assert!(!AppExportHost::is_live_mode(&ctx, mode));
    assert!(!AppThemesHost::has_backend(&ctx));

    // Install for session A (live session start).
    install_live_export_hooks(&ctx, mode, &live_a);
    install_live_themes_hooks(&ctx, &live_a);
    assert!(AppExportHost::is_live_mode(&ctx, mode));
    assert!(AppThemesHost::has_backend(&ctx));
    let key_a = Arc::as_ptr(live_a.bridge_state()) as usize;
    assert_eq!(AppExportHost::bound_session_key(&ctx, mode), Some(key_a));
    assert_eq!(AppThemesHost::bound_session_key(&ctx), Some(key_a));

    // Idempotent ensure keeps session A bound.
    ensure_live_export_hooks(&ctx, mode, &live_a);
    ensure_live_themes_hooks(&ctx, &live_a);
    assert_eq!(AppExportHost::bound_session_key(&ctx, mode), Some(key_a));

    // Account switch A -> B clears and rebinds to session B.
    sync_live_export_hooks(Some(&ctx), mode, Some(&live_b));
    sync_live_themes_hooks(Some(&ctx), Some(&live_b));
    let key_b = Arc::as_ptr(live_b.bridge_state()) as usize;
    assert_ne!(key_a, key_b);
    assert!(AppExportHost::is_live_mode(&ctx, mode));
    assert!(AppThemesHost::has_backend(&ctx));
    assert_eq!(AppExportHost::bound_session_key(&ctx, mode), Some(key_b));
    assert_eq!(AppThemesHost::bound_session_key(&ctx), Some(key_b));

    // Logout clears both hosts cleanly back to offline/demo state.
    clear_live_export_hooks(&ctx, mode);
    clear_live_themes_hooks(&ctx);
    assert!(!AppExportHost::is_live_mode(&ctx, mode));
    assert!(!AppThemesHost::has_backend(&ctx));
    assert_eq!(AppExportHost::bound_session_key(&ctx, mode), None);
    assert_eq!(AppThemesHost::bound_session_key(&ctx), None);

    // LiveActiveWorkspace::new with egui_ctx automatically wires both hosts on start.
    let tmp = TempDir::new().expect("tempdir");
    let ws = LiveActiveWorkspace::new(
        live_a,
        ShellSettings::default(),
        tmp.path().join("settings.json"),
        Some(&ctx),
    );
    assert!(AppExportHost::is_live_mode(&ctx, mode));
    assert!(AppThemesHost::has_backend(&ctx));
    drop(ws);
    drop(live_b);
    shutdown_rt(rt_a);
    shutdown_rt(rt_b);
}

fn run_headless_frame(ctx: &egui::Context, size: Vec2, mut f: impl FnMut(&mut egui::Ui)) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| f(ui));
    out.textures_delta.clear();
}

#[test]
fn settings_export_theme_studio_and_info_export_chat_history_are_wired() {
    // 1. Settings -> Advanced -> Export opens AppExportHost full account export.
    let ctx_exp = egui::Context::default();
    let mut adv_state = AdvancedState {
        panel: AdvancedSubPanel::ExportData,
        ..AdvancedState::default()
    };
    let mut core = CoreSettings::new();
    let mut global = GlobalConfig::default();
    let mut debug_opts = DebugOptions::default();
    let update_snap = UpdateSnapshot::default();
    let mut settings_backend = MockSettingsBackend::new();
    let colors = DayPalette::new();

    run_headless_frame(&ctx_exp, Vec2::new(600.0, 1400.0), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = render_advanced_section(
                ui,
                &mut adv_state,
                &mut core,
                &mut global,
                &mut debug_opts,
                &update_snap,
                ConnectionStatus::Connected,
                ColumnLayout {
                    filters_width: 0,
                    body_width: 600,
                    dialogs_width: 0,
                    chat_width: 600,
                    third_width: 0,
                    window_layout: WindowLayout::OneColumn,
                    chat_layout: ChatLayout::Normal,
                    dialogs_small_column: false,
                },
                &mut settings_backend,
                &colors,
            );
        });
    });
    // Also verify opening AppExportHost directly activates the full account export panel.
    AppExportHost::open_full_export(&ctx_exp, ThemeMode::Day);
    assert!(AppExportHost::is_panel_visible(&ctx_exp, ThemeMode::Day));
    assert_eq!(
        AppExportHost::current_target(&ctx_exp, ThemeMode::Day),
        Some(ExportTarget::FullAccount)
    );

    // 2. Settings -> Chat renders the "Theme Studio / Cloud Wallpapers" entry and opens AppThemesHost.
    let ctx_chat = egui::Context::default();
    let mut chat_state = ChatSettingsState::default();
    let mut live_theme = LiveThemeColors::default();
    let mut rendered_theme_studio = false;
    run_headless_frame(&ctx_chat, Vec2::new(600.0, 1400.0), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = render_chat_settings_section(
                ui,
                &mut chat_state,
                &mut core,
                &mut global,
                &mut live_theme,
                &mut settings_backend,
                &colors,
            );
            rendered_theme_studio = true;
        });
    });
    assert!(rendered_theme_studio);
    assert!(!AppThemesHost::is_open(&ctx_chat));
    AppThemesHost::open(&ctx_chat, None);
    assert!(AppThemesHost::is_open(&ctx_chat));

    // 3. InfoWrapWidget 3-dots menu emits InfoWrapEvent::ExportChatHistory and WindowController opens SingleChat export.
    let self_uid = 840_001i64;
    let peer_uid = 840_002i64;
    let peer_id = PeerId::from_user(peer_uid as u64);
    let (live, _fake, runtime) = boot_live_session(self_uid, peer_uid);
    let tmp = TempDir::new().expect("tempdir");
    let mut ctrl = WindowController::with_session(
        live,
        ShellSettings::default(),
        tmp.path().join("settings.json"),
    );

    // Render InfoWrapWidget to verify the profile builds cleanly with the new menu item.
    let ctx_info = egui::Context::default();
    let mut info_widget = InfoWrapWidget::for_peer(peer_id);
    run_headless_frame(&ctx_info, Vec2::new(380.0, 720.0), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = info_widget.show(ui, ctrl.session_backend_mut(), true, &colors);
        });
    });

    // Triggering ExportChatHistory resolves the real TL InputPeer with access_hash into AppExportHost.
    let (input_peer, title) = AppSession::resolve_export_peer(ctrl.session_backend(), peer_id)
        .expect("resolve_export_peer");
    match &input_peer {
        tg_export::stand_in::InputPeer::User(u) => {
            assert_eq!(u.user_id, peer_uid);
            assert_eq!(u.access_hash, peer_uid * 100 + 7);
        }
        other => panic!("expected InputPeer::User with access_hash, got {other:?}"),
    }
    assert!(title.contains("Peer"));
    // Verify InfoWrapEvent variant exists and matches cleanly.
    let ev = InfoWrapEvent::ExportChatHistory(peer_id);
    assert_eq!(ev, InfoWrapEvent::ExportChatHistory(peer_id));
    AppExportHost::open_single_chat_export(&ctx_info, ThemeMode::Day, input_peer, title);
    assert!(AppExportHost::is_panel_visible(&ctx_info, ThemeMode::Day));
    assert!(matches!(
        AppExportHost::current_target(&ctx_info, ThemeMode::Day),
        Some(ExportTarget::SingleChat { .. })
    ));

    drop(ctrl);
    shutdown_rt(runtime);
}

// =============================================================================
// 3. StickerSetBox & Deep-Link Routing (`addstickers`, `addemoji`, `addtheme`, `bg`)
// =============================================================================

#[test]
fn sticker_set_box_deep_links_render_grid_and_install_remove_and_themes_bg_route_to_host() {
    let self_uid = 850_001i64;
    let peer_uid = 850_002i64;
    let (live, fake, runtime) = boot_live_session(self_uid, peer_uid);
    let tmp = TempDir::new().expect("tempdir");
    let mut ctrl = WindowController::with_session(
        live,
        ShellSettings::default(),
        tmp.path().join("settings.json"),
    );

    // 1. `tg://addstickers?set=CosmicPack` -> BoxKind::StickerSet with grid items + Install button.
    let set_id = 60_001i64;
    let set_hash = 60_002i64;
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
                id: set_id,
                access_hash: set_hash,
                title: "Cosmic Pack".to_owned(),
                short_name: "CosmicPack".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 2,
                hash: 5,
            }),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![
                synthetic_sticker_document(7001, 70011, vec![1, 2], "🚀", set_id, set_hash, false),
                synthetic_sticker_document(7002, 70022, vec![3, 4], "🌙", set_id, set_hash, false),
            ],
        }),
    );
    ctrl.open_link("tg://addstickers?set=CosmicPack");
    assert!(
        !ctrl.layer_stack().is_layer_shown(),
        "addstickers must open StickerSetBox, not ModalBox::Inform"
    );
    let mut sticker_box: StickerSetBoxState = match ctrl.box_host().top_kind() {
        Some(BoxKind::StickerSet(s)) => s.clone(),
        other => panic!("expected BoxKind::StickerSet, got {other:?}"),
    };
    assert_eq!(sticker_box.set_id, set_id as u64);
    assert_eq!(sticker_box.title, "Cosmic Pack");
    assert_eq!(sticker_box.items.len(), 2);
    assert!(!sticker_box.is_removable());
    assert!(
        sticker_box
            .primary_action_label()
            .contains("Add 2 Stickers")
    );

    // Render StickerSetBox body headlessly in egui.
    let ctx = egui::Context::default();
    let colors = DayPalette::new();
    run_headless_frame(&ctx, Vec2::new(420.0, 520.0), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            sticker_box.show_body(ui, &colors);
        });
    });

    // Install & Remove via AppSession -> messages.installStickerSet / uninstallStickerSet.
    fake.enqueue_ok(
        tl_fns::messages::InstallStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSetInstallResult::Success,
    );
    assert!(
        ctrl.session_backend_mut()
            .install_sticker_set_preview(set_id as u64, "CosmicPack")
            .expect("install_sticker_set_preview")
    );
    assert_eq!(
        fake.invoked_count(tl_fns::messages::InstallStickerSet::CONSTRUCTOR_ID),
        1
    );
    assert!(ChatHelpersBackend::is_set_installed(
        ctrl.session_backend(),
        StickerSetId(set_id as u64)
    ));

    fake.enqueue_ok(tl_fns::messages::UninstallStickerSet::CONSTRUCTOR_ID, &true);
    assert!(
        ctrl.session_backend_mut()
            .remove_sticker_set_preview(set_id as u64, "CosmicPack")
            .expect("remove_sticker_set_preview")
    );
    assert_eq!(
        fake.invoked_count(tl_fns::messages::UninstallStickerSet::CONSTRUCTOR_ID),
        1
    );

    // 2. `tg://addemoji?set=SparkleEmoji` -> BoxKind::StickerSet with is_emoji == true + Remove button when installed.
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: false,
                masks: false,
                emojis: true,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: Some(now_unix_i32()),
                id: 60_010,
                access_hash: 60_011,
                title: "Sparkle Emoji".to_owned(),
                short_name: "SparkleEmoji".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 1,
                hash: 6,
            }),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![synthetic_sticker_document(
                7010,
                70101,
                vec![9, 8],
                "✨",
                60_010,
                60_011,
                true,
            )],
        }),
    );
    ctrl.open_link("tg://addemoji?set=SparkleEmoji");
    match ctrl.box_host().top_kind() {
        Some(BoxKind::StickerSet(s)) => {
            assert!(s.is_emoji);
            assert!(s.is_removable());
            assert_eq!(s.primary_action_label(), "Remove Emoji");
            assert_eq!(s.share_url(), "https://t.me/addemoji/SparkleEmoji");
        }
        other => panic!("expected BoxKind::StickerSet for addemoji, got {other:?}"),
    }

    // 3. `tg://addtheme?slug=NordDark` & `tg://bg?...` route into AppThemesHost.
    fake.enqueue_ok(
        tl_fns::account::GetTheme::CONSTRUCTOR_ID,
        &tl_enums::Theme::Theme(tl_types::Theme {
            creator: false,
            default: false,
            for_chat: false,
            id: 61_001,
            access_hash: 61_002,
            slug: "NordDark".to_owned(),
            title: "Nord Dark".to_owned(),
            document: None,
            settings: None,
            emoticon: None,
            installs_count: Some(99),
        }),
    );
    ctrl.open_link("https://t.me/addtheme/NordDark");

    fake.enqueue_ok(
        tl_fns::account::GetWallPaper::CONSTRUCTOR_ID,
        &tl_enums::WallPaper::NoFile(tl_types::WallPaperNoFile {
            id: 71_001,
            default: false,
            dark: true,
            settings: None,
        }),
    );
    ctrl.open_link("https://t.me/bg/forest_wp?mode=blur+motion&intensity=40");
    ctrl.open_link("https://t.me/bg/112233-445566?rotation=90");

    let overlays = ctrl.take_deep_link_overlay_requests();
    assert_eq!(overlays.len(), 3);
    assert!(matches!(
        &overlays[0],
        DeepLinkOverlayRequest::OpenTheme { slug, title: Some(t) }
            if slug == "NordDark" && t == "Nord Dark"
    ));
    assert!(matches!(
        &overlays[1],
        DeepLinkOverlayRequest::OpenBackground {
            slug_or_color,
            is_color_or_gradient: false,
            blur: true,
            motion: true,
            intensity: Some(40),
            ..
        } if slug_or_color == "forest_wp"
    ));
    assert!(matches!(
        &overlays[2],
        DeepLinkOverlayRequest::OpenBackground {
            slug_or_color,
            is_color_or_gradient: true,
            rotation: Some(90),
            ..
        } if slug_or_color == "112233-445566"
    ));

    // Dispatching into AppThemesHost opens the matching tabs and records the slugs.
    let theme_ctx = egui::Context::default();
    assert!(dispatch_themes_deep_link_overlay(&theme_ctx, &overlays[0]));
    assert!(AppThemesHost::is_open(&theme_ctx));
    assert_eq!(AppThemesHost::active_tab(&theme_ctx), ThemesTab::Previews);
    assert_eq!(
        AppThemesHost::last_deep_link_theme_slug(&theme_ctx).as_deref(),
        Some("NordDark")
    );

    assert!(dispatch_themes_deep_link_overlay(&theme_ctx, &overlays[2]));
    assert_eq!(
        AppThemesHost::active_tab(&theme_ctx),
        ThemesTab::Backgrounds
    );
    assert_eq!(
        AppThemesHost::last_deep_link_background(&theme_ctx).as_deref(),
        Some("112233-445566")
    );

    drop(ctrl);
    shutdown_rt(runtime);
}
