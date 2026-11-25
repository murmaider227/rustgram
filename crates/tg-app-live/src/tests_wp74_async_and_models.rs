//! WP-74 verification suite (fake-only, zero real network traffic):
//! 1. Non-blocking async `LiveSession::sticker_media` prefetch off the UI thread:
//!    returns `None` immediately on cache miss without blocking the caller thread, deduplicates
//!    in-flight fetches per `ChatDocId`, calls `notify_ui()`, and serves cached `StickerMedia`
//!    on subsequent polls with zero extra RPCs.
//! 2. Live `ChatHelpersBackend::emoji_groups` backed by `messages.getEmojiGroups` with 32-bit
//!    hash caching (`NotModified` support) and TTL cache.
//! 3. Persisted trusted Mini App bots across `tg-storage` (`StoredSessionSettings`),
//!    `tg-session` (`AccountManager`), and `AppSession` (`LiveSession`), plus wipe on logout.
//! 4. `chat_filter_to_dialog_folder` & `chat_filter_to_settings_folder` returning
//!    `FolderIconKind::Custom` / `FolderIcon::Custom` for a `"Work"` folder with explicit `always`
//!    peers (`Ui::ComputeDefaultFilterIcon`).
//! 5. `DialogItem` populating `is_contact` and `media_preview` (kind, stripped_thumb, service tint).
//! 6. `MediaItem` carrying `grouped_id` and tdesktop local-time `"today at H:mm"` /
//!    `"yesterday at H:mm"` / `"DD.MM.YY at H:mm"` date formatting.
//! 7. Stage-3 local-time & tdesktop date formatting rules (`unix_to_local_datetime`,
//!    `format_short_time`, `format_dialogs_date_at`, `format_date_time_at`, `format_read_date_at`).

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{ApiCredentials, DcId, InvocationError, RpcClient};
use tg_app_core::chat_helpers::backend::{ChatHelpersBackend, EmojiGroupInfo, StickerMedia};
use tg_app_core::dialogs::backend::{DialogListQuery, DialogMediaPreviewKind, FolderIconKind};
use tg_app_core::session_api::AppSession;
use tg_app_core::settings::backend::FolderIcon;
use tg_app_core::stand_in::types::{FilterId, PeerId as UiPeerId};
use tg_chat::DocumentId as ChatDocId;
use tg_data::{
    ChatFilter, ChatFilterFlags, DocumentData, DocumentKind, Message, MessageContent, MessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags, PhotoData, PhotoId, ServiceAction,
    Session, TextWithEntities as DataText, User, UserFlags, UserId as DataUserId,
};
use tg_history::LocalDateTime;
use tg_session::{AccountManager, SessionRuntime, SessionRuntimeConfig, WorkingDirOpenOptions};
use tg_storage::StoredSessionSettings;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::{Notify, broadcast};

use crate::convert::{
    chat_filter_to_dialog_folder, chat_filter_to_settings_folder, dialog_entry_from_session,
    dialog_item_from_session, format_date_time_at, format_dialogs_date_at, format_read_date_at,
    format_short_time, message_to_viewer_media_item_at, unix_to_local_datetime,
};
use crate::session::LiveSession;

// =============================================================================
// Process-wide UTC offset test guard (deterministic local-time tests)
// =============================================================================

static TZ_TEST_LOCK: Mutex<()> = Mutex::new(());

struct UtcOffsetGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl UtcOffsetGuard {
    fn set(offset_secs: i32) -> Self {
        let lock = TZ_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        tg_history::datetime::set_utc_offset_override(Some(offset_secs));
        Self { _lock: lock }
    }
}

impl Drop for UtcOffsetGuard {
    fn drop(&mut self) {
        tg_history::datetime::set_utc_offset_override(None);
    }
}

// =============================================================================
// Async-gateable FakeRpcClient for WP-74 tests
// =============================================================================

type RecordedRpc = (u32, Vec<u8>);
type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct GatedFakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    /// Optional async gate that `upload.getFile` awaits before replying.
    upload_gate: Arc<Mutex<Option<Arc<Notify>>>>,
    /// Optional signal notified as soon as `upload.getFile` starts executing on a background task.
    upload_started: Arc<Mutex<Option<Arc<Notify>>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl GatedFakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            upload_gate: Arc::new(Mutex::new(None)),
            upload_started: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn set_upload_gate(&self, gate: Option<Arc<Notify>>, started: Option<Arc<Notify>>) {
        *self.upload_gate.lock().expect("lock gate") = gate;
        *self.upload_started.lock().expect("lock started") = started;
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
    }
}

