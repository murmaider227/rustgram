//! Scripted fake-server tests for WP-67: the 17 sticker/emoji search, clear-recents,
//! attached stickers, emoji-keyword languages, default emoji catalogs, and reaction
//! privacy/moderation RPCs (13 `messages.*` + 4 `account.getDefault*Emojis`).
//!
//! Asserts exact TL wire bytes, constructor IDs, flags, hash-cached `NotModified`
//! handling, TTL caching, hash reset on RPC errors, document pool hydration, and
//! `Updates` application through `SessionRuntime::apply_updates_blocking`.
//!
//! Zero network traffic, synthetic IDs and fictional names only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::cached_lists::{
    EMOJI_KEYWORDS_REFRESH_MS, EMOJI_STATUSES_REFRESH_MS, REACTIONS_FULL_LIST_REFRESH_MS,
};
use tg_api::hash::count_document_vector_hash;
use tg_api::stickers::STICKERS_UPDATE_TIMEOUT_MS;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_chat::{DocumentId as ChatDocId, StickerSetId};
use tg_data::{
    Channel, ChannelFlags, ChannelId, DocumentData, DocumentId as DataDocId,
    FullMsgId as DataFullMsgId, Message, MessageContent, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId, PeerUpdateFlags, PhotoData, PhotoId, ReactionId,
    TextWithEntities as DataText, User, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Identifiable, RemoteCall, Serializable, enums as tl_enums,
    functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use crate::session::LiveSession;
use tg_app_core::chat_helpers::backend::{
    AttachedStickersMedia, CLOUD_SEARCH_PAGE_LIMIT, ChatHelpersBackend, DefaultEmojiCatalogKind,
    DefaultTagReaction, OLD_FEATURED_LOAD_LIMIT, PaidReactionPrivacy, SearchStickersRequest,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

// =============================================================================
// Scripted In-Process FakeRpcClient
// =============================================================================

type RecordedRpc = (u32, Vec<u8>);
type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
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

    fn enqueue_bool(&self, expected_ctor: u32, value: bool) {
        let ctor = if value { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(ctor.to_le_bytes().to_vec())));
    }

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError::new(code, name))),
        ));
    }

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
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
        let ctor = u32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let popped = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
        };
        match popped {
            Some(Ok(raw)) => <R::Return as tg_tl::Deserializable>::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string())),
            Some(Err(err)) => Err(err),
            None => Err(InvocationError::Dropped),
        }
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

// =============================================================================
// Synthetic Fixture Builders
// =============================================================================

const BASE_NOW_MS: i64 = 1_750_000_000_000;
const SELF_UID: u64 = 100;
const CHANNEL_ID: u64 = 500_001;
const CHANNEL_HASH: u64 = 9_900_501;
const PARTICIPANT_UID: u64 = 200_002;
const PARTICIPANT_HASH: u64 = 8_800_202;

fn make_live_session() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    // Seed initial `updates.getState` so `SessionActor`'s `UpdatesHandler` exits its
    // initial `requesting_difference` state and processes `apply_updates_blocking` replies.
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_750_000_000,
            seq: 0,
            unread_count: 0,
        }),
    );
    let runtime = SessionRuntime::start_on(
        DataUserId(SELF_UID),
        crate::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS));

    // Seed synthetic channel, participant user, and self user in Session.
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        let mut self_user = User::new(DataUserId(SELF_UID), true);
        self_user.access_hash = 111_222;
        self_user.first_name = "SelfBot".to_owned();
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut participant = User::new(DataUserId(PARTICIPANT_UID), false);
        participant.access_hash = PARTICIPANT_HASH;
        participant.first_name = "ReactorOne".to_owned();
        s.upsert_user(participant, PeerUpdateFlags::empty());

        let mut ch = Channel::new(ChannelId(CHANNEL_ID));
        ch.access_hash = CHANNEL_HASH;
        ch.common.name = "Test Broadcast".to_owned();
        ch.flags = ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::empty());
    });

    (rt, rpc, live)
}

fn make_sticker_doc(
    id: i64,
    access_hash: i64,
    alt: &str,
    set_id: i64,
    custom_emoji: bool,
) -> tl_enums::Document {
    let attr = if custom_emoji {
        tl_enums::DocumentAttribute::CustomEmoji(tl_types::DocumentAttributeCustomEmoji {
            free: true,
            text_color: false,
            alt: alt.to_owned(),
            stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id,
                access_hash: set_id * 10,
            }),
        })
    } else {
        tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
            mask: false,
            alt: alt.to_owned(),
            stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id,
                access_hash: set_id * 10,
            }),
            mask_coords: None,
        })
    };
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash,
        file_reference: vec![1, 2, 3, (id & 0xFF) as u8],
        date: 1_700_000_000,
        mime_type: "image/webp".to_owned(),
        size: 4096,
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

fn make_raw_set(
    id: i64,
    access_hash: i64,
    title: &str,
    short_name: &str,
    count: i32,
    emojis: bool,
    masks: bool,
) -> tl_enums::StickerSet {
    tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
        archived: false,
        official: true,
        masks,
        emojis,
        text_color: false,
        channel_emoji_status: false,
        creator: false,
        installed_date: None,
        id,
        access_hash,
        title: title.to_owned(),
        short_name: short_name.to_owned(),
        thumbs: None,
        thumb_dc_id: None,
        thumb_version: None,
        thumb_document_id: None,
        count,
        hash: (id & 0x7FFF_FFFF) as i32,
    })
}

fn channel_input_peer() -> tl_enums::InputPeer {
    tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
        channel_id: CHANNEL_ID as i64,
        access_hash: CHANNEL_HASH as i64,
    })
}

fn participant_input_peer() -> tl_enums::InputPeer {
    tl_enums::InputPeer::User(tl_types::InputPeerUser {
        user_id: PARTICIPANT_UID as i64,
        access_hash: PARTICIPANT_HASH as i64,
    })
}

