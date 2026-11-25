//! `FakeRpcClient` conformance tests for `tg_api::StickersSyncManager` pack owner actions:
//! - `stickers.renameStickerSet` (`boxes/sticker_set_box.cpp:327-363`)
//! - `stickers.changeStickerPosition` (`boxes/sticker_set_box.cpp:1585-1613`)
//! - `stickers.removeStickerFromSet` (`boxes/sticker_set_box.cpp:1939-1957`)
//! - `stickers.deleteStickerSet` (`api/api_stickers_creator.cpp:364-382`)

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{
    ApiError, DcId, InvocationError, MAX_STICKER_SET_TITLE_LEN, RpcClient, StickerSetSpec,
    StickersSyncManager,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};

type RecordedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;
type ResponseQueue = Arc<Mutex<VecDeque<(u32, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct MockRpc {
    recorded: RecordedRequests,
    responses: ResponseQueue,
}

impl MockRpc {
    fn enqueue<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, resp.to_bytes()));
    }

    fn recorded(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for MockRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let popped = self.responses.lock().expect("lock").pop_front();
        match popped {
            Some((expected, raw)) => {
                assert_eq!(expected, ctor);
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Dropped),
        }
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}
}

fn make_sticker_doc(
    id: i64,
    access_hash: i64,
    file_ref: &[u8],
    set_id: i64,
    set_ah: i64,
    alt: &str,
) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash,
        file_reference: file_ref.to_vec(),
        date: 1_700_000_000,
        mime_type: "image/webp".to_owned(),
        size: 4_096,
        thumbs: None,
        video_thumbs: None,
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
                    access_hash: set_ah,
                }),
                mask_coords: None,
            }),
        ],
    })
}

fn make_tl_sticker_set(
    set_id: i64,
    access_hash: i64,
    title: &str,
    short_name: &str,
    docs: &[(i64, i64, &[u8], &str)],
) -> tl_enums::messages::StickerSet {
    let documents: Vec<tl_enums::Document> = docs
        .iter()
        .map(|&(id, ah, fref, alt)| make_sticker_doc(id, ah, fref, set_id, access_hash, alt))
        .collect();
    let packs: Vec<tl_enums::StickerPack> = docs
        .iter()
        .map(|&(id, _, _, alt)| {
            tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
                emoticon: alt.to_owned(),
                documents: vec![id],
            })
        })
        .collect();
    tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
        set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
            archived: false,
            official: false,
            masks: false,
            emojis: false,
            text_color: false,
            channel_emoji_status: false,
            creator: true,
            installed_date: Some(1_700_000_000),
            id: set_id,
            access_hash,
            title: title.to_owned(),
            short_name: short_name.to_owned(),
            thumbs: None,
            thumb_dc_id: None,
            thumb_version: None,
            thumb_document_id: docs.first().map(|d| d.0),
            count: docs.len() as i32,
            hash: 99_999,
        }),
        packs,
        keywords: Vec::new(),
        documents,
    })
}