impl RpcClient for GatedFakeRpcClient {
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
            .push((ctor, bytes));

        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let started = self.upload_started.lock().expect("lock started").clone();
            if let Some(sig) = started {
                sig.notify_waiters();
            }
            let gate = self.upload_gate.lock().expect("lock gate").clone();
            if let Some(g) = gate {
                g.notified().await;
            }
        }

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

fn start_live_harness() -> (GatedFakeRpcClient, LiveSession, tokio::runtime::Runtime) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let fake = GatedFakeRpcClient::new();
    let erased = tg_app_core::core_types::erased_rpc::ErasedRpcClient::new(fake.clone());
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId(10_001),
            erased,
            rt.handle().clone(),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    let live = LiveSession::new(runtime, 74_001);
    (fake, live, rt)
}

fn synthetic_tgs_sticker_doc(doc_id: i64, set_id: i64, alt: &str) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id: doc_id,
        access_hash: doc_id * 13 + 7,
        file_reference: vec![1, 2, 3, 4],
        date: 1_764_000_000,
        mime_type: "application/x-tgsticker".to_owned(),
        size: 16,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                mask: false,
                alt: alt.to_owned(),
                stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                    id: set_id,
                    access_hash: 555,
                }),
                mask_coords: None,
            }),
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 512,
                h: 512,
            }),
        ],
    })
}

fn synthetic_sticker_set(
    id: i64,
    hash: i32,
    title: &str,
    short_name: &str,
) -> tl_enums::StickerSet {
    tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
        archived: false,
        official: false,
        masks: false,
        emojis: false,
        text_color: false,
        channel_emoji_status: false,
        creator: false,
        installed_date: Some(1_764_000_000),
        id,
        access_hash: 555,
        title: title.to_owned(),
        short_name: short_name.to_owned(),
        thumbs: None,
        thumb_dc_id: None,
        thumb_version: None,
        thumb_document_id: None,
        count: 1,
        hash,
    })
}

// =============================================================================
// 1. Async `sticker_media` prefetch off UI thread + in_flight dedup + notify_ui
// =============================================================================

#[test]
fn sticker_media_cache_miss_returns_none_without_blocking_dedups_and_notifies_ui() {
    let (fake, live, _rt) = start_live_harness();
    let set_id = 74_101i64;
    let doc_id = 74_201i64;
    let set_hdr = synthetic_sticker_set(set_id, 741, "Async Pack", "async_pack");
    let doc = synthetic_tgs_sticker_doc(doc_id, set_id, "🎉");

    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 741,
            sets: vec![set_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: set_hdr,
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![doc],
        }),
    );
    live.refresh_all_stickers(true)
        .expect("hydrate sticker set");
    fake.clear_recorded();

    // Gate upload.getFile behind an async Notify: if sticker_media blocked on the calling thread,
    // this test would hang forever because nobody can call `gate.notify_waiters()` until after
    // `live.sticker_media(...)` returns!
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    fake.set_upload_gate(Some(Arc::clone(&gate)), Some(Arc::clone(&started)));

    let tgs_payload = vec![0x1f, 0x8b, 0x08, 0x00, 0xAA, 0xBB, 0xCC, 0xDD];
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_764_000_000,
            bytes: tgs_payload.clone(),
        }),
    );

    let gen_before = live.bridge_state().generation.load(Ordering::Acquire);
    let t0 = Instant::now();
    // First cache-miss poll must return None immediately without blocking on the gated RPC.
    assert!(live.sticker_media(ChatDocId(doc_id as u64)).is_none());
    // Repeated polls while in-flight must also return None and NOT spawn duplicate downloads.
    assert!(live.sticker_media(ChatDocId(doc_id as u64)).is_none());
    assert!(live.sticker_media(ChatDocId(doc_id as u64)).is_none());
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "sticker_media blocked the calling thread"
    );

    // Wait briefly until the background task actually hits upload.getFile, then release the gate.
    let deadline = Instant::now() + Duration::from_secs(3);
    while fake
        .recorded_of(tl_fns::upload::GetFile::CONSTRUCTOR_ID)
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "background upload.getFile never started"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fake.recorded_of(tl_fns::upload::GetFile::CONSTRUCTOR_ID)
            .len(),
        1,
        "in_flight deduplication must spawn exactly one upload.getFile RPC"
    );

    gate.notify_waiters();

    // Poll until background task populates cache and wakes UI via notify_ui().
    let media = loop {
        if let Some(m) = live.sticker_media(ChatDocId(doc_id as u64)) {
            break m;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for background sticker_media completion"
        );
        std::thread::sleep(Duration::from_millis(5));
    };

    match media {
        StickerMedia::LottieTgs(bytes) => assert_eq!(bytes, tgs_payload),
        other => panic!("expected LottieTgs, got {other:?}"),
    }
    assert!(
        live.bridge_state().generation.load(Ordering::Acquire) > gen_before,
        "background prefetch must call bridge_state().notify_ui()"
    );

    // Subsequent cache hit executes zero additional RPCs.
    fake.clear_recorded();
    assert!(live.sticker_media(ChatDocId(doc_id as u64)).is_some());
    assert!(
        fake.recorded_of(tl_fns::upload::GetFile::CONSTRUCTOR_ID)
            .is_empty()
    );
}