// =============================================================================
// 1. messages.searchStickers
// =============================================================================

#[test]
fn search_stickers_remote_exact_tl_bytes_not_modified_error_reset_and_doc_pool() {
    let (_rt, rpc, live) = make_live_session();

    let doc1 = make_sticker_doc(7001, 17001, "🐱", 801, false);
    let doc2 = make_sticker_doc(7002, 17002, "😺", 801, false);
    let server_hash: i64 = 0x1122_3344_5566_7788;

    rpc.enqueue_ok(
        tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickers::FoundStickers(tl_types::messages::FoundStickers {
            next_offset: Some(2),
            hash: server_hash,
            stickers: vec![doc1, doc2],
        }),
    );

    let req = SearchStickersRequest {
        emojis: false,
        query: "cat".to_owned(),
        emoticon: String::new(),
        lang_codes: vec!["en".to_owned(), "de".to_owned()],
        offset: 0,
        limit: 0, // should default to CLOUD_SEARCH_PAGE_LIMIT (50)
    };
    let page1 = live.search_stickers_remote(&req).expect("first search");
    assert_eq!(page1.stickers, vec![ChatDocId(7001), ChatDocId(7002)]);
    assert_eq!(page1.next_offset, Some(2));
    assert_eq!(page1.hash, server_hash);
    assert!(!page1.not_modified);

    // Document pool hydrated for immediate UI rendering.
    assert_eq!(
        live.sticker_document(ChatDocId(7001))
            .expect("doc 7001 hydrated")
            .alt_emoji,
        "🐱"
    );
    assert_eq!(
        live.sticker_document(ChatDocId(7002))
            .expect("doc 7002 hydrated")
            .alt_emoji,
        "😺"
    );

    // Exact TL wire bytes check (flags = 0, limit defaulted to 50, hash = 0).
    let expected_first = tl_fns::messages::SearchStickers {
        emojis: false,
        q: "cat".to_owned(),
        emoticon: String::new(),
        lang_code: vec!["en".to_owned(), "de".to_owned()],
        offset: 0,
        limit: CLOUD_SEARCH_PAGE_LIMIT,
        hash: 0,
    };
    let recorded = rpc.recorded_of(tl_fns::messages::SearchStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], expected_first.to_bytes());

    // Inside TTL: cached hit sends 0 extra RPCs.
    let cached_hit = live.search_stickers_remote(&req).expect("cached hit");
    assert_eq!(cached_hit.stickers, page1.stickers);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::SearchStickers::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Advance past 1-hour TTL -> sends cached `server_hash` and handles `NotModified`.
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS + STICKERS_UPDATE_TIMEOUT_MS as i64 + 1));
    rpc.enqueue_ok(
        tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickers::NotModified(
            tl_types::messages::FoundStickersNotModified {
                next_offset: Some(2),
            },
        ),
    );
    let revalidated = live.search_stickers_remote(&req).expect("not modified");
    assert!(revalidated.not_modified);
    assert_eq!(revalidated.stickers, vec![ChatDocId(7001), ChatDocId(7002)]);
    assert_eq!(revalidated.next_offset, Some(2));
    assert_eq!(revalidated.hash, server_hash);

    let expected_second = tl_fns::messages::SearchStickers {
        hash: server_hash,
        ..expected_first.clone()
    };
    let recorded = rpc.recorded_of(tl_fns::messages::SearchStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1], expected_second.to_bytes());

    // RPC error resets cached hash to 0 so the retry sends hash = 0.
    live.set_chat_helpers_now_ms(Some(
        BASE_NOW_MS + (STICKERS_UPDATE_TIMEOUT_MS as i64) * 2 + 2,
    ));
    rpc.enqueue_rpc_err(
        tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        420,
        "FLOOD_WAIT_5",
    );
    assert!(live.search_stickers_remote(&req).is_err());

    rpc.enqueue_ok(
        tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickers::FoundStickers(tl_types::messages::FoundStickers {
            next_offset: None,
            hash: 999,
            stickers: vec![make_sticker_doc(7003, 17003, "🐈", 801, false)],
        }),
    );
    let after_err = live.search_stickers_remote(&req).expect("retry after err");
    assert_eq!(after_err.stickers, vec![ChatDocId(7003)]);
    let recorded = rpc.recorded_of(tl_fns::messages::SearchStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 4);
    assert_eq!(recorded[3], expected_first.to_bytes());

    // Custom emoji mode (`emojis = true`, `flags.0 = 1`) with emoticon filter & empty query.
    let emoji_doc = make_sticker_doc(7101, 17101, "🔥", 901, true);
    rpc.enqueue_ok(
        tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickers::FoundStickers(tl_types::messages::FoundStickers {
            next_offset: Some(10),
            hash: 0, // exercises fallback hash computation via tg_api::hash
            stickers: vec![emoji_doc],
        }),
    );
    let emoji_req = SearchStickersRequest {
        emojis: true,
        query: String::new(),
        emoticon: "🔥".to_owned(),
        lang_codes: vec!["en".to_owned()],
        offset: 5,
        limit: 20,
    };
    let emoji_page = live
        .search_stickers_remote(&emoji_req)
        .expect("emoji search");
    assert_eq!(emoji_page.stickers, vec![ChatDocId(7101)]);
    assert_eq!(emoji_page.next_offset, Some(10));
    assert_eq!(emoji_page.hash, count_document_vector_hash(&[7101]) as i64);
    assert!(live.sticker_document(ChatDocId(7101)).is_some());

    let expected_emoji = tl_fns::messages::SearchStickers {
        emojis: true,
        q: String::new(),
        emoticon: "🔥".to_owned(),
        lang_code: vec!["en".to_owned()],
        offset: 5,
        limit: 20,
        hash: 0,
    };
    let recorded = rpc.recorded_of(tl_fns::messages::SearchStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded[4], expected_emoji.to_bytes());
}

