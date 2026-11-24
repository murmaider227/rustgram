//! Server interactive emoji animation sticker pack (`EmojiStickersPack`),
//! fetched via `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/chat_helpers/stickers_emoji_pack.h`
//! - `Telegram/SourceFiles/chat_helpers/stickers_emoji_pack.cpp`
//! - `Telegram/SourceFiles/chat_helpers/emoji_interactions.cpp`
//! - `Telegram/SourceFiles/data/stickers/data_stickers.cpp`

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_data::{DocumentData, DocumentId, DocumentKind, StickerMeta};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::media::{
    block_on_runtime, document_to_location, download_file_bytes,
    download_file_bytes_with_progress_async, with_rpc_timeout,
};
use crate::media_cache::{LiveMediaCache, decompress_tgs_to_lottie_json, document_cache_key};
use crate::session::LiveSession;

/// Default RPC timeout for fetching the animated emoji animations sticker set.
const EMOJI_PACK_RPC_TIMEOUT: Duration = Duration::from_secs(15);

pub use tg_app_core::core_types::emoji_interaction::{
    index_from_emoticon, normalize_interaction_emoticon,
};

/// One animation variant entry inside [`EmojiStickersPack`].
#[derive(Debug, Clone, PartialEq)]
pub struct EmojiAnimationEntry {
    /// Server sticker document metadata (used to download `.tgs` via `upload.getFile`).
    pub document: DocumentData,
    /// Decompressed UTF-8 Lottie JSON once downloaded or loaded from cache.
    pub lottie_json: Option<String>,
}

/// Server interactive emoji animation pack (`Stickers::EmojiPack` / `EmojiStickersPack`).
///
/// Populated from `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)`.
#[derive(Debug, Clone, Default)]
pub struct EmojiStickersPack {
    /// Canonical emoticon -> ordered map of `variant_index (0..=9) -> animation entry`.
    animations: HashMap<String, BTreeMap<usize, EmojiAnimationEntry>>,
    loaded_from_server: bool,
    request_in_flight: bool,
    in_flight_docs: HashSet<u64>,
    animations_version: u64,
}

impl EmojiStickersPack {
    /// Creates an empty [`EmojiStickersPack`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` once `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)`
    /// has been applied.
    #[must_use]
    pub const fn is_loaded_from_server(&self) -> bool {
        self.loaded_from_server
    }

    /// Monotonically incremented counter bumped each time `apply_animations_set` runs.
    #[must_use]
    pub const fn animations_version(&self) -> u64 {
        self.animations_version
    }

    /// Returns the total number of distinct canonical emojis with server animations.
    #[must_use]
    pub fn emoji_count(&self) -> usize {
        self.animations.len()
    }

    /// Applies a `messages.stickerSet` response returned for
    /// `inputStickerSetAnimatedEmojiAnimations`
    /// (`EmojiPack::applyAnimationsSet` in `stickers_emoji_pack.cpp:439-487`).
    ///
    /// Returns the total number of `(emoji, variant_index)` animation entries mapped.
    pub fn apply_animations_set(&mut self, data: &tl_types::messages::StickerSet) -> usize {
        let mut stickers_by_id: HashMap<i64, DocumentData> = HashMap::new();
        for tl_doc in &data.documents {
            let Some(doc) = tl_document_to_data(tl_doc) else {
                continue;
            };
            if doc.sticker.is_none()
                && !matches!(
                    doc.kind,
                    DocumentKind::Sticker
                        | DocumentKind::AnimatedSticker
                        | DocumentKind::VideoSticker
                )
            {
                continue;
            }
            stickers_by_id.insert(doc.id.0 as i64, doc);
        }

        // Preserve any already-downloaded Lottie JSON across pack refreshes.
        let mut existing_json: HashMap<u64, String> = HashMap::new();
        for variants in self.animations.values() {
            for entry in variants.values() {
                if let Some(ref json) = entry.lottie_json {
                    existing_json.insert(entry.document.id.0, json.clone());
                }
            }
        }

        let indices = collect_animations_indices(&data.packs);
        self.animations.clear();
        let mut mapped = 0usize;

        for tl_pack in &data.packs {
            let tl_enums::StickerPack::StickerPack(pack) = tl_pack;
            if index_from_emoticon(&pack.emoticon).is_some() {
                continue;
            }
            let Some(canonical) = normalize_interaction_emoticon(&pack.emoticon) else {
                continue;
            };
            let variants = self.animations.entry(canonical).or_default();
            for &doc_id in &pack.documents {
                let Some(doc_indices) = indices.get(&doc_id) else {
                    continue;
                };
                let Some(doc) = stickers_by_id.get(&doc_id) else {
                    continue;
                };
                let cached_json = existing_json.get(&doc.id.0).cloned();
                for &idx in doc_indices {
                    variants.insert(
                        idx,
                        EmojiAnimationEntry {
                            document: doc.clone(),
                            lottie_json: cached_json.clone(),
                        },
                    );
                    mapped += 1;
                }
            }
        }

        self.animations.retain(|_, v| !v.is_empty());
        self.loaded_from_server = true;
        self.request_in_flight = false;
        self.animations_version = self.animations_version.wrapping_add(1);
        mapped
    }