// =============================================================================
// 2. Live `messages.getEmojiGroups` with hash caching
// =============================================================================

#[test]
fn emoji_groups_fetches_server_mood_groups_and_caches_hash() {
    let (fake, live, _rt) = start_live_harness();

    let server_groups =
        tl_enums::messages::EmojiGroups::EmojiGroups(tl_types::messages::EmojiGroups {
            hash: 98_765,
            groups: vec![
                tl_enums::EmojiGroup::EmojiGroup(tl_types::EmojiGroup {
                    title: "Love".to_owned(),
                    icon_emoji_id: 500_001,
                    emoticons: vec!["❤️".to_owned(), "😍".to_owned()],
                }),
                tl_enums::EmojiGroup::Greeting(tl_types::EmojiGroupGreeting {
                    title: "Greetings".to_owned(),
                    icon_emoji_id: 500_002,
                    emoticons: vec!["👋".to_owned(), "🤝".to_owned()],
                }),
                tl_enums::EmojiGroup::Premium(tl_types::EmojiGroupPremium {
                    title: "Premium".to_owned(),
                    icon_emoji_id: 500_003,
                }),
            ],
        });
    fake.enqueue_ok(
        tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID,
        &server_groups,
    );

    let groups = live.emoji_groups().expect("initial emoji_groups");
    assert_eq!(
        groups,
        vec![
            EmojiGroupInfo {
                icon_emoji_id: 500_001,
                title: "Love".to_owned(),
                emoticons: vec!["❤️".to_owned(), "😍".to_owned()],
                is_premium: false,
                is_greeting: false,
            },
            EmojiGroupInfo {
                icon_emoji_id: 500_002,
                title: "Greetings".to_owned(),
                emoticons: vec!["👋".to_owned(), "🤝".to_owned()],
                is_premium: false,
                is_greeting: true,
            },
            EmojiGroupInfo {
                icon_emoji_id: 500_003,
                title: "Premium".to_owned(),
                emoticons: Vec::new(),
                is_premium: true,
                is_greeting: false,
            },
        ]
    );

    // Verify wire request used initial hash 0.
    let sent = fake.recorded_of(tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID);
    assert_eq!(sent.len(), 1);
    let req0 = tl_fns::messages::GetEmojiGroups::from_bytes(&sent[0]).expect("decode req0");
    assert_eq!(req0.hash, 0);

    // Immediate second call serves from TTL cache with zero RPCs.
    fake.clear_recorded();
    let cached = live.emoji_groups().expect("cached emoji_groups");
    assert_eq!(cached, groups);
    assert!(
        fake.recorded_of(tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID)
            .is_empty()
    );

    // Forced refresh sends cached hash 98_765 and preserves groups on NotModified.
    fake.enqueue_ok(
        tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID,
        &tl_enums::messages::EmojiGroups::NotModified,
    );
    let refreshed = live
        .refresh_emoji_groups(true)
        .expect("not-modified refresh");
    assert_eq!(refreshed, groups);
    let sent_force = fake.recorded_of(tl_fns::messages::GetEmojiGroups::CONSTRUCTOR_ID);
    assert_eq!(sent_force.len(), 1);
    let req1 = tl_fns::messages::GetEmojiGroups::from_bytes(&sent_force[0]).expect("decode req1");
    assert_eq!(req1.hash, 98_765);
}

