//! Unit and integration tests for live `ChatHelpersBackend`, `SettingsBackend`
//! sticker sets, custom emoji sets/documents, GIF search (`@gif`), and
//! `HistoryBackend::chat_mention_candidates` over an in-process `FakeRpcClient`.
//!
//! Uses only synthetic fictional names, synthetic IDs, and in-process fakes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_chat::stickers::{
    FAVED_SET_ID, RECENT_SET_ID, STICKERS_UPDATE_TIMEOUT_MS, StickerType, StickersSet,
    StickersSetFlags, StickersStore, StickersType,
};
use tg_chat::{DocumentId as ChatDocId, StickerSetId, UserId as ChatUserId};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, Message, MsgId as DataMsgId,
    PeerId as DataPeerId, TextWithEntities as DataText, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::chat_helpers::{
    PARTICIPANTS_CACHE_TTL_MS, count_custom_emoji_hash, count_document_vector_hash,
    count_faved_stickers_hash, count_featured_stickers_hash, count_recent_stickers_hash,
    count_saved_gifs_hash, count_stickers_hash, hash_init, hash_update_u64,
};
use crate::session::LiveSession;
use tg_app_core::chat_helpers::backend::{ChatHelpersBackend, StickerMedia};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::PeerId;

// =============================================================================
// FakeRpcClient & Synthetic Builders
// =============================================================================

type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;
type RecordedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;
type QueuedResponses = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedRequests,
    responses: QueuedResponses,
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

    fn recorded_requests_of(&self, ctor: u32) -> Vec<Vec<u8>> {
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

fn synthetic_tl_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    is_bot: bool,
    deleted: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted,
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
        access_hash: Some(id * 1009 + 17),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: is_bot.then_some(1),
        restriction_reason: None,
        bot_inline_placeholder: is_bot.then(|| "Search...".to_owned()),
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

#[allow(clippy::too_many_arguments)]
fn synthetic_sticker_set(
    id: i64,
    access_hash: i64,
    hash: i32,
    title: &str,
    short_name: &str,
    count: i32,
    installed_date: Option<i32>,
    archived: bool,
    emojis: bool,
) -> tl_enums::StickerSet {
    tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
        archived,
        official: true,
        masks: false,
        emojis,
        text_color: false,
        channel_emoji_status: false,
        creator: false,
        installed_date,
        id,
        access_hash,
        title: title.to_owned(),
        short_name: short_name.to_owned(),
        thumbs: None,
        thumb_dc_id: None,
        thumb_version: None,
        thumb_document_id: None,
        count,
        hash,
    })
}

fn synthetic_sticker_doc(
    id: i64,
    set_id: i64,
    set_access_hash: i64,
    alt: &str,
    mime_type: &str,
    premium: bool,
) -> tl_enums::Document {
    let video_thumbs = if premium {
        Some(vec![tl_enums::VideoSize::VideoSize(tl_types::VideoSize {
            r#type: "f".to_owned(),
            w: 512,
            h: 512,
            size: 1024,
            video_start_ts: None,
        })])
    } else {
        None
    };
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 17 + 3,
        file_reference: vec![1, 2, 3, (id & 0xFF) as u8],
        date: 1_750_000_000,
        mime_type: mime_type.to_owned(),
        size: 4096,
        thumbs: None,
        video_thumbs,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 512,
                h: 512,
            }),
            tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                mask: false,
                alt: alt.to_owned(),
                stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                    id: set_id,
                    access_hash: set_access_hash,
                }),
                mask_coords: None,
            }),
        ],
    })
}

fn synthetic_custom_emoji_doc(
    id: i64,
    set_id: i64,
    set_access_hash: i64,
    alt: &str,
    free: bool,
) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 13 + 5,
        file_reference: vec![9, 8, 7],
        date: 1_750_000_000,
        mime_type: "application/x-tgsticker".to_owned(),
        size: 2048,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 100,
                h: 100,
            }),
            tl_enums::DocumentAttribute::CustomEmoji(tl_types::DocumentAttributeCustomEmoji {
                free,
                text_color: false,
                alt: alt.to_owned(),
                stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                    id: set_id,
                    access_hash: set_access_hash,
                }),
            }),
        ],
    })
}

fn synthetic_gif_doc(
    id: i64,
    width: i32,
    height: i32,
    file_name: &str,
    cached_thumb: Option<Vec<u8>>,
) -> tl_enums::Document {
    let thumbs = cached_thumb.map(|bytes| {
        vec![tl_enums::PhotoSize::PhotoCachedSize(
            tl_types::PhotoCachedSize {
                r#type: "s".to_owned(),
                w: 4,
                h: 4,
                bytes,
            },
        )]
    });
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 31 + 11,
        file_reference: vec![4, 5, 6],
        date: 1_750_000_000,
        mime_type: "video/mp4".to_owned(),
        size: 32_768,
        thumbs,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl_enums::DocumentAttribute::Animated,
            tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
                round_message: false,
                supports_streaming: true,
                nosound: true,
                duration: 2.5,
                w: width,
                h: height,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            }),
            tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
                file_name: file_name.to_owned(),
            }),
        ],
    })
}

fn make_tiny_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = Vec::new();
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    let dyn_img = image::DynamicImage::ImageRgba8(img);
    dyn_img
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("encode tiny png");
    buf
}