    /// Resolves the canonical emoji key in this server pack for `emoticon`, if present
    /// (`EmojiPack::chooseInteractionEmoji` in `stickers_emoji_pack.cpp:226-253`).
    #[must_use]
    pub fn choose_interaction_emoji(&self, emoticon: &str) -> Option<String> {
        let trimmed = emoticon.trim();
        if trimmed.is_empty() {
            return None;
        }
        let stripped: String = trimmed.chars().filter(|&c| c != '\u{FE0F}').collect();
        if self.animations.contains_key(&stripped) {
            return Some(stripped);
        }
        let normalized = normalize_interaction_emoticon(trimmed)?;
        if self.animations.contains_key(&normalized) {
            return Some(normalized);
        }
        None
    }

    /// Returns the ordered map of animation variants for `emoticon`, if present
    /// (`EmojiPack::animationsForEmoji`).
    #[must_use]
    pub fn animations_for_emoji(
        &self,
        emoticon: &str,
    ) -> Option<&BTreeMap<usize, EmojiAnimationEntry>> {
        let key = self.choose_interaction_emoji(emoticon)?;
        self.animations.get(&key)
    }

    /// Returns `true` if the server pack contains at least one interaction animation for `emoticon`.
    #[must_use]
    pub fn has_animations_for(&self, emoticon: &str) -> bool {
        self.animations_for_emoji(emoticon)
            .is_some_and(|m| !m.is_empty())
    }

    /// Returns the number of server animation variants for `emoticon`.
    #[must_use]
    pub fn variant_count(&self, emoticon: &str) -> usize {
        self.animations_for_emoji(emoticon).map_or(0, BTreeMap::len)
    }

    /// Returns the `variant_index`-th server animation entry for `emoticon`, matching
    /// tdesktop's `(begin(list) + index)->second` positional lookup in `emoji_interactions.cpp`.
    #[must_use]
    pub fn animation_at(
        &self,
        emoticon: &str,
        variant_index: usize,
    ) -> Option<&EmojiAnimationEntry> {
        let map = self.animations_for_emoji(emoticon)?;
        map.values()
            .nth(variant_index)
            .or_else(|| map.get(&variant_index))
    }

    /// Stores decompressed UTF-8 Lottie JSON for every animation entry referencing `doc_id`.
    pub fn set_lottie_json(&mut self, doc_id: u64, json: String) {
        for variants in self.animations.values_mut() {
            for entry in variants.values_mut() {
                if entry.document.id.0 == doc_id {
                    entry.lottie_json = Some(json.clone());
                }
            }
        }
    }

    /// Resolves `(document_id, lottie_json)` for `(emoticon, variant_index)` if already
    /// downloaded in memory or present in `media_cache`.
    #[must_use]
    pub fn resolve_lottie(
        &self,
        media_cache: Option<&LiveMediaCache>,
        emoticon: &str,
        variant_index: usize,
    ) -> Option<(u64, String)> {
        let entry = self.animation_at(emoticon, variant_index)?;
        let doc_id = entry.document.id.0;
        if let Some(ref json) = entry.lottie_json
            && !json.is_empty()
        {
            return Some((doc_id, json.clone()));
        }
        if let Some(cache) = media_cache
            && let Some(json) = cache.cached_lottie_json_for_doc(&entry.document)
            && !json.is_empty()
        {
            return Some((doc_id, json));
        }
        None
    }
}