// =============================================================================
// 2. messages.searchEmojiStickerSets
// =============================================================================

#[test]
fn search_emoji_sticker_sets_exact_tl_bytes_not_modified_and_hydration() {
    let (_rt, rpc, live) = make_live_session();
    let cover = make_sticker_doc(8101, 18101, "⭐", 910, true);
    let set = make_raw_set(910, 9100, "Star Emoji", "star_emoji", 12, true, false);
    let server_hash: i64 = 0x0A0B_0C0D_1122_3344;

    rpc.enqueue_ok(
        tl_fns::messages::SearchEmojiStickerSets::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickerSets::FoundStickerSets(
            tl_types::messages::FoundStickerSets {
                hash: server_hash,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered { set, cover },
                )],
            },
        ),
    );

    let first = live
        .search_emoji_sticker_sets("star", true)
        .expect("searchEmojiStickerSets");
    assert_eq!(first.set_ids, vec![StickerSetId(910)]);
    assert_eq!(first.hash, server_hash);
    assert!(!first.not_modified);
    assert!(live.sticker_document(ChatDocId(8101)).is_some());

    let expected_first = tl_fns::messages::SearchEmojiStickerSets {
        exclude_featured: true,
        q: "star".to_owned(),
        hash: 0,
    };
    let recorded = rpc.recorded_of(tl_fns::messages::SearchEmojiStickerSets::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], expected_first.to_bytes());

    // Advance past TTL and verify NotModified preserves hydrated sets.
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS + STICKERS_UPDATE_TIMEOUT_MS as i64 + 10));
    rpc.enqueue_ok(
        tl_fns::messages::SearchEmojiStickerSets::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickerSets::NotModified,
    );
    let second = live
        .search_emoji_sticker_sets("star", true)
        .expect("not modified");
    assert!(second.not_modified);
    assert_eq!(second.set_ids, vec![StickerSetId(910)]);
    assert_eq!(second.hash, server_hash);

    let expected_second = tl_fns::messages::SearchEmojiStickerSets {
        exclude_featured: true,
        q: "star".to_owned(),
        hash: server_hash,
    };
    let recorded = rpc.recorded_of(tl_fns::messages::SearchEmojiStickerSets::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1], expected_second.to_bytes());
}

// =============================================================================
// 3. messages.getOldFeaturedStickers
// =============================================================================

#[test]
fn get_old_featured_stickers_pagination_exact_tl_bytes_and_not_modified() {
    let (_rt, rpc, live) = make_live_session();
    let cover1 = make_sticker_doc(9001, 19001, "🦊", 951, false);
    let set1 = make_raw_set(951, 9510, "Old Pack 1", "old_pack_1", 20, false, false);
    let cover2 = make_sticker_doc(9002, 19002, "🐻", 952, false);
    let set2 = make_raw_set(952, 9520, "Old Pack 2", "old_pack_2", 15, false, false);
    let page1_hash: i64 = 0x55AA_1234_5678_9ABC;

    rpc.enqueue_ok(
        tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: true,
                hash: page1_hash,
                count: 100,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: set1,
                        cover: cover1,
                    },
                )],
                unread: vec![951],
            },
        ),
    );

    let p1 = live
        .load_old_featured_stickers(0, 0) // limit <= 0 defaults to OLD_FEATURED_LOAD_LIMIT (40)
        .expect("page 1");
    assert_eq!(p1.set_ids, vec![StickerSetId(951)]);
    assert_eq!(p1.unread_ids, vec![StickerSetId(951)]);
    assert_eq!(p1.total_count, 100);
    assert_eq!(p1.next_offset, 1);
    assert!(p1.premium);
    assert_eq!(p1.hash, page1_hash);
    assert!(!p1.not_modified);
    assert!(live.sticker_document(ChatDocId(9001)).is_some());

    let expected_p1 = tl_fns::messages::GetOldFeaturedStickers {
        offset: 0,
        limit: OLD_FEATURED_LOAD_LIMIT,
        hash: 0,
    };
    let recorded = rpc.recorded_of(tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], expected_p1.to_bytes());

    // Page 2 at next_offset = 1.
    rpc.enqueue_ok(
        tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: false,
                hash: 0, // exercises tg_api::hash featured fallback
                count: 100,
                sets: vec![tl_enums::StickerSetCovered::StickerSetCovered(
                    tl_types::StickerSetCovered {
                        set: set2,
                        cover: cover2,
                    },
                )],
                unread: vec![],
            },
        ),
    );
    let p2 = live
        .load_old_featured_stickers(p1.next_offset, 40)
        .expect("page 2");
    assert_eq!(p2.set_ids, vec![StickerSetId(952)]);
    assert_eq!(p2.next_offset, 2);
    assert_ne!(p2.hash, 0);
    assert!(live.sticker_document(ChatDocId(9002)).is_some());

    // Expire TTL and verify `FeaturedStickersNotModified` updates total_count & preserves sets.
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS + STICKERS_UPDATE_TIMEOUT_MS as i64 + 5));
    rpc.enqueue_ok(
        tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::NotModified(
            tl_types::messages::FeaturedStickersNotModified { count: 104 },
        ),
    );
    let p1_nm = live
        .load_old_featured_stickers(0, OLD_FEATURED_LOAD_LIMIT)
        .expect("page 1 not modified");
    assert!(p1_nm.not_modified);
    assert_eq!(p1_nm.total_count, 104);
    assert_eq!(p1_nm.set_ids, vec![StickerSetId(951)]);
    let expected_p1_nm = tl_fns::messages::GetOldFeaturedStickers {
        offset: 0,
        limit: OLD_FEATURED_LOAD_LIMIT,
        hash: page1_hash,
    };
    let recorded = rpc.recorded_of(tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[2], expected_p1_nm.to_bytes());
}

// =============================================================================
// 4. messages.clearRecentStickers
// =============================================================================