fn start_live_harness() -> (FakeRpcClient, LiveSession) {
    let self_uid = 700_001i64;
    let self_user = synthetic_tl_user(self_uid, "Kestrel", "kestrel_self", true, false, false);
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
            date: 1_750_000_000,
            seq: 1,
            unread_count: 0,
        }),
    );

    let rt = SessionRuntime::start(
        DataUserId(self_uid as u64),
        crate::ErasedRpcClient::new(fake.clone()),
        SessionRuntimeConfig {
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");

    let live = LiveSession::new(Arc::new(rt), 1);
    live.set_chat_helpers_now_ms(Some(1_750_000_000_000));
    fake.clear_recorded();
    (fake, live)
}

// =============================================================================
// Tests
// =============================================================================

#[test]
fn test_telegram_hash_known_vectors() {
    // Verify Api::HashUpdate matches Telegram Desktop's xorshift-add formula.
    let mut h = hash_init();
    hash_update_u64(&mut h, 100);
    assert_eq!(h, 100);
    hash_update_u64(&mut h, 200);
    let mut expected = 100u64;
    expected ^= expected >> 21;
    expected ^= expected << 35;
    expected ^= expected >> 4;
    expected = expected.wrapping_add(200);
    assert_eq!(h, expected);

    let mut store = StickersStore::new();
    let s1 = StickersSet::new(
        StickerSetId(10),
        111,
        42,
        "Pack One",
        "pack_one",
        1,
        StickersSetFlags::INSTALLED,
        100,
    );
    let s2 = StickersSet::new(
        StickerSetId(20),
        222,
        99,
        "Pack Two",
        "pack_two",
        1,
        StickersSetFlags::INSTALLED | StickersSetFlags::EMOJI,
        100,
    );
    let f1 = StickersSet::new(
        StickerSetId(30),
        333,
        7,
        "Featured One",
        "feat_one",
        1,
        StickersSetFlags::FEATURED | StickersSetFlags::UNREAD,
        0,
    );
    store.sets.insert(StickerSetId(10), s1);
    store.sets.insert(StickerSetId(20), s2);
    store.sets.insert(StickerSetId(30), f1);
    store.sets_order = vec![StickerSetId(10)];
    store.emoji_sets_order = vec![StickerSetId(20)];
    store.featured_sets_order = vec![StickerSetId(30)];
    store.saved_gifs = vec![ChatDocId(500), ChatDocId(501)];

    assert_eq!(count_stickers_hash(&store, true), 42);
    assert_eq!(count_custom_emoji_hash(&store, true), 99);
    assert_eq!(
        count_saved_gifs_hash(&store),
        count_document_vector_hash(&[ChatDocId(500), ChatDocId(501)])
    );

    // Featured hash hashes set_id followed by `1` when UNREAD is set.
    let mut feat_expected = hash_init();
    hash_update_u64(&mut feat_expected, 30);
    hash_update_u64(&mut feat_expected, 1);
    assert_eq!(count_featured_stickers_hash(&store), feat_expected);
    assert_eq!(count_faved_stickers_hash(&store), 0);
    assert_eq!(count_recent_stickers_hash(&store, false), 0);
}

#[test]
fn test_get_all_stickers_and_get_sticker_set_with_hash_refresh() {
    let (fake, live) = start_live_harness();

    let set_id = 10_001i64;
    let set_hash = 123_456i32;
    let doc1 = synthetic_sticker_doc(1001, set_id, 555, "🔥", "application/x-tgsticker", false);
    let doc2 = synthetic_sticker_doc(1002, set_id, 555, "⭐", "image/webp", true);
    let set_header = synthetic_sticker_set(
        set_id,
        555,
        set_hash,
        "Aurora Pack",
        "aurora_pack",
        2,
        Some(1_749_999_000),
        false,
        false,
    );

    fake.enqueue_ok(
        tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FavedStickers::FavedStickers(tl_types::messages::FavedStickers {
            hash: 77,
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "🔥".to_owned(),
                documents: vec![1001],
            })],
            stickers: vec![doc1.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::RecentStickers::RecentStickers(tl_types::messages::RecentStickers {
            hash: 88,
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "⭐".to_owned(),
                documents: vec![1002],
            })],
            stickers: vec![doc2.clone()],
            dates: vec![1_749_999_500],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 999,
            sets: vec![set_header.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: set_header,
            packs: vec![
                tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                    emoticon: "🔥".to_owned(),
                    documents: vec![1001],
                }),
                tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                    emoticon: "⭐".to_owned(),
                    documents: vec![1002],
                }),
            ],
            keywords: vec![tl_enums::StickerKeyword::StickerKeyword(
                tl_types::StickerKeyword {
                    document_id: 1001,
                    keyword: vec!["blaze".to_owned()],
                },
            )],
            documents: vec![doc1, doc2],
        }),
    );

    let sections = live.sticker_sections();
    assert_eq!(sections.len(), 3);
    assert_eq!(sections[0].id, FAVED_SET_ID);
    assert_eq!(sections[0].stickers, vec![ChatDocId(1001)]);
    assert_eq!(sections[1].id, RECENT_SET_ID);
    assert_eq!(sections[1].stickers, vec![ChatDocId(1002)]);
    assert_eq!(sections[2].id, StickerSetId(10_001));
    assert_eq!(sections[2].title, "Aurora Pack");
    assert_eq!(sections[2].stickers, vec![ChatDocId(1001), ChatDocId(1002)]);

    // Borrowed `&StickerDocument` lookup works immediately without `&mut self`.
    let d1 = live
        .sticker_document(ChatDocId(1001))
        .expect("doc 1001 cached");
    assert_eq!(d1.sticker_type, StickerType::Tgs);
    assert_eq!(d1.alt_emoji, "🔥");
    assert!(!d1.is_premium_sticker);
    let d2 = live
        .sticker_document(ChatDocId(1002))
        .expect("doc 1002 cached");
    assert_eq!(d2.sticker_type, StickerType::Webp);
    assert!(d2.is_premium_sticker);

    let icons = live.sticker_tab_icons(&sections, 24);
    assert_eq!(icons.len(), 2); // Favorites + Aurora Pack (Recent shares slot with Favorites)
    assert_eq!(icons[0].set_id, FAVED_SET_ID);
    assert_eq!(icons[1].set_id, StickerSetId(10_001));

    // Second call within 1-hour TTL makes ZERO RPCs.
    fake.clear_recorded();
    let sections_cached = live.sticker_sections();
    assert_eq!(sections_cached.len(), 3);
    assert!(fake.recorded_ctors().is_empty());

    // Advance clock past 1 hour -> sends non-zero hashes and handles NotModified cleanly.
    live.set_chat_helpers_now_ms(Some(1_750_000_000_000 + STICKERS_UPDATE_TIMEOUT_MS + 1000));
    fake.enqueue_ok(
        tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FavedStickers::NotModified,
    );
    fake.enqueue_ok(
        tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::RecentStickers::NotModified,
    );
    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::NotModified,
    );

    let sections_after = live.sticker_sections();
    assert_eq!(sections_after.len(), 3);

    // Verify GetAllStickers sent the exact computed hash (123_456).
    let raw_all = fake.recorded_requests_of(tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID);
    assert_eq!(raw_all.len(), 1);
    let parsed_all = tl_fns::messages::GetAllStickers::from_bytes(&raw_all[0]).expect("deser");
    assert_eq!(parsed_all.hash, i64::from(set_hash));

    // Verify GetFavedStickers sent non-zero document hash (1001).
    let raw_faved = fake.recorded_requests_of(tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID);
    assert_eq!(raw_faved.len(), 1);
    let parsed_faved =
        tl_fns::messages::GetFavedStickers::from_bytes(&raw_faved[0]).expect("deser");
    assert_eq!(parsed_faved.hash, 1001);
}