fn collect_animations_indices(packs: &[tl_enums::StickerPack]) -> HashMap<i64, BTreeSet<usize>> {
    let mut result: HashMap<i64, BTreeSet<usize>> = HashMap::new();
    for tl_pack in packs {
        let tl_enums::StickerPack::StickerPack(pack) = tl_pack;
        if let Some(index) = index_from_emoticon(&pack.emoticon) {
            for &doc_id in &pack.documents {
                result.entry(doc_id).or_default().insert(index);
            }
        }
    }
    result
}

// =============================================================================
// Per-session EmojiStickersPack registry (zero fields added to LiveSession)
// =============================================================================

struct EmojiPackEntry {
    owner: Weak<LiveBridgeState>,
    pack: EmojiStickersPack,
}

fn emoji_pack_registry() -> &'static Mutex<HashMap<usize, EmojiPackEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, EmojiPackEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_emoji_pack_by_key<R>(
    bridge_key: usize,
    owner: Option<Weak<LiveBridgeState>>,
    f: impl FnOnce(&mut EmojiStickersPack) -> R,
) -> R {
    let mut map = emoji_pack_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    if let Some(weak) = owner {
        let entry = map.entry(bridge_key).or_insert_with(|| EmojiPackEntry {
            owner: weak,
            pack: EmojiStickersPack::new(),
        });
        f(&mut entry.pack)
    } else if let Some(entry) = map.get_mut(&bridge_key) {
        f(&mut entry.pack)
    } else {
        let mut tmp = EmojiStickersPack::new();
        f(&mut tmp)
    }
}

fn with_emoji_pack<R>(live: &LiveSession, f: impl FnOnce(&mut EmojiStickersPack) -> R) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    with_emoji_pack_by_key(key, Some(Arc::downgrade(live.bridge_state())), f)
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            EMOJI_PACK_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
    .map_err(|e| format!("EmojiStickersPack RPC failed: {e}"))
}

/// Synchronously fetches the server interactive emoji animations pack via
/// `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)` and stores it in the
/// session's [`EmojiStickersPack`].
///
/// Returns the total number of `(emoji, variant_index)` animation entries mapped.
pub fn fetch_emoji_interactions_pack(live: &LiveSession) -> Result<usize, String> {
    let req = tl_fns::messages::GetStickerSet {
        stickerset: tl_enums::InputStickerSet::AnimatedEmojiAnimations,
        hash: 0,
    };
    let resp = invoke_rpc(live, req)?;
    let mapped = match resp {
        tl_enums::messages::StickerSet::StickerSet(full) => {
            with_emoji_pack(live, |pack| pack.apply_animations_set(&full))
        }
        tl_enums::messages::StickerSet::NotModified => {
            with_emoji_pack(live, |pack| pack.emoji_count())
        }
    };
    live.bridge_state().notify_ui();
    Ok(mapped)
}

/// Downloads and caches the `.tgs` Lottie JSON for `(emoticon, variant_index)` from the
/// session's [`EmojiStickersPack`] via `upload.getFile`.
///
/// Validates the decompressed Lottie JSON with `tg_lottie::Composition::from_json` before caching.
pub fn ensure_emoji_animation_downloaded(
    live: &LiveSession,
    emoticon: &str,
    variant_index: usize,
) -> Result<(u64, String), String> {
    enum PackLookup {
        Cached(u64, String),
        NeedsDownload(Box<DocumentData>),
        Missing,
    }

    let lookup = with_emoji_pack(live, |pack| {
        if let Some((doc_id, json)) =
            pack.resolve_lottie(Some(live.media_cache()), emoticon, variant_index)
        {
            return PackLookup::Cached(doc_id, json);
        }
        match pack.animation_at(emoticon, variant_index) {
            Some(entry) => PackLookup::NeedsDownload(Box::new(entry.document.clone())),
            None => PackLookup::Missing,
        }
    });

    let doc = match lookup {
        PackLookup::Cached(doc_id, json) => {
            with_emoji_pack(live, |pack| pack.set_lottie_json(doc_id, json.clone()));
            return Ok((doc_id, json));
        }
        PackLookup::NeedsDownload(doc) => doc,
        PackLookup::Missing => {
            return Err(format!(
                "No server interaction animation mapped for {emoticon} variant {variant_index}"
            ));
        }
    };

    let loc = document_to_location(&doc);
    let rpc = live.runtime().rpc().clone();
    let bytes = download_file_bytes(
        &rpc,
        live.runtime().tokio_handle(),
        loc.dc_id,
        loc.location,
        (loc.size > 0).then_some(loc.size),
    )
    .map_err(|e| format!("Failed to download emoji interaction .tgs: {e}"))?;
    let json = decompress_tgs_to_lottie_json(&bytes)?;
    tg_lottie::Composition::from_json(&json)
        .map_err(|e| format!("Invalid emoji interaction Lottie JSON: {e}"))?;

    let cache_key = document_cache_key(doc.dc_id, doc.id.0);
    let _ = live
        .media_cache()
        .put_tgs_sticker(doc.id.0, &cache_key, &bytes);
    with_emoji_pack(live, |pack| pack.set_lottie_json(doc.id.0, json.clone()));
    live.bridge_state().notify_ui();
    Ok((doc.id.0, json))
}