#[test]
fn clear_recent_stickers_exact_tl_bytes_and_local_store_cleanup() {
    let (_rt, rpc, mut live) = make_live_session();

    // Seed cloud recent stickers via `messages.getRecentStickers`.
    let recent_doc = make_sticker_doc(9101, 19101, "👍", 960, false);
    rpc.enqueue_ok(
        tl_fns::messages::GetRecentStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::RecentStickers::RecentStickers(tl_types::messages::RecentStickers {
            hash: 42,
            packs: vec![],
            stickers: vec![recent_doc],
            dates: vec![1_700_000_001],
        }),
    );
    live.refresh_recent_stickers(false, true)
        .expect("seed recent");
    assert!(live.sticker_document(ChatDocId(9101)).is_some());

    // 1. Clear regular recent stickers (`attached = false`, `flags = 0`).
    rpc.enqueue_bool(tl_fns::messages::ClearRecentStickers::CONSTRUCTOR_ID, true);
    assert!(live.clear_recent_stickers(false).expect("clear recent"));

    // 2. Clear attached mask recent stickers (`attached = true`, `flags = 1`).
    rpc.enqueue_bool(tl_fns::messages::ClearRecentStickers::CONSTRUCTOR_ID, true);
    assert!(live.clear_recent_stickers(true).expect("clear attached"));

    // 3. Server returning `Bool::False` propagates false cleanly.
    rpc.enqueue_bool(tl_fns::messages::ClearRecentStickers::CONSTRUCTOR_ID, false);
    assert!(!live.clear_recent_stickers(false).expect("server declined"));

    let recorded = rpc.recorded_of(tl_fns::messages::ClearRecentStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 3);
    assert_eq!(
        recorded[0],
        (tl_fns::messages::ClearRecentStickers { attached: false }).to_bytes()
    );
    assert_eq!(
        recorded[1],
        (tl_fns::messages::ClearRecentStickers { attached: true }).to_bytes()
    );
    // Confirm `attached=false` clears bit 0 and `attached=true` sets bit 0 in flags word.
    assert_eq!(u32::from_le_bytes(recorded[0][4..8].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(recorded[1][4..8].try_into().unwrap()), 1);
}

// =============================================================================
// 5. messages.getAttachedStickers
// =============================================================================

#[test]
fn get_attached_stickers_photo_document_and_session_message_resolution() {
    let (rt, rpc, live) = make_live_session();

    // Seed a photo message and a document message in Session for `AttachedStickersMedia::Message`.
    let ch_peer = DataPeerId::from_channel(ChannelId(CHANNEL_ID));
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        let mut photo_msg = Message::new_text(
            ch_peer,
            DataMsgId(55),
            None,
            1_700_000_000,
            DataText::default(),
        );
        photo_msg.content = MessageContent::Regular {
            text: DataText::default(),
            media: MessageMedia::Photo(Box::new(PhotoData {
                id: PhotoId(3001),
                access_hash: 30_001,
                file_reference: vec![0xAA, 0xBB],
                date: 1_700_000_000,
                dc_id: 2,
                has_stickers: true,
                ..PhotoData::default()
            })),
        };
        s.insert_message(photo_msg);

        let mut doc_msg = Message::new_text(
            ch_peer,
            DataMsgId(56),
            None,
            1_700_000_000,
            DataText::default(),
        );
        doc_msg.content = MessageContent::Regular {
            text: DataText::default(),
            media: MessageMedia::Document(Box::new(DocumentData {
                id: DataDocId(4001),
                access_hash: 40_001,
                file_reference: vec![0xCC, 0xDD],
                date: 1_700_000_000,
                mime_type: "video/mp4".to_owned(),
                size: 10_000,
                dc_id: 2,
                file_name: "clip.mp4".to_owned(),
                ..DocumentData::default()
            })),
        };
        s.insert_message(doc_msg);

        let text_msg = Message::new_text(
            ch_peer,
            DataMsgId(57),
            None,
            1_700_000_000,
            DataText::default(),
        );
        s.insert_message(text_msg);
    });

    // 1. Photo message resolution + StickerSetMultiCovered hydration.
    let c1 = make_sticker_doc(9201, 19201, "🎭", 971, false);
    let c2 = make_sticker_doc(9202, 19202, "🎩", 971, false);
    let mask_set = make_raw_set(971, 9710, "Mask Pack", "mask_pack", 5, false, true);
    rpc.enqueue_ok(
        tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID,
        &vec![tl_enums::StickerSetCovered::StickerSetMultiCovered(
            tl_types::StickerSetMultiCovered {
                set: mask_set,
                covers: vec![c1, c2],
            },
        )],
    );

    let res_photo = live
        .get_attached_stickers(&AttachedStickersMedia::Message {
            peer_id: PeerId::from_channel(CHANNEL_ID),
            msg_id: MsgId(55),
        })
        .expect("photo attached stickers");
    assert_eq!(res_photo.len(), 1);
    assert_eq!(res_photo[0].set_id, StickerSetId(971));
    assert!(res_photo[0].is_masks);
    assert!(!res_photo[0].is_emoji);
    assert_eq!(
        res_photo[0].cover_doc_ids,
        vec![ChatDocId(9201), ChatDocId(9202)]
    );
    assert!(live.sticker_document(ChatDocId(9201)).is_some());
    assert!(live.sticker_document(ChatDocId(9202)).is_some());

    // 2. Document message resolution + StickerSetNoCovered.
    let no_cov_set = make_raw_set(972, 9720, "Emoji Overlay", "emoji_ov", 3, true, false);
    rpc.enqueue_ok(
        tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID,
        &vec![tl_enums::StickerSetCovered::StickerSetNoCovered(
            tl_types::StickerSetNoCovered { set: no_cov_set },
        )],
    );
    let res_doc = live
        .get_attached_stickers(&AttachedStickersMedia::Message {
            peer_id: PeerId::from_channel(CHANNEL_ID),
            msg_id: MsgId(56),
        })
        .expect("doc attached stickers");
    assert_eq!(res_doc.len(), 1);
    assert!(res_doc[0].is_emoji);
    assert!(res_doc[0].cover_doc_ids.is_empty());

    // 3. Explicit Photo & Document variants.
    rpc.enqueue_ok(
        tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID,
        &Vec::<tl_enums::StickerSetCovered>::new(),
    );
    let empty = live
        .get_attached_stickers(&AttachedStickersMedia::Document {
            id: 4002,
            access_hash: 40_002,
            file_reference: vec![9, 8],
        })
        .expect("explicit doc");
    assert!(empty.is_empty());

    // 4. Plain text message returns descriptive error (no RPC sent).
    assert!(
        live.get_attached_stickers(&AttachedStickersMedia::Message {
            peer_id: PeerId::from_channel(CHANNEL_ID),
            msg_id: MsgId(57),
        })
        .is_err()
    );

    let recorded = rpc.recorded_of(tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 3);
    assert_eq!(
        recorded[0],
        (tl_fns::messages::GetAttachedStickers {
            media: tl_enums::InputStickeredMedia::Photo(tl_types::InputStickeredMediaPhoto {
                id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                    id: 3001,
                    access_hash: 30_001,
                    file_reference: vec![0xAA, 0xBB],
                }),
            }),
        })
        .to_bytes()
    );
    assert_eq!(
        recorded[1],
        (tl_fns::messages::GetAttachedStickers {
            media: tl_enums::InputStickeredMedia::Document(tl_types::InputStickeredMediaDocument {
                id: tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                    id: 4001,
                    access_hash: 40_001,
                    file_reference: vec![0xCC, 0xDD],
                }),
            }),
        })
        .to_bytes()
    );
    assert_eq!(
        recorded[2],
        (tl_fns::messages::GetAttachedStickers {
            media: tl_enums::InputStickeredMedia::Document(tl_types::InputStickeredMediaDocument {
                id: tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                    id: 4002,
                    access_hash: 40_002,
                    file_reference: vec![9, 8],
                }),
            }),
        })
        .to_bytes()
    );
}

