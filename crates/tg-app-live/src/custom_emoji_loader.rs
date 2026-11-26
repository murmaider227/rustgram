//! Loads the custom emoji documents that loaded messages show: the sender's emoji status on the
//! name line and custom emoji inside message text (TGB-2964).
//!
//! Port of the loading half of `Data::CustomEmojiManager` (`data/stickers/data_custom_emoji.cpp`):
//! unknown ids are queued, sent as `messages.getCustomEmojiDocuments` in batches of at most
//! [`MAX_PER_REQUEST`] (`kMaxPerRequest`), and each document's file is downloaded once and handed
//! to the shared renderer registry ([`tg_ui::custom_emoji`]), which paints it animated.
//!
//! Internal design (differs from tdesktop): instead of one `CustomEmoji` loader object per painted
//! instance, a slice of messages hands its ids to one per-session queue in a single call. The ids
//! go through a hash set (already registered or already queued ids cost one lookup), one tokio
//! task drains the queue, and downloading, decoding (gzip + Lottie parse, WebM frames, WebP) and
//! registration all run off the UI thread. The raw file is kept in the media cache under
//! `custom_emoji:<id>` with a one-byte header, so the next run decodes it without the network.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::document_from_tl;
use tg_app_core::history::backend::HistoryMessage;
use tg_data::Session;
use tg_text::EntityType;
use tg_tl::{enums as tl_enums, functions as tl_fns};
use tg_ui::custom_emoji::{
    CustomEmojiAsset, has_registered_custom_emoji, register_custom_emoji_asset,
};
use tokio::sync::RwLock;
use tracing::debug;

use crate::ErasedRpcClient;
use crate::bridge::LiveBridgeState;
use crate::media::{document_to_location, download_file_bytes_async};
use crate::media_cache::{LiveMediaCache, MediaCacheTag};
use crate::session::LiveSession;

/// `kMaxPerRequest` in `data_custom_emoji.cpp`.
pub(crate) const MAX_PER_REQUEST: usize = 100;

const RPC_TIMEOUT: Duration = Duration::from_secs(30);

const KIND_TGS: u8 = 0;
const KIND_WEBM: u8 = 1;
const KIND_IMAGE: u8 = 2;
const TEXT_COLOR_BIT: u8 = 0b100;

#[derive(Default)]
struct LoaderState {
    /// Ids queued, in flight, or done in this session.
    seen: HashSet<u64>,
    pending: Vec<u64>,
    running: bool,
}

struct RegistryEntry {
    session_weak: Weak<RwLock<Session>>,
    state: Arc<Mutex<LoaderState>>,
}

fn registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state_for(live: &LiveSession) -> Arc<Mutex<LoaderState>> {
    let session_arc = live.runtime().session();
    let key = Arc::as_ptr(session_arc) as usize;
    let session_weak = Arc::downgrade(session_arc);
    let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
    map.retain(|_, entry| entry.session_weak.strong_count() > 0);
    if let Some(entry) = map.get(&key)
        && entry.session_weak.ptr_eq(&session_weak)
    {
        return Arc::clone(&entry.state);
    }
    let created = Arc::new(Mutex::new(LoaderState::default()));
    map.insert(
        key,
        RegistryEntry {
            session_weak,
            state: Arc::clone(&created),
        },
    );
    created
}

/// Custom emoji document ids `messages` paint: emoji statuses on name lines and custom emoji
/// entities in the text.
pub(crate) fn wanted_ids(messages: &[HistoryMessage]) -> Vec<u64> {
    let mut out = Vec::new();
    for msg in messages {
        if let Some(id) = msg.extra.sender_status.and_then(|s| s.emoji_status_id) {
            out.push(id);
        }
        for entity in &msg.text.entities {
            if entity.entity_type() == EntityType::CustomEmoji
                && let Ok(id) = entity.data().parse::<u64>()
            {
                out.push(id);
            }
        }
    }
    out
}

/// Queues the custom emoji of `messages` that are not painted yet and starts the loader task if
/// it is idle. Never blocks: called while building a history slice or live update batch.
pub(crate) fn request_for_messages(live: &LiveSession, messages: &[HistoryMessage]) {
    let ids = wanted_ids(messages);
    if !ids.is_empty() {
        request(live, &ids);
    }
}

/// Queues `ids` (see [`request_for_messages`]).
pub(crate) fn request(live: &LiveSession, ids: &[u64]) {
    let state = state_for(live);
    let start = {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        for &id in ids {
            if id != 0 && !st.seen.contains(&id) && !has_registered_custom_emoji(&id.to_string()) {
                st.seen.insert(id);
                st.pending.push(id);
            }
        }
        let start = !st.pending.is_empty() && !st.running;
        if start {
            st.running = true;
        }
        start
    };
    if !start {
        return;
    }
    let job = LoaderJob {
        rpc: live.runtime().rpc().clone(),
        media_cache: live.media_cache().clone(),
        bridge: Arc::clone(live.bridge_state()),
        state,
    };
    live.runtime().tokio_handle().spawn(job.run());
}