/// Spawns a non-blocking background fetch of
/// `messages.getStickerSet(InputStickerSetAnimatedEmojiAnimations)` if not yet loaded.
///
/// Returns `true` if a background task was spawned.
pub fn prefetch_emoji_interactions_pack_if_needed(live: &LiveSession) -> bool {
    let should_spawn = with_emoji_pack(live, |pack| {
        if pack.loaded_from_server || pack.request_in_flight {
            return false;
        }
        pack.request_in_flight = true;
        true
    });
    if !should_spawn {
        return false;
    }

    let bridge_state = Arc::clone(live.bridge_state());
    let bridge_key = Arc::as_ptr(&bridge_state) as usize;
    let owner_weak = Arc::downgrade(&bridge_state);
    let rpc = live.runtime().rpc().clone();

    live.runtime().tokio_handle().spawn(async move {
        let req = tl_fns::messages::GetStickerSet {
            stickerset: tl_enums::InputStickerSet::AnimatedEmojiAnimations,
            hash: 0,
        };
        match with_rpc_timeout(
            EMOJI_PACK_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
        {
            Ok(tl_enums::messages::StickerSet::StickerSet(full)) => {
                with_emoji_pack_by_key(bridge_key, Some(owner_weak), |pack| {
                    pack.apply_animations_set(&full);
                });
                bridge_state.notify_ui();
            }
            Ok(tl_enums::messages::StickerSet::NotModified) => {
                with_emoji_pack_by_key(bridge_key, Some(owner_weak), |pack| {
                    pack.request_in_flight = false;
                });
            }
            Err(err) => {
                debug!(%err, "background EmojiStickersPack fetch failed");
                with_emoji_pack_by_key(bridge_key, Some(owner_weak), |pack| {
                    pack.request_in_flight = false;
                });
            }
        }
    });
    true
}

/// Spawns a non-blocking background download of the `.tgs` Lottie payload for
/// `(emoticon, variant_index)` if the server pack is loaded and the document is not yet cached.
///
/// Returns `true` if a background download task was spawned.
pub fn prefetch_emoji_animation_if_needed(
    live: &LiveSession,
    emoticon: &str,
    variant_index: usize,
) -> bool {
    let doc_opt = with_emoji_pack(live, |pack| {
        if pack
            .resolve_lottie(Some(live.media_cache()), emoticon, variant_index)
            .is_some()
        {
            return None;
        }
        let doc = pack.animation_at(emoticon, variant_index)?.document.clone();
        if !pack.in_flight_docs.insert(doc.id.0) {
            return None;
        }
        Some(doc)
    });
    let Some(doc) = doc_opt else {
        return false;
    };

    let bridge_state = Arc::clone(live.bridge_state());
    let bridge_key = Arc::as_ptr(&bridge_state) as usize;
    let owner_weak = Arc::downgrade(&bridge_state);
    let media_cache = live.media_cache().clone();
    let rpc = live.runtime().rpc().clone();

    live.runtime().tokio_handle().spawn(async move {
        let loc = document_to_location(&doc);
        let result = download_file_bytes_with_progress_async(
            &rpc,
            loc.dc_id,
            loc.location,
            (loc.size > 0).then_some(loc.size),
            |_, _| {},
        )
        .await;
        match result {
            Ok(bytes) => {
                if let Ok(json) = decompress_tgs_to_lottie_json(&bytes)
                    && tg_lottie::Composition::from_json(&json).is_ok()
                {
                    let cache_key = document_cache_key(doc.dc_id, doc.id.0);
                    let _ = media_cache.put_tgs_sticker(doc.id.0, &cache_key, &bytes);
                    with_emoji_pack_by_key(bridge_key, Some(owner_weak), |pack| {
                        pack.in_flight_docs.remove(&doc.id.0);
                        pack.set_lottie_json(doc.id.0, json);
                    });
                    bridge_state.notify_ui();
                    return;
                }
            }
            Err(err) => {
                debug!(%err, doc_id = doc.id.0, "background emoji interaction .tgs download failed");
            }
        }
        with_emoji_pack_by_key(bridge_key, Some(owner_weak), |pack| {
            pack.in_flight_docs.remove(&doc.id.0);
        });
    });
    true
}

/// Resolves cached `(document_id, lottie_json)` for an interactive emoji animation in `live`,
/// triggering non-blocking background fetch/download when not yet ready.
///
/// Returns `None` while the pack or `.tgs` asset is not yet loaded so the UI seamlessly falls
/// back to the procedural burst animation.
#[must_use]
pub fn resolve_emoji_interaction_lottie_live(
    live: &LiveSession,
    emoticon: &str,
    variant_index: usize,
) -> Option<(u64, String)> {
    let (loaded, hit) = with_emoji_pack(live, |pack| {
        let hit = pack.resolve_lottie(Some(live.media_cache()), emoticon, variant_index);
        (pack.is_loaded_from_server(), hit)
    });
    if let Some((doc_id, json)) = hit {
        with_emoji_pack(live, |pack| pack.set_lottie_json(doc_id, json.clone()));
        return Some((doc_id, json));
    }
    if !loaded {
        let _ = prefetch_emoji_interactions_pack_if_needed(live);
    } else {
        let _ = prefetch_emoji_animation_if_needed(live, emoticon, variant_index);
    }
    None
}

/// Returns the number of server animation variants for `emoticon` if [`EmojiStickersPack`]
/// has already been fetched from the server and contains animations for `emoticon`.
#[must_use]
pub fn emoji_interaction_variant_count_live(live: &LiveSession, emoticon: &str) -> Option<usize> {
    with_emoji_pack(live, |pack| {
        if !pack.is_loaded_from_server() {
            return None;
        }
        let count = pack.variant_count(emoticon);
        (count > 0).then_some(count)
    })
}

/// Returns a cloned snapshot of the session's [`EmojiStickersPack`].
#[must_use]
pub fn snapshot_emoji_stickers_pack(live: &LiveSession) -> EmojiStickersPack {
    with_emoji_pack(live, |pack| pack.clone())
}

fn tl_document_to_data(doc: &tl_enums::Document) -> Option<DocumentData> {
    let tl_enums::Document::Document(d) = doc else {
        return None;
    };
    let mut file_name = String::new();
    let mut width = None;
    let mut height = None;
    let mut sticker = None;
    let mut kind = if d.mime_type.eq_ignore_ascii_case("application/x-tgsticker") {
        DocumentKind::AnimatedSticker
    } else if d.mime_type.eq_ignore_ascii_case("video/webm") {
        DocumentKind::VideoSticker
    } else {
        DocumentKind::File
    };

    for attr in &d.attributes {
        match attr {
            tl_enums::DocumentAttribute::Filename(f) => {
                file_name = f.file_name.clone();
            }
            tl_enums::DocumentAttribute::ImageSize(sz) => {
                width = Some(sz.w);
                height = Some(sz.h);
            }
            tl_enums::DocumentAttribute::Sticker(s) => {
                let (set_id, set_access_hash) = match &s.stickerset {
                    tl_enums::InputStickerSet::ID(id) => (id.id as u64, id.access_hash as u64),
                    _ => (0, 0),
                };
                sticker = Some(StickerMeta {
                    alt: s.alt.clone(),
                    set_id,
                    set_access_hash,
                    is_mask: s.mask,
                });
                if d.mime_type.eq_ignore_ascii_case("application/x-tgsticker") {
                    kind = DocumentKind::AnimatedSticker;
                } else if d.mime_type.eq_ignore_ascii_case("video/webm") {
                    kind = DocumentKind::VideoSticker;
                } else {
                    kind = DocumentKind::Sticker;
                }
            }
            _ => {}
        }
    }

    Some(DocumentData {
        id: DocumentId(d.id as u64),
        access_hash: d.access_hash as u64,
        file_reference: d.file_reference.clone(),
        date: d.date,
        mime_type: d.mime_type.clone(),
        size: d.size,
        dc_id: d.dc_id,
        kind,
        file_name,
        width,
        height,
        sticker,
        ..Default::default()
    })
}