#[test]
fn test_faved_and_recent_stickers_mutations() {
    let (fake, mut live) = start_live_harness();
    let doc = synthetic_sticker_doc(2001, 20_001, 777, "🎉", "image/webp", false);
    let set_hdr = synthetic_sticker_set(
        20_001,
        777,
        44,
        "Party",
        "party_pack",
        1,
        Some(1_750_000_000),
        false,
        false,
    );

    fake.enqueue_ok(
        tl_fns::messages::GetFavedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FavedStickers::FavedStickers(tl_types::messages::FavedStickers {
            hash: 0,
            packs: Vec::new(),
            stickers: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::RecentStickers::RecentStickers(tl_types::messages::RecentStickers {
            hash: 0,
            packs: Vec::new(),
            stickers: Vec::new(),
            dates: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 44,
            sets: vec![set_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: set_hdr,
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "🎉".to_owned(),
                documents: vec![2001],
            })],
            keywords: Vec::new(),
            documents: vec![doc],
        }),
    );
    let _ = live.sticker_sections();
    assert!(!live.is_sticker_faved(ChatDocId(2001)));

    // Fave sticker
    fake.enqueue_ok(tl_fns::messages::FaveSticker::CONSTRUCTOR_ID, &true);
    assert!(live.toggle_faved_sticker(ChatDocId(2001)));
    assert!(live.is_sticker_faved(ChatDocId(2001)));

    // Unfave sticker
    fake.enqueue_ok(tl_fns::messages::FaveSticker::CONSTRUCTOR_ID, &true);
    assert!(!live.toggle_faved_sticker(ChatDocId(2001)));
    assert!(!live.is_sticker_faved(ChatDocId(2001)));

    // Record sticker used -> sends SaveRecentSticker & adds to Recent section
    fake.enqueue_ok(tl_fns::messages::SaveRecentSticker::CONSTRUCTOR_ID, &true);
    live.record_sticker_used(ChatDocId(2001));
    assert_eq!(
        fake.recorded_requests_of(tl_fns::messages::SaveRecentSticker::CONSTRUCTOR_ID)
            .len(),
        1
    );
    let sections = live.sticker_sections();
    assert!(
        sections
            .iter()
            .any(|s| s.id == RECENT_SET_ID && s.stickers.contains(&ChatDocId(2001)))
    );
}

#[test]
fn test_featured_stickers_and_read_featured_stickers() {
    let (fake, live) = start_live_harness();
    let feat_id = 30_001i64;
    let cover_doc =
        synthetic_sticker_doc(3001, feat_id, 888, "🚀", "application/x-tgsticker", false);
    let feat_hdr = synthetic_sticker_set(
        feat_id,
        888,
        55,
        "Orbital Pack",
        "orbital_pack",
        1,
        None,
        false,
        false,
    );

    fake.enqueue_ok(
        tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: false,
                hash: 55,
                count: 1,
                sets: vec![tl_enums::StickerSetCovered::StickerSetMultiCovered(
                    tl_types::StickerSetMultiCovered {
                        set: feat_hdr,
                        covers: vec![cover_doc],
                    },
                )],
                unread: vec![feat_id],
            },
        ),
    );

    let featured = live.featured_sticker_sections();
    assert_eq!(featured.len(), 1);
    assert_eq!(featured[0].id, StickerSetId(30_001));
    assert_eq!(featured[0].stickers, vec![ChatDocId(3001)]);
    assert_eq!(live.featured_stickers_unread_count(), 1);

    // Mark featured set read -> calls messages.readFeaturedStickers
    fake.enqueue_ok(
        tl_fns::messages::ReadFeaturedStickers::CONSTRUCTOR_ID,
        &true,
    );
    let flushed = live
        .read_featured_stickers(&[StickerSetId(30_001)])
        .expect("read featured");
    assert_eq!(flushed, vec![StickerSetId(30_001)]);
    assert_eq!(live.featured_stickers_unread_count(), 0);

    let raw_read =
        fake.recorded_requests_of(tl_fns::messages::ReadFeaturedStickers::CONSTRUCTOR_ID);
    assert_eq!(raw_read.len(), 1);
    let req = tl_fns::messages::ReadFeaturedStickers::from_bytes(&raw_read[0]).expect("deser");
    assert_eq!(req.id, vec![feat_id]);
}

#[test]
fn test_install_uninstall_and_reorder_sticker_sets() {
    let (fake, mut live) = start_live_harness();
    let s1_id = 40_001i64;
    let s2_id = 40_002i64;
    let d1 = synthetic_sticker_doc(4001, s1_id, 101, "😄", "image/webp", false);
    let d2 = synthetic_sticker_doc(4002, s2_id, 202, "🔥", "image/webp", false);
    let s1_hdr = synthetic_sticker_set(
        s1_id,
        101,
        11,
        "First Pack",
        "first_pack",
        1,
        Some(1_749_000_000),
        false,
        false,
    );
    let s2_hdr = synthetic_sticker_set(
        s2_id,
        202,
        22,
        "Trending Pack",
        "trending_pack",
        1,
        None,
        false,
        false,
    );

    // Load initial installed set s1
    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 11,
            sets: vec![s1_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: s1_hdr.clone(),
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "😄".to_owned(),
                documents: vec![4001],
            })],
            keywords: Vec::new(),
            documents: vec![d1],
        }),
    );
    live.refresh_all_stickers(true).expect("refresh all");
    assert!(live.is_set_installed(StickerSetId(40_001)));

    // Load featured set s2
    fake.enqueue_ok(
        tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: false,
                hash: 22,
                count: 1,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: s2_hdr.clone(),
                        cover: d2.clone(),
                    },
                )],
                unread: Vec::new(),
            },
        ),
    );
    let _ = live.featured_sticker_sections();
    assert!(!live.is_set_installed(StickerSetId(40_002)));

    // Install s2 -> InstallStickerSet + GetStickerSet (since only cover was loaded)
    fake.enqueue_ok(
        tl_fns::messages::InstallStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSetInstallResult::Success,
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: synthetic_sticker_set(
                s2_id,
                202,
                22,
                "Trending Pack",
                "trending_pack",
                1,
                Some(1_750_000_000),
                false,
                false,
            ),
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "🔥".to_owned(),
                documents: vec![4002],
            })],
            keywords: Vec::new(),
            documents: vec![d2],
        }),
    );
    assert!(live.install_sticker_set(StickerSetId(40_002)));
    assert!(live.is_set_installed(StickerSetId(40_002)));

    // Reorder [s1, s2] -> sends messages.reorderStickerSets
    fake.enqueue_ok(tl_fns::messages::ReorderStickerSets::CONSTRUCTOR_ID, &true);
    live.reorder_sticker_sets(
        StickersType::Stickers,
        &[StickerSetId(40_001), StickerSetId(40_002)],
    )
    .expect("reorder");
    let raw_reorder =
        fake.recorded_requests_of(tl_fns::messages::ReorderStickerSets::CONSTRUCTOR_ID);
    assert_eq!(raw_reorder.len(), 1);
    let parsed_reorder =
        tl_fns::messages::ReorderStickerSets::from_bytes(&raw_reorder[0]).expect("deser");
    assert_eq!(parsed_reorder.order, vec![s1_id, s2_id]);
    assert!(!parsed_reorder.masks);
    assert!(!parsed_reorder.emojis);

    // Uninstall s2 -> sends messages.uninstallStickerSet
    fake.enqueue_ok(tl_fns::messages::UninstallStickerSet::CONSTRUCTOR_ID, &true);
    assert!(ChatHelpersBackend::remove_sticker_set(
        &mut live,
        StickerSetId(40_002)
    ));
    assert!(!live.is_set_installed(StickerSetId(40_002)));
}