// =============================================================================
// 6. messages.getEmojiKeywordsLanguages
// =============================================================================

#[test]
fn get_emoji_keywords_languages_exact_tl_bytes_and_ttl_cache() {
    let (_rt, rpc, live) = make_live_session();
    rpc.enqueue_ok(
        tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID,
        &vec![
            tl_enums::EmojiLanguage::EmojiLanguage(tl_types::EmojiLanguage {
                lang_code: "en".to_owned(),
            }),
            tl_enums::EmojiLanguage::EmojiLanguage(tl_types::EmojiLanguage {
                lang_code: "fr".to_owned(),
            }),
        ],
    );

    let input_codes = vec!["en".to_owned(), "fr".to_owned()];
    let langs = live
        .emoji_keywords_languages(&input_codes)
        .expect("getEmojiKeywordsLanguages");
    assert_eq!(langs, vec!["en".to_owned(), "fr".to_owned()]);

    // Within 1-hour TTL -> cached hit, no second RPC.
    let cached = live
        .emoji_keywords_languages(&input_codes)
        .expect("cached langs");
    assert_eq!(cached, langs);
    let recorded = rpc.recorded_of(tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0],
        (tl_fns::messages::GetEmojiKeywordsLanguages {
            lang_codes: vec!["en".to_owned(), "fr".to_owned()],
        })
        .to_bytes()
    );

    // Past 1-hour TTL -> refetches from server.
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS + EMOJI_KEYWORDS_REFRESH_MS as i64 + 1));
    rpc.enqueue_ok(
        tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID,
        &vec![tl_enums::EmojiLanguage::EmojiLanguage(
            tl_types::EmojiLanguage {
                lang_code: "en".to_owned(),
            },
        )],
    );
    let refreshed = live
        .emoji_keywords_languages(&input_codes)
        .expect("refreshed langs");
    assert_eq!(refreshed, vec!["en".to_owned()]);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID)
            .len(),
        2
    );
}

// =============================================================================
// 7-10. All 4 account.getDefault*Emojis / getChannelRestrictedStatusEmojis
// =============================================================================