// =============================================================================
// 3. Persistent trusted Mini App bots across tdata reload and logout wipe
// =============================================================================

#[test]
fn trusted_mini_app_bots_persist_across_tdata_reload_and_clear_on_logout() {
    let (_fake, mut live, rt) = start_live_harness();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let creds = ApiCredentials {
        api_id: 12_345,
        api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    let bot_a = UiPeerId::from_user(900_001);
    let bot_b = UiPeerId::from_user(900_002);

    // 1. Trust bot_a through AppSession on LiveSession and collect StoredSessionSettings.
    assert!(!live.is_mini_app_bot_trusted(bot_a));
    assert!(!live.is_mini_app_bot_trusted(bot_b));
    live.trust_mini_app_bot(bot_a);
    assert!(live.is_mini_app_bot_trusted(bot_a));
    assert!(!live.is_mini_app_bot_trusted(bot_b));
    assert!(
        live.take_session_settings_dirty(),
        "trusting a new Mini App bot must mark session_settings_dirty"
    );

    let collected = live.collect_stored_session_settings(&StoredSessionSettings::default());
    assert!(collected.trusted_mini_app_bots.contains(&bot_a.0));

    // 2. Persist into encrypted tdata through AccountManager, drop, and reopen.
    let opts = WorkingDirOpenOptions::new(dir.path(), creds.clone()).with_pfs(false);
    {
        let mut mgr = AccountManager::open_and_unlock(opts.clone(), b"").expect("open tdata");
        let idx = mgr.active_index().expect("active slot");
        mgr.save_session_settings(idx, &collected)
            .expect("save session settings");
        mgr.trust_mini_app_bot(idx, bot_b.0)
            .expect("trust bot_b directly");
        assert!(
            mgr.is_mini_app_bot_trusted(idx, bot_a.0)
                .expect("check bot_a")
        );
        assert!(
            mgr.is_mini_app_bot_trusted(idx, bot_b.0)
                .expect("check bot_b")
        );
    }

    // Reopen tdata in a new AccountManager and restore into a brand new LiveSession.
    let (_fake2, mut live2, _rt2) = start_live_harness();
    {
        let mut mgr = AccountManager::open_and_unlock(opts.clone(), b"").expect("reopen tdata");
        let idx = mgr.active_index().expect("active slot");
        let reloaded = mgr
            .load_session_settings(idx)
            .expect("reload session settings");
        live2.apply_stored_session_settings(&reloaded, (true, true, true));
        assert!(live2.is_mini_app_bot_trusted(bot_a));
        assert!(live2.is_mini_app_bot_trusted(bot_b));

        // 3. Logout wipes per-account encrypted storage, clearing trusted Mini App bots.
        rt.block_on(async {
            mgr.logout_account(idx)
                .await
                .expect("logout clears account storage");
        });
        assert!(
            !mgr.is_mini_app_bot_trusted(idx, bot_a.0)
                .expect("check bot_a after logout")
        );
        assert!(
            !mgr.is_mini_app_bot_trusted(idx, bot_b.0)
                .expect("check bot_b after logout")
        );
    }

    // Reopening after logout still shows empty trusted bots.
    let mgr_after = AccountManager::open_and_unlock(opts, b"").expect("reopen after logout");
    let idx = mgr_after.active_index().expect("active slot");
    assert!(
        mgr_after
            .trusted_mini_app_bots(idx)
            .expect("trusted set after logout")
            .is_empty()
    );
}

// =============================================================================
// 4. Work-folder Custom icon conversion (`Ui::ComputeDefaultFilterIcon`)
// =============================================================================

#[test]
fn work_folder_with_explicit_always_peers_maps_to_custom_icon() {
    let session = Session::new(DataUserId(1001));
    let work_peer = DataPeerId::from_user(DataUserId(2001));

    // A "Work" folder with explicit `always` peers and empty `icon_emoji` must use `Custom`,
    // even when category flags like CONTACTS or GROUPS are also set (tdesktop filter_icons.cpp:314).
    let work_filter = ChatFilter::new(
        2,
        DataText::plain("Work"),
        "",
        None,
        ChatFilterFlags::CONTACTS | ChatFilterFlags::GROUPS,
        BTreeSet::from([work_peer]),
        Vec::new(),
        BTreeSet::new(),
    );

    let dialog_folder = chat_filter_to_dialog_folder(&work_filter, &session);
    assert_eq!(dialog_folder.icon, FolderIconKind::Custom);
    let settings_folder = chat_filter_to_settings_folder(&work_filter);
    assert_eq!(settings_folder.icon, FolderIcon::Custom);

    // Pure category folders with no explicit always/never peers keep their category icons.
    let groups_only = ChatFilter::new(
        3,
        DataText::plain("Groups"),
        "",
        None,
        ChatFilterFlags::GROUPS,
        BTreeSet::new(),
        Vec::new(),
        BTreeSet::new(),
    );
    assert_eq!(
        chat_filter_to_dialog_folder(&groups_only, &session).icon,
        FolderIconKind::Groups
    );

    let personal_only = ChatFilter::new(
        4,
        DataText::plain("Personal"),
        "",
        None,
        ChatFilterFlags::CONTACTS | ChatFilterFlags::NON_CONTACTS,
        BTreeSet::new(),
        Vec::new(),
        BTreeSet::new(),
    );
    assert_eq!(
        chat_filter_to_dialog_folder(&personal_only, &session).icon,
        FolderIconKind::Private
    );

    let unread_all = ChatFilter::new(
        5,
        DataText::plain("Unread"),
        "",
        None,
        ChatFilterFlags::CONTACTS
            | ChatFilterFlags::NON_CONTACTS
            | ChatFilterFlags::GROUPS
            | ChatFilterFlags::CHANNELS
            | ChatFilterFlags::BOTS
            | ChatFilterFlags::NO_READ,
        BTreeSet::new(),
        Vec::new(),
        BTreeSet::new(),
    );
    assert_eq!(
        chat_filter_to_dialog_folder(&unread_all, &session).icon,
        FolderIconKind::Unread
    );

    // Explicit emoticon overrides default computation.
    let work_emoji = ChatFilter::new(
        6,
        DataText::plain("Work"),
        "💼",
        None,
        ChatFilterFlags::empty(),
        BTreeSet::from([work_peer]),
        Vec::new(),
        BTreeSet::new(),
    );
    assert_eq!(
        chat_filter_to_dialog_folder(&work_emoji, &session).icon,
        FolderIconKind::Work
    );
}

// =============================================================================
// 5. DialogItem `is_contact` & `media_preview` population
// =============================================================================

#[test]
fn dialog_item_populates_is_contact_and_media_preview_metadata() {
    let mut session = Session::new(DataUserId(1001));

    // Self peer (Saved Messages) must NEVER have is_contact == true.
    let mut me = User::new(DataUserId(1001), true);
    me.flags.insert(UserFlags::CONTACT);
    me.set_name("Self", "User", "", "");
    session.upsert_user(me, PeerUpdateFlags::all());

    // Contact user.
    let mut contact = User::new(DataUserId(2001), false);
    contact.flags.insert(UserFlags::CONTACT);
    contact.set_name("Mira", "Vance", "", "");
    session.upsert_user(contact, PeerUpdateFlags::all());

    // Non-contact user.
    let mut stranger = User::new(DataUserId(2002), false);
    stranger.set_name("Theo", "Reed", "", "");
    session.upsert_user(stranger, PeerUpdateFlags::all());

    let self_pid = DataPeerId::from_user(DataUserId(1001));
    let contact_pid = DataPeerId::from_user(DataUserId(2001));
    let stranger_pid = DataPeerId::from_user(DataUserId(2002));

    // 1. Service message in Saved Messages -> is_contact=false, Service preview with blue tint.
    let mut svc = Message::new_text(
        self_pid,
        DataMsgId(1),
        Some(self_pid),
        1_764_000_000,
        DataText::plain(""),
    );
    svc.content = MessageContent::Service(Box::new(ServiceAction::BotAllowed {
        attach_menu: true,
        from_request: false,
        domain: None,
        app_id: None,
    }));
    session.insert_message(svc);

    let saved_item = dialog_item_from_session(
        &session,
        self_pid,
        DialogListQuery::Filter(FilterId::ALL),
        1_764_000_060,
    )
    .expect("saved dialog");
    assert!(!saved_item.is_contact);
    let saved_preview = saved_item.media_preview.expect("service media_preview");
    assert_eq!(saved_preview.kind, DialogMediaPreviewKind::Service);
    assert!(saved_preview.use_service_color);
    assert!(!saved_preview.has_caption);

    // 2. Photo without caption in contact chat -> is_contact=true, Photo preview with stripped_thumb & service tint.
    let thumb_bytes = vec![1u8, 40, 40, 0xFF, 0xD9];
    let mut photo_msg = Message::new_text(
        contact_pid,
        DataMsgId(2),
        Some(contact_pid),
        1_764_000_010,
        DataText::plain(""),
    );
    photo_msg.content = MessageContent::Regular {
        text: DataText::plain(""),
        media: MessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(501),
            stripped_thumb: Some(thumb_bytes.clone()),
            ..Default::default()
        })),
    };
    session.insert_message(photo_msg);

    let contact_item = dialog_item_from_session(
        &session,
        contact_pid,
        DialogListQuery::Filter(FilterId::ALL),
        1_764_000_060,
    )
    .expect("contact dialog");
    assert!(contact_item.is_contact);
    let photo_preview = contact_item.media_preview.expect("photo media_preview");
    assert_eq!(photo_preview.kind, DialogMediaPreviewKind::Photo);
    assert_eq!(
        photo_preview.stripped_thumb.as_deref(),
        Some(thumb_bytes.as_slice())
    );
    assert!(photo_preview.use_service_color);
    assert!(!photo_preview.has_caption);

    // 3. Photo WITH caption -> has_caption=true, use_service_color=false (caption uses normal text color).
    let mut photo_caption = Message::new_text(
        contact_pid,
        DataMsgId(3),
        Some(contact_pid),
        1_764_000_020,
        DataText::plain("Sunset view"),
    );
    photo_caption.content = MessageContent::Regular {
        text: DataText::plain("Sunset view"),
        media: MessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(502),
            stripped_thumb: Some(thumb_bytes),
            ..Default::default()
        })),
    };
    session.insert_message(photo_caption);
    let captioned_item = dialog_item_from_session(
        &session,
        contact_pid,
        DialogListQuery::Filter(FilterId::ALL),
        1_764_000_060,
    )
    .expect("captioned contact dialog");
    let cap_preview = captioned_item
        .media_preview
        .expect("captioned photo preview");
    assert_eq!(cap_preview.kind, DialogMediaPreviewKind::Photo);
    assert!(cap_preview.has_caption);
    assert!(!cap_preview.use_service_color);

    // 4. Plain text in stranger chat -> is_contact=false, media_preview=None.
    session.insert_message(Message::new_text(
        stranger_pid,
        DataMsgId(4),
        Some(stranger_pid),
        1_764_000_030,
        DataText::plain("Hello there"),
    ));
    let stranger_item = dialog_item_from_session(
        &session,
        stranger_pid,
        DialogListQuery::Filter(FilterId::ALL),
        1_764_000_060,
    )
    .expect("stranger dialog");
    assert!(!stranger_item.is_contact);
    assert!(stranger_item.media_preview.is_none());

    // 5. File document attachment -> File preview kind.
    let mut file_msg = Message::new_text(
        stranger_pid,
        DataMsgId(5),
        Some(stranger_pid),
        1_764_000_040,
        DataText::plain(""),
    );
    file_msg.content = MessageContent::Regular {
        text: DataText::plain(""),
        media: MessageMedia::Document(Box::new(DocumentData {
            kind: DocumentKind::File,
            file_name: "notes.pdf".to_owned(),
            ..Default::default()
        })),
    };
    session.insert_message(file_msg);
    let file_item = dialog_item_from_session(
        &session,
        stranger_pid,
        DialogListQuery::Filter(FilterId::ALL),
        1_764_000_060,
    )
    .expect("file dialog");
    let file_preview = file_item.media_preview.expect("file media_preview");
    assert_eq!(file_preview.kind, DialogMediaPreviewKind::File);
    assert!(file_preview.use_service_color);
}

