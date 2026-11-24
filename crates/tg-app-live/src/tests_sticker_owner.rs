//! `FakeRpcClient` tests for sticker pack owner actions (`stickers.renameStickerSet`,
//! `stickers.changeStickerPosition`, `stickers.removeStickerFromSet`,
//! `stickers.deleteStickerSet`) and local cache refresh.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_chat::StickerSetId;
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::session::LiveSession;
use tg_app_core::chat_helpers::backend::ChatHelpersBackend;
use tg_app_core::session_api::AppSession;

type RecordedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;
type QueuedResponses = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedRequests,
    responses: QueuedResponses,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
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

    fn recorded_sticker_requests(&self) -> Vec<(u32, Vec<u8>)> {
        let sticker_ctors = [
            tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
            tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID,
            tl_fns::stickers::ChangeStickerPosition::CONSTRUCTOR_ID,
            tl_fns::stickers::RemoveStickerFromSet::CONSTRUCTOR_ID,
            tl_fns::stickers::DeleteStickerSet::CONSTRUCTOR_ID,
        ];
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| sticker_ctors.contains(c))
            .cloned()
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
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
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
            Some(Ok(raw)) => {
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
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

fn make_live_session() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    let runtime = SessionRuntime::start_on(
        DataUserId(100),
        crate::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);
    live.set_chat_helpers_now_ms(Some(1_750_000_000_000));
    (rt, rpc, live)
}

fn make_sticker_doc(
    id: i64,
    access_hash: i64,
    file_ref: &[u8],
    set_id: i64,
    set_access_hash: i64,
    alt: &str,
) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash,
        file_reference: file_ref.to_vec(),
        date: 1_700_000_000,
        mime_type: "image/webp".to_owned(),
        size: 12_345,
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
                    access_hash: set_access_hash,
                }),
                mask_coords: None,
            }),
        ],
    })
}

fn make_full_sticker_set(
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
            hash: 12345,
        }),
        packs,
        keywords: Vec::new(),
        documents,
    })
}