#[test]
fn test_search_sticker_sets_and_get_stickers_by_emoji() {
    let (fake, live) = start_live_harness();
    // Seed empty installed/recent so stickers_for_emoji focuses on remote GetStickers.
    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 0,
            sets: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::RecentStickers::RecentStickers(tl_types::messages::RecentStickers {
            hash: 0,
            packs: Vec::new(),
            stickers: Vec::new(),
            dates: Vec::new(),
        }),
    );
    let fire_doc = synthetic_sticker_doc(5001, 50_001, 303, "🔥", "application/x-tgsticker", false);
    fake.enqueue_ok(
        tl_fns::messages::GetStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::Stickers::Stickers(tl_types::messages::Stickers {
            hash: 5050,
            stickers: vec![fire_doc],
        }),
    );

    let hits = live.stickers_for_emoji("🔥");
    assert_eq!(hits, vec![ChatDocId(5001)]);
    assert!(live.sticker_document(ChatDocId(5001)).is_some());

    // Second emoji lookup within 1 hour hits cache (no extra GetStickers RPC).
    fake.clear_recorded();
    let cached_hits = live.stickers_for_emoji("🔥");
    assert_eq!(cached_hits, vec![ChatDocId(5001)]);
    assert!(fake.recorded_ctors().is_empty());

    // Search remote sticker sets via messages.searchStickerSets.
    let remote_doc = synthetic_sticker_doc(5002, 50_002, 404, "✨", "image/webp", false);
    let remote_hdr = synthetic_sticker_set(
        50_002,
        404,
        66,
        "Nebula Art",
        "nebula_art",
        1,
        None,
        false,
        false,
    );
    fake.enqueue_ok(
        tl_fns::messages::SearchStickerSets::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickerSets::FoundStickerSets(
            tl_types::messages::FoundStickerSets {
                hash: 6060,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: remote_hdr,
                        cover: remote_doc,
                    },
                )],
            },
        ),
    );
    let search_hits = live.search_stickers("nebula");
    assert!(search_hits.contains(&ChatDocId(5002)));
    assert_eq!(
        fake.recorded_requests_of(tl_fns::messages::SearchStickerSets::CONSTRUCTOR_ID)
            .len(),
        1
    );
}

#[test]
fn test_saved_gifs_save_gif_and_at_gif_inline_search() {
    let (fake, mut live) = start_live_harness();
    let tiny_png = make_tiny_png(4, 4, [20, 140, 220, 255]);
    let saved_gif = synthetic_gif_doc(6001, 200, 120, "aurora_wave.mp4", Some(tiny_png));
    let search_gif = synthetic_gif_doc(6002, 180, 100, "nebula_cat.mp4", None);

    fake.enqueue_ok(
        tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedGifs::SavedGifs(tl_types::messages::SavedGifs {
            hash: 6001,
            gifs: vec![saved_gif],
        }),
    );

    let saved = live.saved_gifs();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, ChatDocId(6001));
    assert!(saved[0].saved);
    assert_eq!(saved[0].width, 200);
    assert_eq!(saved[0].height, 120);

    // gif_frame_rgba decodes cached PNG thumbnail cleanly.
    let (fw, fh, rgba) = live.gif_frame_rgba(ChatDocId(6001), 0).expect("gif frame");
    assert_eq!((fw, fh), (4, 4));
    assert_eq!(rgba.len(), 4 * 4 * 4);
    assert!(!live.gif_categories().is_empty());

    // Search GIFs via @gif inline bot: ResolveUsername("gif") + GetInlineBotResults.
    let gif_bot_user = synthetic_tl_user(900_001, "GIF Bot", "gif", false, true, false);
    fake.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 900_001 }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(gif_bot_user)],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID,
        &tl_enums::messages::BotResults::BotResults(tl_types::messages::BotResults {
            gallery: true,
            query_id: 4242,
            next_offset: Some("page2".to_owned()),
            switch_pm: None,
            switch_webview: None,
            results: vec![tl_enums::BotInlineResult::BotInlineMediaResult(
                tl_types::BotInlineMediaResult {
                    id: "res_1".to_owned(),
                    r#type: "gif".to_owned(),
                    photo: None,
                    document: Some(search_gif),
                    title: Some("Nebula Cat".to_owned()),
                    description: None,
                    send_message: tl_enums::BotInlineMessage::MediaAuto(
                        tl_types::BotInlineMessageMediaAuto {
                            invert_media: false,
                            message: String::new(),
                            entities: None,
                            reply_markup: None,
                        },
                    ),
                },
            )],
            cache_time: 120,
            users: Vec::new(),
        }),
    );

    let found = live.search_gifs("cats");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, ChatDocId(6002));
    assert_eq!(found[0].title, "Nebula Cat");
    assert!(!found[0].saved);
    assert_eq!(
        fake.recorded_requests_of(tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_requests_of(tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Repeated search within cache_time hits cache without any RPC.
    fake.clear_recorded();
    let cached_found = live.search_gifs("cats");
    assert_eq!(cached_found.len(), 1);
    assert!(fake.recorded_ctors().is_empty());

    // Save the searched GIF -> calls messages.saveGif { unsave: false }.
    fake.enqueue_ok(tl_fns::messages::SaveGif::CONSTRUCTOR_ID, &true);
    assert!(live.toggle_saved_gif(ChatDocId(6002)));
    let saved_now = live.saved_gifs();
    assert_eq!(saved_now.len(), 2);
    assert_eq!(saved_now[0].id, ChatDocId(6002));

    // Unsave it -> calls messages.saveGif { unsave: true }.
    fake.enqueue_ok(tl_fns::messages::SaveGif::CONSTRUCTOR_ID, &true);
    assert!(!live.toggle_saved_gif(ChatDocId(6002)));
    assert_eq!(live.saved_gifs().len(), 1);

    let save_reqs = fake.recorded_requests_of(tl_fns::messages::SaveGif::CONSTRUCTOR_ID);
    assert_eq!(save_reqs.len(), 2);
    let req0 = tl_fns::messages::SaveGif::from_bytes(&save_reqs[0]).expect("deser save");
    let req1 = tl_fns::messages::SaveGif::from_bytes(&save_reqs[1]).expect("deser unsave");
    assert!(!req0.unsave);
    assert!(req1.unsave);
}

#[test]
fn test_custom_emoji_sets_and_get_custom_emoji_documents_batching() {
    let (fake, live) = start_live_harness();
    let emoji_set_id = 70_001i64;
    let emoji_hdr = synthetic_sticker_set(
        emoji_set_id,
        909,
        77,
        "Crystal Emoji",
        "crystal_emoji",
        1,
        Some(1_750_000_000),
        false,
        true,
    );
    let emoji_doc = synthetic_custom_emoji_doc(7001, emoji_set_id, 909, "💎", false);

    fake.enqueue_ok(
        tl_fns::messages::GetEmojiStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 77,
            sets: vec![emoji_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: emoji_hdr,
            packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: "💎".to_owned(),
                documents: vec![7001],
            })],
            keywords: Vec::new(),
            documents: vec![emoji_doc],
        }),
    );

    let set_ids = live.custom_emoji_set_ids();
    assert_eq!(set_ids, vec![StickerSetId(70_001)]);
    let doc = live
        .sticker_document(ChatDocId(7001))
        .expect("custom emoji doc cached");
    assert!(doc.is_premium_emoji);
    assert_eq!(doc.alt_emoji, "💎");

    // Batch fetch 125 uncached custom emoji document IDs -> splits into 100 + 25 RPCs!
    fake.set_dynamic_handler(Arc::new(move |ctor, bytes| {
        if ctor != tl_fns::messages::GetCustomEmojiDocuments::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::messages::GetCustomEmojiDocuments::from_bytes(bytes).ok()?;
        let docs: Vec<tl_enums::Document> = req
            .document_id
            .iter()
            .map(|&id| synthetic_custom_emoji_doc(id, emoji_set_id, 909, "✨", true))
            .collect();
        Some(Ok(docs.to_bytes()))
    }));

    fake.clear_recorded();
    let requested_ids: Vec<ChatDocId> = (8000..8125).map(ChatDocId).collect();
    let fetched = live
        .get_custom_emoji_documents(&requested_ids)
        .expect("batch custom emoji docs");
    assert_eq!(fetched.len(), 125);
    assert_eq!(
        fake.recorded_requests_of(tl_fns::messages::GetCustomEmojiDocuments::CONSTRUCTOR_ID)
            .len(),
        2
    );

    // Requesting the same 125 IDs again hits cache with ZERO RPCs.
    fake.clear_recorded();
    let cached = live
        .get_custom_emoji_documents(&requested_ids)
        .expect("cached custom emoji docs");
    assert_eq!(cached.len(), 125);
    assert!(fake.recorded_ctors().is_empty());
}