// =============================================================================
// 6. MediaItem `grouped_id` and tdesktop media-viewer date formatting
// =============================================================================

#[test]
fn media_item_preserves_grouped_id_and_formats_local_mediaview_dates() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    let mut session = Session::new(DataUserId(1001));
    let mut me = User::new(DataUserId(1001), true);
    me.set_name("Alice", "Example", "", "");
    session.upsert_user(me, PeerUpdateFlags::all());
    let peer = DataPeerId::from_user(DataUserId(1001));

    // Reference now: Wed 2026-11-25 03:30:00 local (01:30:00 UTC)
    let now_ts = LocalDateTime::new(2026, 11, 25, 1, 30, 0).to_local_epoch_secs() as i32;
    // Today 03:07 local (01:07 UTC)
    let today_ts = LocalDateTime::new(2026, 11, 25, 1, 7, 0).to_local_epoch_secs() as i32;
    // Yesterday 16:29 local (14:29 UTC)
    let yesterday_ts = LocalDateTime::new(2026, 11, 24, 14, 29, 0).to_local_epoch_secs() as i32;
    // Older: 2026-11-15 03:07 local (01:07 UTC)
    let older_ts = LocalDateTime::new(2026, 11, 15, 1, 7, 0).to_local_epoch_secs() as i32;

    // 1. Album photo (grouped_id = Some(9_999_001)) sent today.
    let mut album_photo = Message::new_text(
        peer,
        DataMsgId(10),
        Some(peer),
        today_ts,
        DataText::plain(""),
    );
    album_photo.grouped_id = Some(9_999_001);
    album_photo.content = MessageContent::Regular {
        text: DataText::plain(""),
        media: MessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(100),
            ..Default::default()
        })),
    };
    let album_item =
        message_to_viewer_media_item_at(&session, &album_photo, now_ts).expect("album media item");
    assert_eq!(album_item.grouped_id, Some(9_999_001));
    assert_eq!(album_item.date_text, "today at 3:07");

    // 2. Non-album photo (grouped_id = None) sent yesterday.
    let mut single_photo = Message::new_text(
        peer,
        DataMsgId(11),
        Some(peer),
        yesterday_ts,
        DataText::plain(""),
    );
    single_photo.content = MessageContent::Regular {
        text: DataText::plain(""),
        media: MessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(101),
            ..Default::default()
        })),
    };
    let single_item = message_to_viewer_media_item_at(&session, &single_photo, now_ts)
        .expect("single media item");
    assert_eq!(single_item.grouped_id, None);
    assert_eq!(single_item.date_text, "yesterday at 16:29");

    // 3. Older photo (10 days ago).
    let mut older_photo = Message::new_text(
        peer,
        DataMsgId(12),
        Some(peer),
        older_ts,
        DataText::plain(""),
    );
    older_photo.grouped_id = Some(0);
    older_photo.content = MessageContent::Regular {
        text: DataText::plain(""),
        media: MessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(102),
            ..Default::default()
        })),
    };
    let older_item =
        message_to_viewer_media_item_at(&session, &older_photo, now_ts).expect("older media item");
    assert_eq!(older_item.grouped_id, None);
    assert_eq!(older_item.date_text, "15.11.26 at 3:07");
}