struct LoaderJob {
    rpc: ErasedRpcClient,
    media_cache: LiveMediaCache,
    bridge: Arc<LiveBridgeState>,
    state: Arc<Mutex<LoaderState>>,
}

impl LoaderJob {
    async fn run(self) {
        loop {
            let batch: Vec<u64> = {
                let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if st.pending.is_empty() {
                    st.running = false;
                    return;
                }
                let take = st.pending.len().min(MAX_PER_REQUEST);
                st.pending.drain(..take).collect()
            };
            let registered = self.load_batch(&batch).await;
            if registered > 0 {
                self.bridge.notify_ui();
            }
        }
    }

    /// Loads one batch; returns how many emoji were registered.
    async fn load_batch(&self, batch: &[u64]) -> usize {
        let mut registered = 0;
        let mut missing = Vec::with_capacity(batch.len());
        for &id in batch {
            match self.media_cache.get_blob(&cache_key(id)) {
                Some(blob) if register_blob(id, &blob).await => registered += 1,
                _ => missing.push(id),
            }
        }
        if missing.is_empty() {
            return registered;
        }
        let req = tl_fns::messages::GetCustomEmojiDocuments {
            document_id: missing.iter().map(|&id| id as i64).collect(),
        };
        let docs = match tokio::time::timeout(RPC_TIMEOUT, self.rpc.invoke(&req)).await {
            Ok(Ok(docs)) => docs,
            Ok(Err(err)) => {
                debug!(count = missing.len(), error = %err, "getCustomEmojiDocuments failed");
                self.forget(&missing);
                return registered;
            }
            Err(_) => {
                self.forget(&missing);
                return registered;
            }
        };
        for doc in &docs {
            if let Some(id) = self.load_document(doc).await {
                registered += 1;
                missing.retain(|&m| m != id);
            }
        }
        // Ids the server did not return or whose file failed may be asked again later.
        self.forget(&missing);
        registered
    }

    async fn load_document(&self, doc: &tl_enums::Document) -> Option<u64> {
        let tl_enums::Document::Document(raw) = doc else {
            return None;
        };
        let text_color = raw
            .attributes
            .iter()
            .any(|a| matches!(a, tl_enums::DocumentAttribute::CustomEmoji(c) if c.text_color));
        let data = document_from_tl(doc)?;
        let kind = match data.mime_type.as_str() {
            "application/x-tgsticker" => KIND_TGS,
            "video/webm" => KIND_WEBM,
            _ => KIND_IMAGE,
        };
        let location = document_to_location(&data);
        let size = (location.size > 0).then_some(location.size);
        let bytes = match tokio::time::timeout(
            RPC_TIMEOUT,
            download_file_bytes_async(&self.rpc, location.dc_id, location.location, size),
        )
        .await
        {
            Ok(Ok(bytes)) if !bytes.is_empty() => bytes,
            Ok(Err(err)) => {
                debug!(doc = data.id.0, error = %err, "custom emoji download failed");
                return None;
            }
            _ => return None,
        };
        let header = kind | if text_color { TEXT_COLOR_BIT } else { 0 };
        let mut blob = Vec::with_capacity(bytes.len() + 1);
        blob.push(header);
        blob.extend_from_slice(&bytes);
        let _ = self
            .media_cache
            .put_blob(&cache_key(data.id.0), MediaCacheTag::Sticker, &blob);
        register_blob(data.id.0, &blob).await.then_some(data.id.0)
    }

    fn forget(&self, ids: &[u64]) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for id in ids {
            st.seen.remove(id);
        }
    }
}

fn cache_key(id: u64) -> String {
    format!("custom_emoji:{id}")
}

/// Decodes a cached blob (header byte + file) on a blocking worker and registers it.
async fn register_blob(id: u64, blob: &[u8]) -> bool {
    let blob = blob.to_vec();
    tokio::task::spawn_blocking(move || {
        asset_from_blob(&blob)
            .is_some_and(|asset| register_custom_emoji_asset(id.to_string(), &asset))
    })
    .await
    .unwrap_or(false)
}

/// The renderer asset for a cached blob: `.tgs`, WebM, or a still image (WebP / PNG).
pub(crate) fn asset_from_blob(blob: &[u8]) -> Option<CustomEmojiAsset> {
    let (&header, bytes) = blob.split_first()?;
    let text_color_tinted = header & TEXT_COLOR_BIT != 0;
    match header & 0b11 {
        KIND_TGS => Some(CustomEmojiAsset::TgsBytes {
            bytes: bytes.to_vec(),
            text_color_tinted,
        }),
        KIND_WEBM => Some(CustomEmojiAsset::WebmBytes {
            bytes: bytes.to_vec(),
            text_color_tinted,
        }),
        KIND_IMAGE => {
            let image = image::load_from_memory(bytes).ok()?.to_rgba8();
            let (width, height) = image.dimensions();
            Some(CustomEmojiAsset::StaticRgba {
                width,
                height,
                rgba: image.into_raw(),
                text_color_tinted,
            })
        }
        _ => None,
    }
}