#[test]
fn test_settings_backend_sticker_sets_archive_and_remove() {
    let (fake, mut live) = start_live_harness();
    let active_id = 80_001i64;
    let emoji_id = 80_002i64;
    let archived_id = 80_003i64;

    let active_hdr = synthetic_sticker_set(
        active_id,
        111,
        10,
        "Active Pack",
        "active_pack",
        1,
        Some(1_749_000_000),
        false,
        false,
    );
    let emoji_hdr = synthetic_sticker_set(
        emoji_id,
        222,
        20,
        "Emoji Pack",
        "emoji_pack",
        1,
        Some(1_749_000_000),
        false,
        true,
    );
    let archived_hdr = synthetic_sticker_set(
        archived_id,
        333,
        30,
        "Old Pack",
        "old_pack",
        1,
        None,
        true,
        false,
    );

    let d1 = synthetic_sticker_doc(8001, active_id, 111, "😄", "application/x-tgsticker", false);
    let d2 = synthetic_custom_emoji_doc(8002, emoji_id, 222, "✨", true);
    let d3 = synthetic_sticker_doc(8003, archived_id, 333, "👍", "image/webp", false);

    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 10,
            sets: vec![active_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: active_hdr,
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![d1],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetEmojiStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 20,
            sets: vec![emoji_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: emoji_hdr,
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![d2],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetArchivedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::ArchivedStickers::ArchivedStickers(
            tl_types::messages::ArchivedStickers {
                count: 1,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: archived_hdr,
                        cover: d3,
                    },
                )],
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetArchivedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::ArchivedStickers::ArchivedStickers(
            tl_types::messages::ArchivedStickers {
                count: 0,
                sets: Vec::new(),
            },
        ),
    );

    let sets = SettingsBackend::sticker_sets(&live);
    assert_eq!(sets.len(), 3);
    let active = sets
        .iter()
        .find(|s| s.id == active_id as u64)
        .expect("active");
    assert!(!active.is_emoji);
    assert!(!active.archived);
    assert!(active.animated);
    let emoji = sets
        .iter()
        .find(|s| s.id == emoji_id as u64)
        .expect("emoji");
    assert!(emoji.is_emoji);
    assert!(!emoji.archived);
    let archived = sets
        .iter()
        .find(|s| s.id == archived_id as u64)
        .expect("archived");
    assert!(archived.archived);

    // Archive active set -> messages.toggleStickerSets
    fake.enqueue_ok(tl_fns::messages::ToggleStickerSets::CONSTRUCTOR_ID, &true);
    SettingsBackend::set_sticker_set_archived(&mut live, active_id as u64, true);
    let after_archive = SettingsBackend::sticker_sets(&live);
    assert!(
        after_archive
            .iter()
            .find(|s| s.id == active_id as u64)
            .expect("archived active")
            .archived
    );

    // Remove archived set -> messages.uninstallStickerSet
    fake.enqueue_ok(tl_fns::messages::UninstallStickerSet::CONSTRUCTOR_ID, &true);
    SettingsBackend::remove_sticker_set(&mut live, archived_id as u64);
    let after_remove = SettingsBackend::sticker_sets(&live);
    assert!(!after_remove.iter().any(|s| s.id == archived_id as u64));
}