// =============================================================================
// 7. Stage-3 local-time & tdesktop date formatting rules (one test per rule)
// =============================================================================

#[test]
fn local_short_time_converts_utc_to_local_and_uses_unpadded_h_mm() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    // 2026-11-25 01:07:00 UTC -> 2026-11-25 03:07:00 local -> "3:07" (NOT "01:07", NOT "03:07").
    let ts = LocalDateTime::new(2026, 11, 25, 1, 7, 0).to_local_epoch_secs() as i32;
    let dt = unix_to_local_datetime(ts);
    assert_eq!((dt.date.year, dt.date.month, dt.date.day), (2026, 11, 25));
    assert_eq!((dt.hour, dt.minute), (3, 7));
    assert_eq!(format_short_time(ts), "3:07");
}

#[test]
fn dialogs_date_today_shows_local_h_mm() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    let now = LocalDateTime::new(2026, 11, 25, 1, 30, 0).to_local_epoch_secs() as i32; // 03:30 local
    let msg = LocalDateTime::new(2026, 11, 25, 1, 7, 0).to_local_epoch_secs() as i32; // 03:07 local
    assert_eq!(format_dialogs_date_at(msg, now), "3:07");
}

#[test]
fn dialogs_date_yesterday_within_20h_shows_local_time_not_weekday() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    // Now: Wed 2026-11-25 03:30 local (01:30 UTC).
    // Msg: Tue 2026-11-24 16:29 local (14:29 UTC) — ~11h ago (< 20h), must show "16:29" not "Tue"!
    let now = LocalDateTime::new(2026, 11, 25, 1, 30, 0).to_local_epoch_secs() as i32;
    let msg = LocalDateTime::new(2026, 11, 24, 14, 29, 0).to_local_epoch_secs() as i32;
    assert_eq!(format_dialogs_date_at(msg, now), "16:29");

    // Also verify dialog_entry_from_session uses the exact same local-time Ui::FormatDialogsDate rule.
    let mut session = Session::new(DataUserId(1001));
    let peer = DataPeerId::from_user(DataUserId(2001));
    let mut u = User::new(DataUserId(2001), false);
    u.set_name("Sam", "Test", "", "");
    session.upsert_user(u, PeerUpdateFlags::all());
    session.insert_message(Message::new_text(
        peer,
        DataMsgId(1),
        Some(peer),
        msg,
        DataText::plain("ping"),
    ));
    let entry = dialog_entry_from_session(&session, peer, now).expect("dialog entry");
    assert_eq!(entry.timestamp, "16:29");
}