#[test]
fn default_emoji_catalogs_all_four_rpcs_exact_tl_bytes_not_modified_and_error_reset() {
    let (_rt, rpc, live) = make_live_session();

    let cases: [(DefaultEmojiCatalogKind, u32, i64, Vec<i64>); 4] = [
        (
            DefaultEmojiCatalogKind::ProfilePhoto,
            tl_fns::account::GetDefaultProfilePhotoEmojis::CONSTRUCTOR_ID,
            0x1010_2020_3030_4040,
            vec![5001, 5002],
        ),
        (
            DefaultEmojiCatalogKind::GroupPhoto,
            tl_fns::account::GetDefaultGroupPhotoEmojis::CONSTRUCTOR_ID,
            0x2020_3030_4040_5050,
            vec![5101, 5102, 5103],
        ),
        (
            DefaultEmojiCatalogKind::Background,
            tl_fns::account::GetDefaultBackgroundEmojis::CONSTRUCTOR_ID,
            0, // exercises fallback hash via tg_api::hash::count_document_vector_hash
            vec![5201, 5202],
        ),
        (
            DefaultEmojiCatalogKind::ChannelRestrictedStatus,
            tl_fns::account::GetChannelRestrictedStatusEmojis::CONSTRUCTOR_ID,
            0x4040_5050_6060_7070,
            vec![5301],
        ),
    ];

    for (kind, ctor, server_hash, ids) in cases {
        let expected_docs: Vec<ChatDocId> = ids.iter().map(|&id| ChatDocId(id as u64)).collect();
        let expected_hash = if server_hash != 0 {
            server_hash
        } else {
            let raw: Vec<u64> = ids.iter().map(|&id| id as u64).collect();
            count_document_vector_hash(&raw) as i64
        };

        // First fetch with hash = 0.
        rpc.enqueue_ok(
            ctor,
            &tl_enums::EmojiList::EmojiList(tl_types::EmojiList {
                hash: server_hash,
                document_id: ids.clone(),
            }),
        );
        let first = live
            .default_emoji_catalog(kind, false)
            .expect("initial catalog");
        assert_eq!(first.document_ids, expected_docs);
        assert_eq!(first.hash, expected_hash);
        assert!(!first.not_modified);

        // Within 1-hour TTL -> cache hit, no extra RPC.
        let cached = live
            .default_emoji_catalog(kind, false)
            .expect("cached catalog");
        assert_eq!(cached.document_ids, expected_docs);
        assert_eq!(rpc.recorded_of(ctor).len(), 1);

        // Forced revalidation -> sends `expected_hash`, handles `EmojiList::NotModified`.
        rpc.enqueue_ok(ctor, &tl_enums::EmojiList::NotModified);
        let nm = live
            .default_emoji_catalog(kind, true)
            .expect("not modified catalog");
        assert!(nm.not_modified);
        assert_eq!(nm.document_ids, expected_docs);
        assert_eq!(nm.hash, expected_hash);

        // Exact TL wire bytes check: [ctor_le, hash_le] (12 bytes total).
        let recorded = rpc.recorded_of(ctor);
        assert_eq!(recorded.len(), 2);
        let mut expected_bytes_0 = Vec::with_capacity(12);
        expected_bytes_0.extend_from_slice(&ctor.to_le_bytes());
        expected_bytes_0.extend_from_slice(&0i64.to_le_bytes());
        assert_eq!(recorded[0], expected_bytes_0);

        let mut expected_bytes_1 = Vec::with_capacity(12);
        expected_bytes_1.extend_from_slice(&ctor.to_le_bytes());
        expected_bytes_1.extend_from_slice(&expected_hash.to_le_bytes());
        assert_eq!(recorded[1], expected_bytes_1);

        // Error resets hash to 0; next fetch sends hash = 0.
        rpc.enqueue_rpc_err(ctor, 500, "INTERNAL");
        assert!(live.default_emoji_catalog(kind, true).is_err());
        rpc.enqueue_ok(ctor, &tl_enums::EmojiList::NotModified);
        let _ = live
            .default_emoji_catalog(kind, true)
            .expect("after err reset");
        let recorded = rpc.recorded_of(ctor);
        assert_eq!(recorded.len(), 4);
        assert_eq!(recorded[3], expected_bytes_0);
    }

    // Verify TTL constant matches 1 hour (`EMOJI_STATUSES_REFRESH_MS`).
    assert_eq!(EMOJI_STATUSES_REFRESH_MS, 3_600_000);
}

// =============================================================================
// 11. messages.getMessagesReactions
// =============================================================================

#[test]
fn get_messages_reactions_exact_tl_bytes_and_applies_updates_via_session_actor() {
    let (rt, rpc, live) = make_live_session();
    let ch_peer = DataPeerId::from_channel(ChannelId(CHANNEL_ID));
    let full_id = DataFullMsgId::new(ch_peer, DataMsgId(101));

    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        s.insert_message(Message::new_text(
            ch_peer,
            DataMsgId(101),
            None,
            1_700_000_000,
            DataText::default(),
        ));
    });

    // Empty slice is a no-op with 0 RPCs.
    live.refresh_messages_reactions(PeerId::from_channel(CHANNEL_ID), &[])
        .expect("empty noop");
    assert!(
        rpc.recorded_of(tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID)
            .is_empty()
    );

    // Enqueue `Updates` carrying `Update::MessageReactions`.
    let reactions_update = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::MessageReactions(
            tl_types::UpdateMessageReactions {
                peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: CHANNEL_ID as i64,
                }),
                msg_id: 101,
                top_msg_id: None,
                saved_peer_id: None,
                reactions: tl_enums::MessageReactions::MessageReactions(
                    tl_types::MessageReactions {
                        min: false,
                        can_see_list: true,
                        reactions_as_tags: false,
                        results: vec![tl_enums::ReactionCount::ReactionCount(
                            tl_types::ReactionCount {
                                chosen_order: Some(0),
                                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                    emoticon: "🔥".to_owned(),
                                }),
                                count: 7,
                            },
                        )],
                        recent_reactions: None,
                        top_reactors: None,
                    },
                ),
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_000,
        seq: 0,
    });
    rpc.enqueue_ok(
        tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID,
        &reactions_update,
    );

    live.refresh_messages_reactions(PeerId::from_channel(CHANNEL_ID), &[MsgId(101), MsgId(102)])
        .expect("refresh_messages_reactions");

    let recorded = rpc.recorded_of(tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0],
        (tl_fns::messages::GetMessagesReactions {
            peer: channel_input_peer(),
            id: vec![101, 102],
        })
        .to_bytes()
    );

    // Session state updated through SessionRuntime::apply_updates_blocking -> UpdatesHandler!
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let msg = s.message(full_id).expect("message present");
        let reactions = msg.reactions.as_ref().expect("reactions hydrated");
        assert_eq!(reactions.results.len(), 1);
        assert_eq!(reactions.results[0].id, ReactionId::Emoji("🔥".into()));
        assert_eq!(reactions.results[0].count, 7);
        assert!(reactions.results[0].is_chosen());
    });
}

// =============================================================================
// 12. messages.getDefaultTagReactions
// =============================================================================