#[test]
fn sticker_pack_owner_actions_send_exact_tl_rpcs_and_refresh_store() {
    let (_rt, rpc, mut live) = make_live_session();
    let set_id: i64 = 77_001;
    let set_ah: i64 = 99_001;
    let short_name = "OwnerPack";

    // Seed local sticker store via `resolve_sticker_set` (`messages.getStickerSet`).
    let initial = make_full_sticker_set(
        set_id,
        set_ah,
        "Initial Pack",
        short_name,
        &[(501, 1501, b"ref1", "😀"), (502, 1502, b"ref2", "🔥")],
    );
    rpc.enqueue_ok(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID, &initial);
    let preview0 = live
        .resolve_sticker_set(short_name, false)
        .expect("resolve_sticker_set");
    assert_eq!(preview0.title, "Initial Pack");
    assert_eq!(preview0.items.len(), 2);

    // 1. `stickers.renameStickerSet`
    let renamed = make_full_sticker_set(
        set_id,
        set_ah,
        "Renamed Pack",
        short_name,
        &[(501, 1501, b"ref1", "😀"), (502, 1502, b"ref2", "🔥")],
    );
    rpc.enqueue_ok(tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID, &renamed);
    let preview1 = live
        .rename_sticker_set_preview(set_id as u64, short_name, "  Renamed Pack  ")
        .expect("renameStickerSet")
        .expect("refreshed preview");
    assert_eq!(preview1.title, "Renamed Pack");
    assert_eq!(preview1.set_id, set_id as u64);

    // 2. `stickers.changeStickerPosition`
    let reordered = make_full_sticker_set(
        set_id,
        set_ah,
        "Renamed Pack",
        short_name,
        &[(502, 1502, b"ref2", "🔥"), (501, 1501, b"ref1", "😀")],
    );
    rpc.enqueue_ok(
        tl_fns::stickers::ChangeStickerPosition::CONSTRUCTOR_ID,
        &reordered,
    );
    let preview2 = live
        .change_sticker_position_preview(set_id as u64, short_name, 502, 0)
        .expect("changeStickerPosition")
        .expect("refreshed preview");
    assert_eq!(
        preview2
            .items
            .iter()
            .map(|i| i.document_id)
            .collect::<Vec<_>>(),
        vec![502, 501]
    );

    // 3. `stickers.removeStickerFromSet`
    let after_remove = make_full_sticker_set(
        set_id,
        set_ah,
        "Renamed Pack",
        short_name,
        &[(501, 1501, b"ref1", "😀")],
    );
    rpc.enqueue_ok(
        tl_fns::stickers::RemoveStickerFromSet::CONSTRUCTOR_ID,
        &after_remove,
    );
    let preview3 = live
        .remove_sticker_from_set_preview(set_id as u64, short_name, 502)
        .expect("removeStickerFromSet")
        .expect("refreshed preview");
    assert_eq!(preview3.count, 1);
    assert_eq!(
        preview3
            .items
            .iter()
            .map(|i| i.document_id)
            .collect::<Vec<_>>(),
        vec![501]
    );

    // 4. `stickers.deleteStickerSet`
    rpc.enqueue_ok(tl_fns::stickers::DeleteStickerSet::CONSTRUCTOR_ID, &true);
    live.delete_sticker_set_preview(set_id as u64, short_name)
        .expect("deleteStickerSet");
    assert!(
        !live.is_set_installed(StickerSetId(set_id as u64)),
        "deleted sticker set must no longer be installed"
    );
    assert!(
        live.resolve_input_sticker_set_cached(set_id as u64, short_name)
            .is_none(),
        "deleted sticker set must be evicted from StickersStore"
    );

    // Verify exact TL requests recorded on the wire.
    let reqs = rpc.recorded_sticker_requests();
    assert_eq!(reqs.len(), 5);
    assert_eq!(reqs[0].0, tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID);

    assert_eq!(
        reqs[1].0,
        tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID
    );
    let rename_req = tl_fns::stickers::RenameStickerSet::from_bytes(&reqs[1].1)
        .expect("decode RenameStickerSet");
    assert_eq!(rename_req.title, "Renamed Pack");
    assert_eq!(
        rename_req.stickerset,
        tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: set_id,
            access_hash: set_ah,
        })
    );

    assert_eq!(
        reqs[2].0,
        tl_fns::stickers::ChangeStickerPosition::CONSTRUCTOR_ID
    );
    let reorder_req = tl_fns::stickers::ChangeStickerPosition::from_bytes(&reqs[2].1)
        .expect("decode ChangeStickerPosition");
    assert_eq!(reorder_req.position, 0);
    assert_eq!(
        reorder_req.sticker,
        tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
            id: 502,
            access_hash: 1502,
            file_reference: b"ref2".to_vec(),
        })
    );

    assert_eq!(
        reqs[3].0,
        tl_fns::stickers::RemoveStickerFromSet::CONSTRUCTOR_ID
    );
    let remove_req = tl_fns::stickers::RemoveStickerFromSet::from_bytes(&reqs[3].1)
        .expect("decode RemoveStickerFromSet");
    assert_eq!(
        remove_req.sticker,
        tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
            id: 502,
            access_hash: 1502,
            file_reference: b"ref2".to_vec(),
        })
    );

    assert_eq!(
        reqs[4].0,
        tl_fns::stickers::DeleteStickerSet::CONSTRUCTOR_ID
    );
    let delete_req = tl_fns::stickers::DeleteStickerSet::from_bytes(&reqs[4].1)
        .expect("decode DeleteStickerSet");
    assert_eq!(
        delete_req.stickerset,
        tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: set_id,
            access_hash: set_ah,
        })
    );
}

#[test]
fn sticker_pack_owner_not_modified_reply_falls_back_to_get_sticker_set_refresh() {
    let (_rt, rpc, mut live) = make_live_session();
    let set_id: i64 = 77_002;
    let set_ah: i64 = 99_002;
    let short_name = "RefreshPack";

    rpc.enqueue_ok(
        tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::NotModified,
    );
    let refreshed = make_full_sticker_set(
        set_id,
        set_ah,
        "Refreshed Name",
        short_name,
        &[(601, 1601, b"r601", "✨")],
    );
    rpc.enqueue_ok(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID, &refreshed);

    let preview = live
        .rename_sticker_set_preview(0, short_name, "Refreshed Name")
        .expect("renameStickerSet with NotModified fallback")
        .expect("preview from GetStickerSet refresh");
    assert_eq!(preview.title, "Refreshed Name");
    assert_eq!(preview.set_id, set_id as u64);

    let reqs = rpc.recorded_sticker_requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].0,
        tl_fns::stickers::RenameStickerSet::CONSTRUCTOR_ID
    );
    let rename_req = tl_fns::stickers::RenameStickerSet::from_bytes(&reqs[0].1)
        .expect("decode RenameStickerSet");
    assert_eq!(
        rename_req.stickerset,
        tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
            short_name: short_name.to_owned(),
        })
    );
    assert_eq!(reqs[1].0, tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID);
}