#[test]
fn test_chat_mention_candidates_channel_search_local_members_and_bot_commands() {
    let (fake, mut live) = start_live_harness();

    // 1. Seed a supergroup channel + a recent local message author in Session.
    let chan_id = ChannelId(90_001);
    let chan_peer = DataPeerId::from_channel(chan_id);
    let local_author_uid = 700_010u64;
    {
        let rt = live.runtime();
        let session_arc = Arc::clone(rt.session());
        rt.tokio_handle().block_on(async move {
            let mut s = session_arc.write().await;
            let mut ch = Channel::new(chan_id);
            ch.access_hash = 987_654;
            ch.set_flags(ChannelFlags::MEGAGROUP);
            ch.set_name("Alpine Devs", "alpine_devs");
            s.upsert_channel(ch, tg_data::PeerUpdateFlags::NAME);

            let mut local_u = tg_data::User::new(DataUserId(local_author_uid), false);
            local_u.access_hash = 12345;
            local_u.set_name("Rowan", "", "Rowan", "rowan_dev");
            s.upsert_user(local_u, tg_data::PeerUpdateFlags::NAME);

            let msg = Message::new_text(
                chan_peer,
                DataMsgId(5),
                Some(DataPeerId::from_user(DataUserId(local_author_uid))),
                1_750_000_000,
                DataText::plain("hello team"),
            );
            s.insert_message(msg);
        });
    }

    let remote_u1 = synthetic_tl_user(700_020, "Bramble", "bramble_ui", false, false, false);
    let remote_u2 = synthetic_tl_user(700_021, "Deleted", "", false, false, true);
    fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 2,
                participants: vec![
                    tl_enums::ChannelParticipant::ChannelParticipant(
                        tl_types::ChannelParticipant {
                            user_id: 700_020,
                            date: 1_750_000_000,
                            subscription_until_date: None,
                            rank: None,
                        },
                    ),
                    tl_enums::ChannelParticipant::ChannelParticipant(
                        tl_types::ChannelParticipant {
                            user_id: 700_021,
                            date: 1_750_000_000,
                            subscription_until_date: None,
                            rank: None,
                        },
                    ),
                ],
                chats: Vec::new(),
                users: vec![
                    tl_enums::User::User(remote_u1),
                    tl_enums::User::User(remote_u2),
                ],
            },
        ),
    );

    let cands = HistoryBackend::chat_mention_candidates(&live, PeerId(chan_peer.0));
    // Local recent author first, then remote active participant; deleted user excluded.
    assert_eq!(cands.len(), 2);
    assert_eq!(cands[0].user_id, local_author_uid);
    assert_eq!(cands[0].username.as_deref(), Some("rowan_dev"));
    assert_eq!(cands[1].user_id, 700_020);
    assert_eq!(cands[1].username.as_deref(), Some("bramble_ui"));

    // Cache hit within TTL: no extra channels.getParticipants RPC.
    fake.clear_recorded();
    let cands_cached = HistoryBackend::chat_mention_candidates(&live, PeerId(chan_peer.0));
    assert_eq!(cands_cached.len(), 2);
    assert!(fake.recorded_ctors().is_empty());

    // Advance past participants TTL -> sends non-zero hash and handles NotModified.
    live.set_chat_helpers_now_ms(Some(1_750_000_000_000 + PARTICIPANTS_CACHE_TTL_MS + 1000));
    fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::NotModified,
    );
    let cands_refreshed = HistoryBackend::chat_mention_candidates(&live, PeerId(chan_peer.0));
    assert_eq!(cands_refreshed.len(), 2);
    let gp_reqs = fake.recorded_requests_of(tl_fns::channels::GetParticipants::CONSTRUCTOR_ID);
    assert_eq!(gp_reqs.len(), 1);
    let parsed_gp =
        tl_fns::channels::GetParticipants::from_bytes(&gp_reqs[0]).expect("deser getParticipants");
    assert_ne!(parsed_gp.hash, 0);

    // Filtered mention search sends ChannelParticipantsSearch { q: "bram" }.
    fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::NotModified,
    );
    let filtered = live.search_chat_mention_candidates(PeerId(chan_peer.0), "bram");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].user_id, 700_020);

    // 2. Basic group: fetches messages.getFullChat -> local members + bot commands!
    let group_id = ChatId(90_002);
    let group_peer = DataPeerId::from_chat(group_id);
    {
        let rt = live.runtime();
        let session_arc = Arc::clone(rt.session());
        rt.tokio_handle().block_on(async move {
            let mut s = session_arc.write().await;
            let mut chat = Chat::new(group_id);
            chat.set_name("Studio Group");
            s.upsert_chat(chat, tg_data::PeerUpdateFlags::NAME);
        });
    }

    let bot_user = synthetic_tl_user(700_030, "Helper Bot", "helper_bot", false, true, false);
    let member_user = synthetic_tl_user(700_031, "Cedar", "cedar_ops", false, false, false);
    fake.enqueue_ok(
        tl_fns::messages::GetFullChat::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
            full_chat: tl_enums::ChatFull::ChatFull(tl_types::ChatFull {
                can_set_username: false,
                has_scheduled: false,
                translations_disabled: false,
                has_welcome_messages: false,
                id: 90_002,
                about: "Basic group".to_owned(),
                participants: tl_enums::ChatParticipants::ChatParticipants(
                    tl_types::ChatParticipants {
                        chat_id: 90_002,
                        participants: vec![
                            tl_enums::ChatParticipant::ChatParticipant(tl_types::ChatParticipant {
                                user_id: 700_030,
                                inviter_id: 700_001,
                                date: 1_750_000_000,
                                rank: None,
                            }),
                            tl_enums::ChatParticipant::ChatParticipant(tl_types::ChatParticipant {
                                user_id: 700_031,
                                inviter_id: 700_001,
                                date: 1_750_000_000,
                                rank: None,
                            }),
                        ],
                        version: 1,
                    },
                ),
                chat_photo: None,
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
                exported_invite: None,
                bot_info: Some(vec![tl_enums::BotInfo::BotInfo(tl_types::BotInfo {
                    has_preview_medias: false,
                    user_id: Some(700_030),
                    description: Some("Helps the studio".to_owned()),
                    description_photo: None,
                    description_document: None,
                    commands: Some(vec![tl_enums::BotCommand::BotCommand(
                        tl_types::BotCommand {
                            ephemeral: false,
                            command: "status".to_owned(),
                            description: "Check status".to_owned(),
                        },
                    )]),
                    menu_button: None,
                    privacy_policy_url: None,
                    app_settings: None,
                    verifier_settings: None,
                })]),
                pinned_msg_id: None,
                folder_id: None,
                call: None,
                ttl_period: None,
                groupcall_default_join_as: None,
                theme_emoticon: None,
                requests_pending: None,
                recent_requesters: None,
                available_reactions: None,
                reactions_limit: None,
            }),
            chats: Vec::new(),
            users: vec![
                tl_enums::User::User(bot_user),
                tl_enums::User::User(member_user),
            ],
        }),
    );

    let group_cands = HistoryBackend::chat_mention_candidates(&live, PeerId(group_peer.0));
    assert_eq!(group_cands.len(), 2);
    assert!(group_cands.iter().any(|c| c.user_id == 700_030));
    assert!(group_cands.iter().any(|c| c.user_id == 700_031));

    let bot_cmds = live.chat_bot_commands(PeerId(group_peer.0));
    let helper_cmds = bot_cmds
        .get(&ChatUserId(700_030))
        .expect("helper_bot commands");
    assert_eq!(helper_cmds.len(), 1);
    assert_eq!(helper_cmds[0].command, "status");

    // prepare_chat_autocomplete populates ChatHelpersBackend borrowed slices too.
    live.prepare_chat_autocomplete(PeerId(group_peer.0));
    assert!(
        live.autocomplete_bot_commands()
            .contains_key(&ChatUserId(700_030))
    );
}

#[test]
fn test_sticker_media_downloads_tgs_and_static_and_caches() {
    let (fake, live) = start_live_harness();
    let set_id = 91_001i64;
    let tgs_doc = synthetic_sticker_doc(9101, set_id, 123, "✨", "application/x-tgsticker", false);
    let png_doc = synthetic_sticker_doc(9102, set_id, 123, "🌟", "image/webp", false);
    let set_hdr = synthetic_sticker_set(
        set_id,
        123,
        91,
        "Media Pack",
        "media_pack",
        2,
        Some(1_750_000_000),
        false,
        false,
    );

    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 91,
            sets: vec![set_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: set_hdr,
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![tgs_doc, png_doc],
        }),
    );
    live.refresh_all_stickers(true).expect("load set");

    // 1. Download TGS sticker asynchronously via upload.getFile (cache miss returns None immediately).
    let tgs_bytes = vec![0x1f, 0x8b, 0x08, 0x00, 0x01, 0x02];
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: 1_750_000_000,
            bytes: tgs_bytes.clone(),
        }),
    );
    assert!(
        live.sticker_media(ChatDocId(9101)).is_none(),
        "first call on cache miss must return None without blocking UI thread"
    );
    let media_tgs = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(m) = live.sticker_media(ChatDocId(9101)) {
                break m;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for async TGS sticker_media prefetch"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    match media_tgs {
        StickerMedia::LottieTgs(b) => assert_eq!(b, tgs_bytes),
        other => panic!("expected LottieTgs, got {other:?}"),
    }

    // Subsequent call uses cache (zero RPCs).
    fake.clear_recorded();
    assert!(live.sticker_media(ChatDocId(9101)).is_some());
    assert!(fake.recorded_ctors().is_empty());

    // 2. Download static sticker image asynchronously via upload.getFile.
    let png_bytes = make_tiny_png(6, 6, [250, 180, 40, 255]);
    fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FilePng,
            mtime: 1_750_000_000,
            bytes: png_bytes,
        }),
    );
    assert!(live.sticker_media(ChatDocId(9102)).is_none());
    let media_static = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(m) = live.sticker_media(ChatDocId(9102)) {
                break m;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for async static sticker_media prefetch"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    match media_static {
        StickerMedia::StaticRgba {
            width,
            height,
            rgba,
        } => {
            assert_eq!((width, height), (6, 6));
            assert_eq!(rgba.len(), 6 * 6 * 4);
        }
        other => panic!("expected StaticRgba, got {other:?}"),
    }
}