#[test]
fn get_default_tag_reactions_exact_tl_bytes_not_modified_and_error_reset() {
    let (_rt, rpc, live) = make_live_session();
    let server_hash: i64 = 0x7777_8888_9999_0001;

    rpc.enqueue_ok(
        tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::Reactions(tl_types::messages::Reactions {
            hash: server_hash,
            reactions: vec![
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "❤️".to_owned(),
                }),
                tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                    document_id: 600_001,
                }),
                tl_enums::Reaction::Paid,
            ],
        }),
    );

    let first = live.default_tag_reactions(false).expect("first tag fetch");
    assert_eq!(
        first.reactions,
        vec![
            DefaultTagReaction::Emoji("❤️".to_owned()),
            DefaultTagReaction::CustomEmoji(ChatDocId(600_001)),
            DefaultTagReaction::Paid,
        ]
    );
    assert_eq!(first.hash, server_hash);
    assert!(!first.not_modified);

    // Inside 1-hour TTL -> cache hit.
    let cached = live.default_tag_reactions(false).expect("cached tags");
    assert_eq!(cached.reactions, first.reactions);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Past 1-hour TTL -> sends `server_hash`, handles `ReactionsNotModified`.
    live.set_chat_helpers_now_ms(Some(
        BASE_NOW_MS + REACTIONS_FULL_LIST_REFRESH_MS as i64 + 1,
    ));
    rpc.enqueue_ok(
        tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::NotModified,
    );
    let nm = live.default_tag_reactions(false).expect("not modified");
    assert!(nm.not_modified);
    assert_eq!(nm.reactions, first.reactions);
    assert_eq!(nm.hash, server_hash);

    // Error resets hash to 0 (`data_message_reactions.cpp:1094`).
    rpc.enqueue_rpc_err(
        tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID,
        503,
        "SERVICE_UNAVAILABLE",
    );
    assert!(live.default_tag_reactions(true).is_err());

    rpc.enqueue_ok(
        tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::NotModified,
    );
    let _ = live.default_tag_reactions(true).expect("after error");

    let recorded = rpc.recorded_of(tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 4);
    assert_eq!(
        recorded[0],
        (tl_fns::messages::GetDefaultTagReactions { hash: 0 }).to_bytes()
    );
    assert_eq!(
        recorded[1],
        (tl_fns::messages::GetDefaultTagReactions { hash: server_hash }).to_bytes()
    );
    assert_eq!(
        recorded[3],
        (tl_fns::messages::GetDefaultTagReactions { hash: 0 }).to_bytes()
    );
}

// =============================================================================
// 13 & 14. messages.getPaidReactionPrivacy + messages.togglePaidReactionPrivacy
// =============================================================================

#[test]
fn paid_reaction_privacy_get_toggle_and_live_update_push() {
    let (rt, rpc, mut live) = make_live_session();

    // 1. `messages.getPaidReactionPrivacy` returning `Anonymous`.
    let anon_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::PaidReactionPrivacy(
            tl_types::UpdatePaidReactionPrivacy {
                r#private: tl_enums::PaidReactionPrivacy::Anonymous,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_000,
        seq: 0,
    });
    rpc.enqueue_ok(
        tl_fns::messages::GetPaidReactionPrivacy::CONSTRUCTOR_ID,
        &anon_updates,
    );
    let p0 = live
        .paid_reaction_privacy(false)
        .expect("getPaidReactionPrivacy");
    assert_eq!(p0, PaidReactionPrivacy::Anonymous);

    // Subsequent `force = false` returns cached value with 0 extra RPCs.
    assert_eq!(
        live.paid_reaction_privacy(false).expect("cached"),
        PaidReactionPrivacy::Anonymous
    );
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetPaidReactionPrivacy::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Forced refetch returning `Peer(channel)`.
    let peer_updates = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::PaidReactionPrivacy(tl_types::UpdatePaidReactionPrivacy {
            r#private: tl_enums::PaidReactionPrivacy::Peer(tl_types::PaidReactionPrivacyPeer {
                peer: channel_input_peer(),
            }),
        }),
        date: 1_750_000_005,
    });
    rpc.enqueue_ok(
        tl_fns::messages::GetPaidReactionPrivacy::CONSTRUCTOR_ID,
        &peer_updates,
    );
    let p1 = live
        .paid_reaction_privacy(true)
        .expect("forced getPaidReactionPrivacy");
    assert_eq!(
        p1,
        PaidReactionPrivacy::Peer(PeerId::from_channel(CHANNEL_ID))
    );

    let recorded_get = rpc.recorded_of(tl_fns::messages::GetPaidReactionPrivacy::CONSTRUCTOR_ID);
    assert_eq!(recorded_get.len(), 2);
    assert_eq!(
        recorded_get[0],
        (tl_fns::messages::GetPaidReactionPrivacy {}).to_bytes()
    );

    // Live server push `Update::PaidReactionPrivacy(Default)` updates cached state asynchronously.
    rpc.push_update(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::PaidReactionPrivacy(tl_types::UpdatePaidReactionPrivacy {
            r#private: tl_enums::PaidReactionPrivacy::Default,
        }),
        date: 1_750_000_010,
    }));
    rt.block_on(async {
        for _ in 0..40 {
            if live.paid_reaction_privacy(false).ok() == Some(PaidReactionPrivacy::Default) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(
        live.paid_reaction_privacy(false)
            .expect("updated from push"),
        PaidReactionPrivacy::Default
    );

    // 2. `messages.togglePaidReactionPrivacy`: Anonymous, Peer(channel), Peer(self -> Default).
    rpc.enqueue_bool(
        tl_fns::messages::TogglePaidReactionPrivacy::CONSTRUCTOR_ID,
        true,
    );
    assert!(
        live.toggle_paid_reaction_privacy(
            PeerId::from_channel(CHANNEL_ID),
            MsgId(201),
            PaidReactionPrivacy::Anonymous,
        )
        .expect("toggle anonymous")
    );
    assert_eq!(
        live.cached_message_paid_reaction_privacy(PeerId::from_channel(CHANNEL_ID), MsgId(201)),
        Some(PaidReactionPrivacy::Anonymous)
    );

    rpc.enqueue_bool(
        tl_fns::messages::TogglePaidReactionPrivacy::CONSTRUCTOR_ID,
        true,
    );
    assert!(
        live.toggle_paid_reaction_privacy(
            PeerId::from_channel(CHANNEL_ID),
            MsgId(202),
            PaidReactionPrivacy::Peer(PeerId::from_channel(CHANNEL_ID)),
        )
        .expect("toggle peer")
    );

    // Self user peer normalizes to `paidReactionPrivacyDefault` (matching `PaidReactionShownPeerToTL`).
    rpc.enqueue_bool(
        tl_fns::messages::TogglePaidReactionPrivacy::CONSTRUCTOR_ID,
        true,
    );
    assert!(
        live.toggle_paid_reaction_privacy(
            PeerId::from_channel(CHANNEL_ID),
            MsgId(203),
            PaidReactionPrivacy::Peer(PeerId::from_user(SELF_UID)),
        )
        .expect("toggle self peer -> default")
    );

    let recorded_toggle =
        rpc.recorded_of(tl_fns::messages::TogglePaidReactionPrivacy::CONSTRUCTOR_ID);
    assert_eq!(recorded_toggle.len(), 3);
    assert_eq!(
        recorded_toggle[0],
        (tl_fns::messages::TogglePaidReactionPrivacy {
            peer: channel_input_peer(),
            msg_id: 201,
            r#private: tl_enums::PaidReactionPrivacy::Anonymous,
        })
        .to_bytes()
    );
    assert_eq!(
        recorded_toggle[1],
        (tl_fns::messages::TogglePaidReactionPrivacy {
            peer: channel_input_peer(),
            msg_id: 202,
            r#private: tl_enums::PaidReactionPrivacy::Peer(tl_types::PaidReactionPrivacyPeer {
                peer: channel_input_peer(),
            }),
        })
        .to_bytes()
    );
    assert_eq!(
        recorded_toggle[2],
        (tl_fns::messages::TogglePaidReactionPrivacy {
            peer: channel_input_peer(),
            msg_id: 203,
            r#private: tl_enums::PaidReactionPrivacy::Default,
        })
        .to_bytes()
    );
}