#[tokio::test]
async fn sticker_owner_rpcs_send_exact_wire_requests_and_update_sync_manager() {
    let rpc = MockRpc::default();
    let mut mgr = StickersSyncManager::new();
    let set_id: u64 = 88_001;
    let set_ah: u64 = 44_001;
    let spec = StickerSetSpec::Id {
        id: set_id,
        access_hash: set_ah,
    };
    let short_name = "test_owner_pack";

    // Seed initial pack via `get_sticker_set`.
    let initial = make_tl_sticker_set(
        set_id as i64,
        set_ah as i64,
        "Original Title",
        short_name,
        &[(101, 1101, b"ref_a", "😀"), (102, 1102, b"ref_b", "🔥")],
    );
    rpc.enqueue(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID, &initial);
    let loaded = mgr
        .get_sticker_set(&rpc, &spec, Some(0), 1_700_000_000_000)
        .await
        .expect("get_sticker_set");
    assert_eq!(loaded, Some(set_id));
    mgr.sets_order.push(set_id);

    // Client-side validation rejects empty/whitespace-only and >50 char titles with 0 RPCs.
    assert!(matches!(
        mgr.rename_sticker_set(&rpc, &spec, "   ", 1_700_000_001_000)
            .await,
        Err(ApiError::InvalidState(_))
    ));
    let too_long = "a".repeat(MAX_STICKER_SET_TITLE_LEN + 1);
    assert!(matches!(
        mgr.rename_sticker_set(&rpc, &spec, &too_long, 1_700_000_001_000)
            .await,
        Err(ApiError::InvalidState(_))
    ));
    assert_eq!(rpc.recorded().len(), 1);

    // 1. `stickers.renameStickerSet` trims title and feeds updated set.
    let renamed = make_tl_sticker_set(
        set_id as i64,
        set_ah as i64,
        "Renamed Title",
        short_name,
        &[(101, 1101, b"ref_a", "😀"), (102, 1102, b"ref_b", "🔥")],
    );
    rpc.enqueue(tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID, &renamed);
    let renamed_id = mgr
        .rename_sticker_set(&rpc, &spec, "  Renamed Title  ", 1_700_000_002_000)
        .await
        .expect("rename_sticker_set");
    assert_eq!(renamed_id, Some(set_id));
    assert_eq!(mgr.sets[&set_id].title, "Renamed Title");

    // 2. `stickers.changeStickerPosition` rejects negative position, then reorders on valid index.
    let doc_102 = tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
        id: 102,
        access_hash: 1102,
        file_reference: b"ref_b".to_vec(),
    });
    assert!(matches!(
        mgr.change_sticker_position(&rpc, doc_102.clone(), -1, 1_700_000_002_000)
            .await,
        Err(ApiError::InvalidState(_))
    ));
    let reordered = make_tl_sticker_set(
        set_id as i64,
        set_ah as i64,
        "Renamed Title",
        short_name,
        &[(102, 1102, b"ref_b", "🔥"), (101, 1101, b"ref_a", "😀")],
    );
    rpc.enqueue(
        tl_fns::stickers::ChangeStickerPosition::CONSTRUCTOR_ID,
        &reordered,
    );
    let reordered_id = mgr
        .change_sticker_position(&rpc, doc_102.clone(), 0, 1_700_000_003_000)
        .await
        .expect("change_sticker_position");
    assert_eq!(reordered_id, Some(set_id));
    assert_eq!(mgr.sets[&set_id].stickers, vec![102, 101]);

    // 3. `stickers.removeStickerFromSet` removes sticker 102 and updates cached count/stickers.
    let after_remove = make_tl_sticker_set(
        set_id as i64,
        set_ah as i64,
        "Renamed Title",
        short_name,
        &[(101, 1101, b"ref_a", "😀")],
    );
    rpc.enqueue(
        tl_fns::stickers::RemoveStickerFromSet::CONSTRUCTOR_ID,
        &after_remove,
    );
    let removed_id = mgr
        .remove_sticker_from_set(&rpc, doc_102.clone(), 1_700_000_004_000)
        .await
        .expect("remove_sticker_from_set");
    assert_eq!(removed_id, Some(set_id));
    assert_eq!(mgr.sets[&set_id].stickers, vec![101]);
    assert_eq!(mgr.sets[&set_id].count, 1);

    // 4. `stickers.deleteStickerSet` deletes and evicts from sets, orders, and sets_by_spec.
    rpc.enqueue(tl_fns::stickers::DeleteStickerSet::CONSTRUCTOR_ID, &true);
    let deleted = mgr
        .delete_sticker_set(&rpc, set_id, None)
        .await
        .expect("delete_sticker_set");
    assert!(deleted);
    assert!(!mgr.sets.contains_key(&set_id));
    assert!(!mgr.sets_order.contains(&set_id));
    assert!(!mgr.sets_by_spec.values().any(|&id| id == set_id));

    // Verify exact TL wire encodings for all 4 owner RPCs.
    let reqs = rpc.recorded();
    assert_eq!(reqs.len(), 5);

    let rename_req =
        tl_fns::stickers::RenameStickerSet::from_bytes(&reqs[1].1).expect("decode rename");
    assert_eq!(rename_req.title, "Renamed Title");
    assert_eq!(
        rename_req.stickerset,
        tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: set_id as i64,
            access_hash: set_ah as i64,
        })
    );

    let reorder_req =
        tl_fns::stickers::ChangeStickerPosition::from_bytes(&reqs[2].1).expect("decode reorder");
    assert_eq!(reorder_req.sticker, doc_102);
    assert_eq!(reorder_req.position, 0);

    let remove_req =
        tl_fns::stickers::RemoveStickerFromSet::from_bytes(&reqs[3].1).expect("decode remove");
    assert_eq!(remove_req.sticker, doc_102);

    let delete_req =
        tl_fns::stickers::DeleteStickerSet::from_bytes(&reqs[4].1).expect("decode delete");
    assert_eq!(
        delete_req.stickerset,
        tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: set_id as i64,
            access_hash: set_ah as i64,
        })
    );
}

#[tokio::test]
async fn rename_sticker_set_not_modified_falls_back_to_get_sticker_set() {
    let rpc = MockRpc::default();
    let mut mgr = StickersSyncManager::new();
    let spec = StickerSetSpec::ShortName("fallback_pack".to_owned());

    rpc.enqueue(
        tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::NotModified,
    );
    let refreshed = make_tl_sticker_set(
        88_002,
        44_002,
        "Refreshed Title",
        "fallback_pack",
        &[(201, 1201, b"r201", "✨")],
    );
    rpc.enqueue(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID, &refreshed);

    let id = mgr
        .rename_sticker_set(&rpc, &spec, "Refreshed Title", 1_700_000_005_000)
        .await
        .expect("rename with NotModified fallback");
    assert_eq!(id, Some(88_002));
    assert_eq!(mgr.sets[&88_002].title, "Refreshed Title");

    let reqs = rpc.recorded();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].0,
        tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID
    );
    assert_eq!(reqs[1].0, tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID);
    let get_req =
        tl_fns::messages::GetStickerSet::from_bytes(&reqs[1].1).expect("decode getStickerSet");
    assert_eq!(get_req.hash, 0);
}