#[test]
fn dialogs_date_yesterday_past_20h_and_within_week_shows_weekday() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    // Now: Wed 2026-11-25 03:30 local (01:30 UTC).
    let now = LocalDateTime::new(2026, 11, 25, 1, 30, 0).to_local_epoch_secs() as i32;
    // Msg: Tue 2026-11-24 06:00 local (04:00 UTC) — 21.5h ago (>= 20h, 1 day ago < 7) -> "Tue".
    let tue_morning = LocalDateTime::new(2026, 11, 24, 4, 0, 0).to_local_epoch_secs() as i32;
    assert_eq!(format_dialogs_date_at(tue_morning, now), "Tue");

    // Msg: Thu 2026-11-19 12:00 local (10:00 UTC) — 6 days ago -> "Thu".
    let six_days_ago = LocalDateTime::new(2026, 11, 19, 10, 0, 0).to_local_epoch_secs() as i32;
    assert_eq!(format_dialogs_date_at(six_days_ago, now), "Thu");
}

#[test]
fn dialogs_date_older_than_week_shows_short_calendar_date() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    // Now: Wed 2026-11-25 03:30 local (01:30 UTC).
    // Msg: Wed 2026-11-18 12:00 local (10:00 UTC) — 7 days ago -> "18.11.26".
    let now = LocalDateTime::new(2026, 11, 25, 1, 30, 0).to_local_epoch_secs() as i32;
    let seven_days_ago = LocalDateTime::new(2026, 11, 18, 10, 0, 0).to_local_epoch_secs() as i32;
    assert_eq!(format_dialogs_date_at(seven_days_ago, now), "18.11.26");
}