// =============================================================================
// 15, 16, 17. messages.reportReaction, deleteParticipantReaction, deleteParticipantReactions
// =============================================================================

#[test]
fn reaction_moderation_report_and_delete_participant_reactions_exact_tl_bytes() {
    let (rt, rpc, mut live) = make_live_session();
    let ch_peer = DataPeerId::from_channel(ChannelId(CHANNEL_ID));
    let full_id = DataFullMsgId::new(ch_peer, DataMsgId(301));

    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        s.insert_message(Message::new_text(
            ch_peer,
            DataMsgId(301),
            None,
            1_700_000_000,
            DataText::default(),
        ));
    });

    // 15. `messages.reportReaction`
    rpc.enqueue_bool(tl_fns::messages::ReportReaction::CONSTRUCTOR_ID, true);
    assert!(
        live.report_reaction(
            PeerId::from_channel(CHANNEL_ID),
            MsgId(301),
            PeerId::from_user(PARTICIPANT_UID),
        )
        .expect("reportReaction")
    );
    let recorded_report = rpc.recorded_of(tl_fns::messages::ReportReaction::CONSTRUCTOR_ID);
    assert_eq!(recorded_report.len(), 1);
    assert_eq!(
        recorded_report[0],
        (tl_fns::messages::ReportReaction {
            peer: channel_input_peer(),
            id: 301,
            reaction_peer: participant_input_peer(),
        })
        .to_bytes()
    );

    // 16. `messages.deleteParticipantReaction` -> returns `Updates` applied via SessionRuntime.
    let updated_reactions = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::MessageReactions(
            tl_types::UpdateMessageReactions {
                peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: CHANNEL_ID as i64,
                }),
                msg_id: 301,
                top_msg_id: None,
                saved_peer_id: None,
                reactions: tl_enums::MessageReactions::MessageReactions(
                    tl_types::MessageReactions {
                        min: false,
                        can_see_list: true,
                        reactions_as_tags: false,
                        results: Vec::new(),
                        recent_reactions: None,
                        top_reactors: None,
                    },
                ),
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_020,
        seq: 0,
    });
    rpc.enqueue_ok(
        tl_fns::messages::DeleteParticipantReaction::CONSTRUCTOR_ID,
        &updated_reactions,
    );
    live.delete_participant_reaction(
        PeerId::from_channel(CHANNEL_ID),
        MsgId(301),
        PeerId::from_user(PARTICIPANT_UID),
    )
    .expect("deleteParticipantReaction");

    let recorded_del_one =
        rpc.recorded_of(tl_fns::messages::DeleteParticipantReaction::CONSTRUCTOR_ID);
    assert_eq!(recorded_del_one.len(), 1);
    assert_eq!(
        recorded_del_one[0],
        (tl_fns::messages::DeleteParticipantReaction {
            peer: channel_input_peer(),
            msg_id: 301,
            participant: participant_input_peer(),
        })
        .to_bytes()
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        let msg = s.message(full_id).expect("message present");
        let reactions = msg.reactions.as_ref().expect("reactions cleared by update");
        assert!(reactions.results.is_empty());
    });

    // 17. `messages.deleteParticipantReactions` -> returns `Bool`.
    rpc.enqueue_bool(
        tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID,
        true,
    );
    assert!(
        live.delete_participant_reactions(
            PeerId::from_channel(CHANNEL_ID),
            PeerId::from_user(PARTICIPANT_UID),
        )
        .expect("deleteParticipantReactions")
    );
    let recorded_del_all =
        rpc.recorded_of(tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID);
    assert_eq!(recorded_del_all.len(), 1);
    assert_eq!(
        recorded_del_all[0],
        (tl_fns::messages::DeleteParticipantReactions {
            peer: channel_input_peer(),
            participant: participant_input_peer(),
        })
        .to_bytes()
    );

    // Unknown participant fails before sending any RPC.
    assert!(
        live.delete_participant_reactions(
            PeerId::from_channel(CHANNEL_ID),
            PeerId::from_user(999_999)
        )
        .is_err()
    );
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID)
            .len(),
        1
    );
}