#[test]
fn test_install_sticker_set_archive_result_archives_evicted_set() {
    let (fake, mut live) = start_live_harness();
    let old_id = 41_001i64;
    let new_id = 41_002i64;
    let d_old = synthetic_sticker_doc(4101, old_id, 111, "🌿", "image/webp", false);
    let d_new = synthetic_sticker_doc(4102, new_id, 222, "⚡", "image/webp", false);
    let old_hdr = synthetic_sticker_set(
        old_id,
        111,
        10,
        "Older Pack",
        "older_pack",
        1,
        Some(1_749_000_000),
        false,
        false,
    );
    let old_archived_hdr = synthetic_sticker_set(
        old_id,
        111,
        10,
        "Older Pack",
        "older_pack",
        1,
        Some(1_749_000_000),
        true,
        false,
    );
    let new_hdr = synthetic_sticker_set(
        new_id,
        222,
        20,
        "Fresh Pack",
        "fresh_pack",
        1,
        None,
        false,
        false,
    );

    fake.enqueue_ok(
        tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 10,
            sets: vec![old_hdr.clone()],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: old_hdr,
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![d_old.clone()],
        }),
    );
    live.refresh_all_stickers(true).expect("load old set");
    assert!(live.is_set_installed(StickerSetId(old_id as u64)));

    fake.enqueue_ok(
        tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: false,
                hash: 20,
                count: 1,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: new_hdr.clone(),
                        cover: d_new.clone(),
                    },
                )],
                unread: Vec::new(),
            },
        ),
    );
    let _ = live.featured_sticker_sections();

    // Server returns StickerSetInstallResult::Archive evicting `old_id`.
    fake.enqueue_ok(
        tl_fns::messages::InstallStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSetInstallResult::Archive(
            tl_types::messages::StickerSetInstallResultArchive {
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: old_archived_hdr,
                        cover: d_old,
                    },
                )],
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: synthetic_sticker_set(
                new_id,
                222,
                20,
                "Fresh Pack",
                "fresh_pack",
                1,
                Some(1_750_000_000),
                false,
                false,
            ),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![d_new],
        }),
    );

    assert!(live.install_sticker_set(StickerSetId(new_id as u64)));
    assert!(live.is_set_installed(StickerSetId(new_id as u64)));
    assert!(!live.is_set_installed(StickerSetId(old_id as u64)));

    // SettingsBackend::sticker_sets exposes new pack as active and evicted pack as archived.
    fake.enqueue_ok(
        tl_fns::messages::GetEmojiStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::AllStickers::AllStickers(tl_types::messages::AllStickers {
            hash: 0,
            sets: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetArchivedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::ArchivedStickers::ArchivedStickers(
            tl_types::messages::ArchivedStickers {
                count: 0,
                sets: Vec::new(),
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetArchivedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::ArchivedStickers::ArchivedStickers(
            tl_types::messages::ArchivedStickers {
                count: 0,
                sets: Vec::new(),
            },
        ),
    );
    let sets = SettingsBackend::sticker_sets(&live);
    let fresh = sets
        .iter()
        .find(|s| s.id == new_id as u64)
        .expect("fresh set");
    let evicted = sets
        .iter()
        .find(|s| s.id == old_id as u64)
        .expect("evicted set");
    assert!(!fresh.archived);
    assert!(evicted.archived);
}

#[test]
fn test_supergroup_full_channel_bot_commands_and_mentions() {
    let (fake, mut live) = start_live_harness();
    let chan_id = ChannelId(92_001);
    let chan_peer = DataPeerId::from_channel(chan_id);
    {
        let rt = live.runtime();
        let session_arc = Arc::clone(rt.session());
        rt.tokio_handle().block_on(async move {
            let mut s = session_arc.write().await;
            let mut ch = Channel::new(chan_id);
            ch.access_hash = 654_321;
            ch.set_flags(ChannelFlags::MEGAGROUP);
            ch.set_name("Fjord Ops", "fjord_ops");
            s.upsert_channel(ch, tg_data::PeerUpdateFlags::NAME);
        });
    }

    let member = synthetic_tl_user(701_001, "Linden", "linden_eng", false, false, false);
    let bot = synthetic_tl_user(701_002, "Deploy Bot", "deploy_bot", false, true, false);
    fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::ChannelParticipant(
                    tl_types::ChannelParticipant {
                        user_id: 701_001,
                        date: 1_750_000_000,
                        subscription_until_date: None,
                        rank: None,
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(member)],
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
            full_chat: tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
                can_view_participants: true,
                can_set_username: false,
                can_set_stickers: false,
                hidden_prehistory: false,
                can_set_location: false,
                has_scheduled: false,
                can_view_stats: false,
                blocked: false,
                can_delete_channel: false,
                antispam: false,
                participants_hidden: false,
                translations_disabled: false,
                stories_pinned_available: false,
                view_forum_as_messages: false,
                restricted_sponsored: false,
                can_view_revenue: false,
                paid_media_allowed: false,
                can_view_stars_revenue: false,
                paid_reactions_available: false,
                stargifts_available: false,
                paid_messages_available: false,
                has_welcome_messages: false,
                id: 92_001,
                about: "Supergroup with bot".to_owned(),
                participants_count: Some(2),
                admins_count: None,
                kicked_count: None,
                banned_count: None,
                online_count: None,
                read_inbox_max_id: 0,
                read_outbox_max_id: 0,
                unread_count: 0,
                chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
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
                exported_invite: None,
                bot_info: vec![tl_enums::BotInfo::BotInfo(tl_types::BotInfo {
                    has_preview_medias: false,
                    user_id: Some(701_002),
                    description: Some("Release helper".to_owned()),
                    description_photo: None,
                    description_document: None,
                    commands: Some(vec![tl_enums::BotCommand::BotCommand(
                        tl_types::BotCommand {
                            ephemeral: false,
                            command: "deploy".to_owned(),
                            description: "Trigger build".to_owned(),
                        },
                    )]),
                    menu_button: None,
                    privacy_policy_url: None,
                    app_settings: None,
                    verifier_settings: None,
                })],
                migrated_from_chat_id: None,
                migrated_from_max_id: None,
                pinned_msg_id: None,
                stickerset: None,
                available_min_id: None,
                folder_id: None,
                linked_chat_id: None,
                location: None,
                slowmode_seconds: None,
                slowmode_next_send_date: None,
                stats_dc: None,
                pts: 10,
                call: None,
                ttl_period: None,
                pending_suggestions: None,
                groupcall_default_join_as: None,
                theme_emoticon: None,
                requests_pending: None,
                recent_requesters: None,
                default_send_as: None,
                available_reactions: None,
                reactions_limit: None,
                stories: None,
                wallpaper: None,
                boosts_applied: None,
                boosts_unrestrict: None,
                emojiset: None,
                bot_verification: None,
                stargifts_count: None,
                send_paid_messages_stars: None,
                main_tab: None,
                guard_bot_id: None,
                share_theme: None,
            }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(bot)],
        }),
    );

    let cands = HistoryBackend::chat_mention_candidates(&live, PeerId(chan_peer.0));
    assert!(cands.iter().any(|c| c.user_id == 701_001));
    assert!(cands.iter().any(|c| c.user_id == 701_002));

    let cmds = live.chat_bot_commands(PeerId(chan_peer.0));
    let bot_cmds = cmds.get(&ChatUserId(701_002)).expect("deploy_bot cmds");
    assert_eq!(bot_cmds.len(), 1);
    assert_eq!(bot_cmds[0].command, "deploy");

    live.prepare_chat_autocomplete(PeerId(chan_peer.0));
    assert!(
        live.autocomplete_bot_commands()
            .contains_key(&ChatUserId(701_002))
    );
}