#[test]
fn read_date_and_media_date_use_local_time_and_tdesktop_rules() {
    let _tz = UtcOffsetGuard::set(2 * 3600); // UTC+2
    // Now: Wed 2026-11-25 03:30 local (01:30 UTC).
    let now = LocalDateTime::new(2026, 11, 25, 1, 30, 0).to_local_epoch_secs() as i32;
    let today = LocalDateTime::new(2026, 11, 25, 1, 7, 0).to_local_epoch_secs() as i32; // 03:07 local
    let yesterday = LocalDateTime::new(2026, 11, 24, 14, 29, 0).to_local_epoch_secs() as i32; // 16:29 local
    let earlier = LocalDateTime::new(2026, 11, 20, 1, 7, 0).to_local_epoch_secs() as i32; // Nov 20 03:07 local

    assert_eq!(format_date_time_at(today, now), "today at 3:07");
    assert_eq!(format_date_time_at(yesterday, now), "yesterday at 16:29");
    assert_eq!(format_date_time_at(earlier, now), "20.11.26 at 3:07");

    assert_eq!(format_read_date_at(today, now), "today at 3:07");
    assert_eq!(format_read_date_at(yesterday, now), "yesterday at 16:29");
    assert_eq!(format_read_date_at(earlier, now), "Nov 20 at 3:07");
}