#[test]
fn test_read_featured_stickers_retains_unread_on_rpc_failure_and_recovers() {
    let (fake, live) = start_live_harness();
    let feat_id = 31_001i64;
    let cover_doc = synthetic_sticker_doc(3101, feat_id, 456, "🌟", "image/webp", false);
    let feat_hdr = synthetic_sticker_set(
        feat_id,
        456,
        78,
        "Starlight Pack",
        "starlight_pack",
        1,
        None,
        false,
        false,
    );

    fake.enqueue_ok(
        tl_fns::messages::GetFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: false,
                hash: 78,
                count: 1,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: feat_hdr,
                        cover: cover_doc,
                    },
                )],
                unread: vec![feat_id],
            },
        ),
    );
    let _ = live.featured_sticker_sections();
    assert_eq!(live.featured_stickers_unread_count(), 1);

    // Queue read and invoke without an enqueued response -> fails and preserves UNREAD state.
    assert!(live.queue_read_featured_sticker_set(StickerSetId(feat_id as u64)));
    assert!(live.flush_read_featured_sticker_sets().is_err());
    assert_eq!(live.featured_stickers_unread_count(), 1);

    // Retry with an OK response -> flushes cleanly and clears UNREAD.
    fake.enqueue_ok(
        tl_fns::messages::ReadFeaturedStickers::CONSTRUCTOR_ID,
        &true,
    );
    let flushed = live
        .flush_read_featured_sticker_sets()
        .expect("retry read featured");
    assert_eq!(flushed, vec![StickerSetId(feat_id as u64)]);
    assert_eq!(live.featured_stickers_unread_count(), 0);
}

#[test]
fn test_hash_not_modified_for_saved_gifs_emoji_stickers_and_search_sets() {
    let (fake, live) = start_live_harness();
    let gif_doc = synthetic_gif_doc(6201, 160, 90, "comet.mp4", None);
    fake.enqueue_ok(
        tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedGifs::SavedGifs(tl_types::messages::SavedGifs {
            hash: 6201,
            gifs: vec![gif_doc],
        }),
    );
    assert_eq!(live.saved_gifs().len(), 1);

    // Advance past 1-hour TTL -> sends saved-gifs hash (6201) and preserves cache on NotModified.
    fake.clear_recorded();
    live.set_chat_helpers_now_ms(Some(1_750_000_000_000 + STICKERS_UPDATE_TIMEOUT_MS + 1000));
    fake.enqueue_ok(
        tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedGifs::NotModified,
    );
    assert_eq!(live.saved_gifs().len(), 1);
    let gif_reqs = fake.recorded_requests_of(tl_fns::messages::GetSavedGifs::CONSTRUCTOR_ID);
    assert_eq!(gif_reqs.len(), 1);
    let parsed_gif = tl_fns::messages::GetSavedGifs::from_bytes(&gif_reqs[0]).expect("deser");
    assert_eq!(parsed_gif.hash, 6201);

    // GetStickers by emoji: first fetch caches hash, expired refresh sends previous hash + handles NotModified.
    let emoji_doc = synthetic_sticker_doc(5201, 52_001, 101, "🎯", "image/webp", false);
    fake.enqueue_ok(
        tl_fns::messages::GetStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::Stickers::Stickers(tl_types::messages::Stickers {
            hash: 9191,
            stickers: vec![emoji_doc],
        }),
    );
    assert_eq!(
        live.fetch_stickers_by_emoji("🎯").expect("emoji fetch"),
        vec![ChatDocId(5201)]
    );

    fake.clear_recorded();
    live.set_chat_helpers_now_ms(Some(
        1_750_000_000_000 + 2 * STICKERS_UPDATE_TIMEOUT_MS + 2000,
    ));
    fake.enqueue_ok(
        tl_fns::messages::GetStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::Stickers::NotModified,
    );
    assert_eq!(
        live.fetch_stickers_by_emoji("🎯")
            .expect("emoji not modified"),
        vec![ChatDocId(5201)]
    );
    let emoji_reqs = fake.recorded_requests_of(tl_fns::messages::GetStickers::CONSTRUCTOR_ID);
    assert_eq!(emoji_reqs.len(), 1);
    let parsed_emoji = tl_fns::messages::GetStickers::from_bytes(&emoji_reqs[0]).expect("deser");
    assert_eq!(parsed_emoji.hash, 9191);

    // SearchStickerSets: caches hash and revalidates with NotModified after TTL.
    let found_doc = synthetic_sticker_doc(5202, 52_002, 202, "🌌", "image/webp", false);
    let found_hdr = synthetic_sticker_set(
        52_002,
        202,
        52,
        "Cosmos Pack",
        "cosmos_pack",
        1,
        None,
        false,
        false,
    );
    fake.enqueue_ok(
        tl_fns::messages::SearchStickerSets::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickerSets::FoundStickerSets(
            tl_types::messages::FoundStickerSets {
                hash: 7373,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: found_hdr,
                        cover: found_doc,
                    },
                )],
            },
        ),
    );
    assert_eq!(
        live.search_sticker_sets("cosmos", false)
            .expect("search sets"),
        vec![StickerSetId(52_002)]
    );

    fake.clear_recorded();
    live.set_chat_helpers_now_ms(Some(
        1_750_000_000_000 + 3 * STICKERS_UPDATE_TIMEOUT_MS + 3000,
    ));
    fake.enqueue_ok(
        tl_fns::messages::SearchStickerSets::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickerSets::NotModified,
    );
    assert_eq!(
        live.search_sticker_sets("cosmos", false)
            .expect("search sets not modified"),
        vec![StickerSetId(52_002)]
    );
    let search_reqs =
        fake.recorded_requests_of(tl_fns::messages::SearchStickerSets::CONSTRUCTOR_ID);
    assert_eq!(search_reqs.len(), 1);
    let parsed_search =
        tl_fns::messages::SearchStickerSets::from_bytes(&search_reqs[0]).expect("deser");
    assert_eq!(parsed_search.hash, 7373);
}
