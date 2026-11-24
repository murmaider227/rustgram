//! Dice sticker pack fetcher (`InputStickerSetDice`), `messages.getEmojiGameInfo`,
//! `EmojiSetsManager` (`ManageSetsBox` + cloud blob download/unpack), and live video-stream
//! channel/RTMP metadata helpers.
//!
//! Port origin:
//! - `Telegram/SourceFiles/chat_helpers/stickers_dice_pack.h`
//! - `Telegram/SourceFiles/chat_helpers/stickers_dice_pack.cpp`
//! - `Telegram/SourceFiles/chat_helpers/emoji_sets_manager.h`
//! - `Telegram/SourceFiles/chat_helpers/emoji_sets_manager.cpp`

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for};
use tg_data::{DocumentData, DocumentId, DocumentKind, PeerId as DataPeerId, Session, StickerMeta};
use tg_storage::{
    ArchiveLimits, ArchiveWriteResult, CloudBlobLoader, CloudBlobLocation, STALE_ARCHIVE_TIMEOUT,
    UnpackOptions, UnpackReport, archive_folder, clear_stale_archive_files,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::cloud_blob_source::TelegramCloudBlobSource;
use crate::convert::to_data_peer;
use crate::media::{
    ResolvedMediaLocation, block_on_runtime, document_to_location, download_file_bytes,
    with_rpc_timeout,
};
use crate::media_cache::{LiveMediaCache, decompress_tgs_to_lottie_json, document_cache_key};
use crate::session::LiveSession;
use tg_app_core::stand_in::types::PeerId;

/// Official animated dice emoji (`Stickers::DicePacks::kDiceString`).
pub const DICE_EMOJI: &str = "\u{1F3B2}";
/// Official animated dart emoji (`Stickers::DicePacks::kDartString`).
pub const DART_EMOJI: &str = "\u{1F3AF}";
/// Official animated slot-machine emoji (`Stickers::DicePacks::kSlotString`).
pub const SLOT_EMOJI: &str = "\u{1F3B0}";
/// Official animated football/soccer emoji (`Stickers::DicePacks::kFballString`).
pub const FBALL_EMOJI: &str = "\u{26BD}";
/// Official animated basketball emoji (`Stickers::DicePacks::kBballString`).
pub const BBALL_EMOJI: &str = "\u{1F3C0}";
/// Official animated bowling emoji (`🎳`).
pub const BOWLING_EMOJI: &str = "\u{1F3B3}";
/// Party popper emoji used for giveaway winners (`Stickers::DicePacks::kPartyPopper`).
pub const PARTY_POPPER_EMOJI: &str = "\u{1F389}";

/// Default timeout for dice-pack / emoji-set / video-stream RPC calls.
const DICE_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Normalizes a dice emoji key by stripping trailing variation selector U+FE0F
/// (`DicePacks::lookup` in `stickers_dice_pack.cpp:135`).
#[must_use]
pub fn normalize_dice_emoji(emoji: &str) -> &str {
    emoji.strip_suffix('\u{FE0F}').unwrap_or(emoji)
}

/// Returns `true` when `emoji` is the slot-machine dice pack (`DicePacks::IsSlot`).
#[must_use]
pub fn is_slot_emoji(emoji: &str) -> bool {
    normalize_dice_emoji(emoji) == SLOT_EMOJI
}

/// Generates a minimal, valid animated Lottie JSON fallback for local idle dice states
/// (`DicePack::tryGenerateLocalZero` in `stickers_dice_pack.cpp:101-128`).
#[must_use]
pub fn generate_local_dice_idle_lottie(emoji: &str, value: i32) -> String {
    let norm = normalize_dice_emoji(emoji);
    let (r, g, b) = match norm {
        DICE_EMOJI => (0.25, 0.65, 0.89),
        DART_EMOJI => (0.90, 0.27, 0.27),
        SLOT_EMOJI => (0.95, 0.72, 0.16),
        FBALL_EMOJI => (0.20, 0.74, 0.42),
        BBALL_EMOJI => (0.92, 0.48, 0.18),
        BOWLING_EMOJI => (0.36, 0.47, 0.85),
        PARTY_POPPER_EMOJI => (0.88, 0.36, 0.76),
        _ => (0.40, 0.65, 0.90),
    };
    let end_rot = 360 + (value.clamp(0, 64) * 15);
    format!(
        r#"{{"v":"5.7.4","fr":30,"ip":0,"op":30,"w":160,"h":160,"nm":"dice_{value}","ddd":0,"assets":[],"layers":[{{"ddd":0,"ind":1,"ty":4,"nm":"die","sr":1,"ks":{{"o":{{"a":0,"k":100}},"r":{{"a":1,"k":[{{"t":0,"s":[0],"e":[{end_rot}]}},{{"t":30,"s":[{end_rot}]}}]}},"p":{{"a":0,"k":[80,80,0]}},"a":{{"a":0,"k":[0,0,0]}},"s":{{"a":0,"k":[100,100,100]}}}},"ao":0,"shapes":[{{"ty":"rc","d":1,"s":{{"a":0,"k":[92,92]}},"p":{{"a":0,"k":[0,0]}},"r":{{"a":0,"k":20}}}},{{"ty":"fl","c":{{"a":0,"k":[{r:.2},{g:.2},{b:.2},1]}},"o":{{"a":0,"k":100}}}}],"ip":0,"op":30,"st":0}}]}}"#
    )
}

/// Dice stake game options returned by `messages.getEmojiGameInfo`
/// (`Data::DiceGameOptions` in `stickers_dice_pack.cpp:148-193`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiceGameOptions {
    /// Server-provided fairness seed hash (`game_hash`).
    pub seed_hash: String,
    /// Previous stake amount in nanoTON (`prev_stake`).
    pub previous_stake_nano_ton: i64,
    /// Per-outcome milli-multipliers for dice faces `1..=6` (`params[0..6]`).
    pub milli_rewards: [i32; 6],
    /// Jackpot milli-multiplier (`params[6]`).
    pub jackpot_milli_reward: i32,
    /// Current winning streak count (`current_streak`).
    pub current_streak: i32,
    /// Remaining plays allowed (`plays_left`, `0` if absent).
    pub plays_left: i32,
}

impl DiceGameOptions {
    /// Parses a TL [`tl_enums::messages::EmojiGameInfo`] response into [`DiceGameOptions`].
    #[must_use]
    pub fn from_tl(info: &tl_enums::messages::EmojiGameInfo) -> Self {
        match info {
            tl_enums::messages::EmojiGameInfo::EmojiGameUnavailable => Self::default(),
            tl_enums::messages::EmojiGameInfo::EmojiGameDiceInfo(data) => {
                let mut rewards = [0i32; 6];
                let mut jackpot = 0i32;
                for (i, &param) in data.params.iter().enumerate() {
                    if i < 6 {
                        rewards[i] = param;
                    } else if i == 6 {
                        jackpot = param;
                    } else {
                        break;
                    }
                }
                Self {
                    seed_hash: data.game_hash.clone(),
                    previous_stake_nano_ton: data.prev_stake,
                    milli_rewards: rewards,
                    jackpot_milli_reward: jackpot,
                    current_streak: data.current_streak,
                    plays_left: data.plays_left.unwrap_or(0),
                }
            }
        }
    }

    /// Returns `true` when the server reported an active dice game configuration.
    #[must_use]
    pub fn is_available(&self) -> bool {
        !self.seed_hash.is_empty()
    }
}

/// One resolved sticker entry inside a [`DicePack`].
#[derive(Debug, Clone, PartialEq)]
pub struct DiceStickerEntry {
    /// Cloud sticker document metadata (used to download `.tgs` via `upload.getFile`).
    pub document: Option<DocumentData>,
    /// Uncompressed Lottie JSON source once downloaded (or locally generated fallback).
    pub lottie_json: Option<String>,
    /// True if `lottie_json` came from the local zero-state fallback rather than a cloud `.tgs`.
    pub is_local_fallback: bool,
}

/// Single emoji's dice sticker pack mapping outcome values (`0` = idle, `1..=6` = faces,
/// or sequential indices for `🎰` slot reels) to their sticker documents & Lottie JSON.
///
/// Port of `Stickers::DicePack` (`chat_helpers/stickers_dice_pack.cpp`).
#[derive(Debug, Clone)]
pub struct DicePack {
    emoji: String,
    entries: BTreeMap<i32, DiceStickerEntry>,
    loaded_from_server: bool,
    request_in_flight: bool,
}

impl DicePack {
    /// Creates an empty [`DicePack`] for normalized `emoji`.
    #[must_use]
    pub fn new(emoji: &str) -> Self {
        Self {
            emoji: normalize_dice_emoji(emoji).to_owned(),
            entries: BTreeMap::new(),
            loaded_from_server: false,
            request_in_flight: false,
        }
    }

    /// Returns the normalized dice emoji key.
    #[must_use]
    pub fn emoji(&self) -> &str {
        &self.emoji
    }

    /// Returns `true` once `messages.getStickerSet(InputStickerSetDice)` has been applied.
    #[must_use]
    pub const fn is_loaded_from_server(&self) -> bool {
        self.loaded_from_server
    }

    /// Returns the total number of mapped outcome entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no entries are stored yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Generates local idle fallback entries when no server entries are populated yet
    /// (`DicePack::tryGenerateLocalZero` in `stickers_dice_pack.cpp:101-128`).
    pub fn try_generate_local_zero(&mut self) {
        if !self.entries.is_empty() {
            return;
        }
        let mut insert_local = |idx: i32| {
            self.entries.insert(
                idx,
                DiceStickerEntry {
                    document: None,
                    lottie_json: Some(generate_local_dice_idle_lottie(&self.emoji, idx)),
                    is_local_fallback: true,
                },
            );
        };
        match self.emoji.as_str() {
            DICE_EMOJI | DART_EMOJI | BBALL_EMOJI | FBALL_EMOJI | BOWLING_EMOJI
            | PARTY_POPPER_EMOJI => {
                insert_local(0);
            }
            SLOT_EMOJI => {
                // Exact slot-machine idle indices from `stickers_dice_pack.cpp:119-125`:
                // 0 = slot_back, 2 = slot_pull, 8 = slot_0_idle, 14 = slot_1_idle, 20 = slot_2_idle.
                for idx in [0, 2, 8, 14, 20] {
                    insert_local(idx);
                }
            }
            _ => {
                insert_local(0);
            }
        }
    }

    /// Applies a `messages.stickerSet` response returned for `inputStickerSetDice`
    /// (`DicePack::applySet` in `stickers_dice_pack.cpp:64-99`).
    pub fn apply_sticker_set(&mut self, data: &tl_types::messages::StickerSet) -> usize {
        let is_slot = is_slot_emoji(&self.emoji);
        let mut docs_by_id: HashMap<i64, DocumentData> = HashMap::new();
        let mut mapped = 0usize;
        let mut slot_index = 0i32;

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
            if is_slot {
                self.entries.insert(
                    slot_index,
                    DiceStickerEntry {
                        document: Some(doc),
                        lottie_json: None,
                        is_local_fallback: false,
                    },
                );
                slot_index += 1;
                mapped += 1;
            } else {
                docs_by_id.insert(doc.id.0 as i64, doc);
            }
        }

        if is_slot {
            self.loaded_from_server = true;
            self.request_in_flight = false;
            return mapped;
        }

        for tl_pack in &data.packs {
            let tl_enums::StickerPack::StickerPack(pack) = tl_pack;
            let Some(first_char) = pack.emoticon.chars().next() else {
                continue;
            };
            let index = if first_char == '#' {
                0
            } else {
                (first_char as i32) + 1 - ('1' as i32)
            };
            if !(0..=6).contains(&index) {
                continue;
            }
            for &doc_id in &pack.documents {
                if let Some(doc) = docs_by_id.remove(&doc_id) {
                    self.entries.insert(
                        index,
                        DiceStickerEntry {
                            document: Some(doc),
                            lottie_json: None,
                            is_local_fallback: false,
                        },
                    );
                    mapped += 1;
                }
            }
        }

        self.loaded_from_server = true;
        self.request_in_flight = false;
        mapped
    }

    /// Stores decompressed Lottie JSON for outcome `value`.
    pub fn set_lottie_json(&mut self, value: i32, json: String) {
        if let Some(entry) = self.entries.get_mut(&value) {
            entry.lottie_json = Some(json);
            entry.is_local_fallback = false;
        } else {
            self.entries.insert(
                value,
                DiceStickerEntry {
                    document: None,
                    lottie_json: Some(json),
                    is_local_fallback: false,
                },
            );
        }
    }

    /// Looks up the entry for outcome `value`, generating local zero fallback if empty.
    pub fn lookup(&mut self, value: i32) -> Option<&DiceStickerEntry> {
        self.try_generate_local_zero();
        self.entries.get(&value)
    }

    /// Immutable lookup for outcome `value` (returns exact match or falls back to `0` if requested).
    #[must_use]
    pub fn get(&self, value: i32) -> Option<&DiceStickerEntry> {
        self.entries.get(&value)
    }

    /// Returns all `(value, document)` pairs whose `.tgs` payload has not been populated yet.
    #[must_use]
    pub fn pending_documents(&self) -> Vec<(i32, DocumentData)> {
        self.entries
            .iter()
            .filter_map(|(&value, entry)| {
                if entry.lottie_json.is_some() && !entry.is_local_fallback {
                    return None;
                }
                entry.document.clone().map(|d| (value, d))
            })
            .collect()
    }
}

/// Collection of [`DicePack`]s and [`DiceGameOptions`] per session (`Stickers::DicePacks`).
#[derive(Debug, Default, Clone)]
pub struct DicePacksStore {
    packs: HashMap<String, DicePack>,
    game_options: Option<DiceGameOptions>,
}

impl DicePacksStore {
    /// Creates an empty [`DicePacksStore`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns or initializes the [`DicePack`] for `emoji`.
    pub fn pack_mut(&mut self, emoji: &str) -> &mut DicePack {
        let key = normalize_dice_emoji(emoji).to_owned();
        self.packs
            .entry(key.clone())
            .or_insert_with(|| DicePack::new(&key))
    }

    /// Returns the [`DicePack`] for `emoji`, if already initialized.
    #[must_use]
    pub fn pack(&self, emoji: &str) -> Option<&DicePack> {
        self.packs.get(normalize_dice_emoji(emoji))
    }

    /// Looks up the best available `(sticker_id, lottie_json)` for `(emoji, value)`.
    ///
    /// Prefers the exact outcome `value`'s downloaded cloud Lottie JSON, then the pack's idle `0`
    /// Lottie JSON, and finally falls back to [`generate_local_dice_idle_lottie`].
    #[must_use]
    pub fn resolve_lottie(
        &self,
        media_cache: Option<&LiveMediaCache>,
        emoji: &str,
        value: i32,
    ) -> (Option<u64>, String) {
        let norm = normalize_dice_emoji(emoji);
        if let Some(pack) = self.packs.get(norm) {
            for candidate in [value, 0] {
                if let Some(entry) = pack.get(candidate) {
                    let doc_id = entry.document.as_ref().map(|d| d.id.0);
                    if let Some(ref json) = entry.lottie_json
                        && !json.is_empty()
                    {
                        return (doc_id, json.clone());
                    }
                    if let (Some(cache), Some(doc)) = (media_cache, entry.document.as_ref())
                        && let Some(json) = cache.cached_lottie_json_for_doc(doc)
                    {
                        return (doc_id, json);
                    }
                }
            }
        }
        (None, generate_local_dice_idle_lottie(norm, value))
    }

    /// Returns the latest cached [`DiceGameOptions`], if resolved.
    #[must_use]
    pub const fn game_options(&self) -> Option<&DiceGameOptions> {
        self.game_options.as_ref()
    }

    /// Updates cached [`DiceGameOptions`] (e.g. from `messages.getEmojiGameInfo` or `updateEmojiGameInfo`).
    pub fn set_game_options(&mut self, opts: DiceGameOptions) {
        self.game_options = Some(opts);
    }
}

// =============================================================================
// Per-session DicePacks registry (zero fields added to LiveSession)
// =============================================================================

struct DicePacksEntry {
    owner: Weak<LiveBridgeState>,
    updates_listener_started: bool,
    store: DicePacksStore,
}

fn dice_registry() -> &'static Mutex<HashMap<usize, DicePacksEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, DicePacksEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_dice_store_by_key<R>(
    bridge_key: usize,
    owner: Option<Weak<LiveBridgeState>>,
    f: impl FnOnce(&mut DicePacksStore) -> R,
) -> R {
    let mut map = dice_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    if let Some(weak) = owner {
        let entry = map.entry(bridge_key).or_insert_with(|| DicePacksEntry {
            owner: weak,
            updates_listener_started: false,
            store: DicePacksStore::new(),
        });
        f(&mut entry.store)
    } else if let Some(entry) = map.get_mut(&bridge_key) {
        f(&mut entry.store)
    } else {
        let mut tmp = DicePacksStore::new();
        f(&mut tmp)
    }
}

fn with_dice_store<R>(live: &LiveSession, f: impl FnOnce(&mut DicePacksStore) -> R) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    with_dice_store_by_key(key, Some(Arc::downgrade(live.bridge_state())), f)
}

/// Resolves cached dice Lottie JSON (and optional cloud sticker document ID) for a
/// `LiveSession` identified by `bridge_state`, falling back to the local idle Lottie animation
/// when the server pack has not finished downloading yet.
#[must_use]
pub fn cached_dice_lottie_for_session(
    bridge_state: &Arc<LiveBridgeState>,
    media_cache: &LiveMediaCache,
    emoji: &str,
    value: i32,
) -> (Option<u64>, String) {
    let key = Arc::as_ptr(bridge_state) as usize;
    with_dice_store_by_key(key, None, |store| {
        store.resolve_lottie(Some(media_cache), emoji, value)
    })
}

/// Attaches resolved `InputStickerSetDice` Lottie JSON (and document IDs) onto any dice
/// [`tg_app_core::history::backend::HistoryMedia::Sticker`] messages inside `messages`, and triggers a
/// non-blocking background `messages.getStickerSet(InputStickerSetDice)` fetch for any dice
/// outcome that is not yet loaded from the server.
pub fn attach_and_prefetch_dice_lottie(
    live: &LiveSession,
    messages: &mut [tg_app_core::history::backend::HistoryMessage],
) {
    let mut to_prefetch: Vec<(String, i32)> = Vec::new();
    if let Ok(guard) = live.runtime().session().try_read() {
        for msg in messages.iter_mut() {
            let Some(tg_app_core::history::backend::HistoryMedia::Sticker(ref mut st)) = msg.media
            else {
                continue;
            };
            let data_full = tg_data::FullMsgId::new(
                crate::convert::to_data_peer(msg.peer_id),
                crate::convert::to_data_msg(msg.id),
            );
            let Some(data_msg) = guard.message(data_full) else {
                continue;
            };
            let tg_data::MessageContent::Regular {
                media: tg_data::MessageMedia::Dice(dice),
                ..
            } = &data_msg.content
            else {
                continue;
            };
            let (doc_id, json) = cached_dice_lottie_for_session(
                live.bridge_state(),
                live.media_cache(),
                &dice.emoticon,
                dice.value,
            );
            if let Some(id) = doc_id {
                st.sticker_id = id;
            }
            if !json.is_empty() {
                st.lottie_json = json;
            }
            to_prefetch.push((dice.emoticon.clone(), dice.value));
        }
    }
    for (emoji, value) in to_prefetch {
        let _ = prefetch_dice_pack_if_needed(live, &emoji, value);
    }
}

/// Attaches resolved `InputStickerSetDice` Lottie JSON (and document IDs) onto any dice
/// [`tg_app_core::history::backend::HistoryMedia::Sticker`] messages inside `messages`.
pub fn attach_cached_dice_lottie(
    session: &Arc<tokio::sync::RwLock<Session>>,
    bridge_state: &Arc<LiveBridgeState>,
    media_cache: &LiveMediaCache,
    messages: &mut [tg_app_core::history::backend::HistoryMessage],
) {
    let Ok(guard) = session.try_read() else {
        return;
    };
    for msg in messages.iter_mut() {
        let Some(tg_app_core::history::backend::HistoryMedia::Sticker(ref mut st)) = msg.media
        else {
            continue;
        };
        let data_full = tg_data::FullMsgId::new(
            crate::convert::to_data_peer(msg.peer_id),
            crate::convert::to_data_msg(msg.id),
        );
        let Some(data_msg) = guard.message(data_full) else {
            continue;
        };
        let tg_data::MessageContent::Regular {
            media: tg_data::MessageMedia::Dice(dice),
            ..
        } = &data_msg.content
        else {
            continue;
        };
        let (doc_id, json) =
            cached_dice_lottie_for_session(bridge_state, media_cache, &dice.emoticon, dice.value);
        if let Some(id) = doc_id {
            st.sticker_id = id;
        }
        if !json.is_empty() {
            st.lottie_json = json;
        }
    }
}

/// Fetches the official dice sticker pack via `messages.getStickerSet(InputStickerSetDice)` and
/// populates [`DicePacksStore`] for `live`.
///
/// If `download_value` is `Some(v)`, also downloads that outcome's `.tgs` payload via
/// `upload.getFile`, decompresses it into Lottie JSON, and stores it in both `DicePack` and
/// `LiveMediaCache`.
pub fn fetch_dice_pack(
    live: &LiveSession,
    emoji: &str,
    download_value: Option<i32>,
) -> Result<usize, String> {
    let norm = normalize_dice_emoji(emoji).to_owned();
    if norm == PARTY_POPPER_EMOJI {
        with_dice_store(live, |store| {
            store.pack_mut(&norm).try_generate_local_zero();
        });
        return Ok(1);
    }

    let req = tl_fns::messages::GetStickerSet {
        stickerset: tl_enums::InputStickerSet::Dice(tl_types::InputStickerSetDice {
            emoticon: norm.clone(),
        }),
        hash: 0,
    };
    let resp = invoke_rpc(live, req)?;
    let mapped = match resp {
        tl_enums::messages::StickerSet::StickerSet(full) => {
            with_dice_store(live, |store| store.pack_mut(&norm).apply_sticker_set(&full))
        }
        tl_enums::messages::StickerSet::NotModified => {
            with_dice_store(live, |store| store.pack(&norm).map_or(0, DicePack::len))
        }
    };

    if let Some(val) = download_value {
        let _ = ensure_dice_value_lottie_downloaded(live, &norm, val);
    }
    live.bridge_state().notify_ui();
    Ok(mapped)
}

/// Downloads and caches the `.tgs` Lottie JSON for `(emoji, value)` from its mapped
/// [`DocumentData`] if not already cached.
pub fn ensure_dice_value_lottie_downloaded(
    live: &LiveSession,
    emoji: &str,
    value: i32,
) -> Result<String, String> {
    let norm = normalize_dice_emoji(emoji).to_owned();
    let doc_opt = with_dice_store(live, |store| {
        let pack = store.pack(&norm)?;
        if let Some(entry) = pack.get(value) {
            if let Some(ref json) = entry.lottie_json
                && !entry.is_local_fallback
                && !json.is_empty()
            {
                return Some(Err(json.clone()));
            }
            return entry.document.clone().map(Ok);
        }
        None
    });

    let doc = match doc_opt {
        Some(Err(existing_json)) => return Ok(existing_json),
        Some(Ok(doc)) => doc,
        None => return Err(format!("No dice document mapped for {norm} value {value}")),
    };

    if let Some(cached) = live.media_cache().cached_lottie_json_for_doc(&doc) {
        with_dice_store(live, |store| {
            store.pack_mut(&norm).set_lottie_json(value, cached.clone());
        });
        return Ok(cached);
    }

    let loc = document_to_location(&doc);
    let rpc = live.runtime().rpc().clone();
    let bytes = download_file_bytes(
        &rpc,
        live.runtime().tokio_handle(),
        loc.dc_id,
        loc.location,
        (loc.size > 0).then_some(loc.size),
    )
    .map_err(|e| format!("Failed to download dice sticker .tgs: {e}"))?;
    let json = decompress_tgs_to_lottie_json(&bytes)?;

    let cache_key = document_cache_key(doc.dc_id, doc.id.0);
    let _ = live
        .media_cache()
        .put_tgs_sticker(doc.id.0, &cache_key, &bytes);
    with_dice_store(live, |store| {
        store.pack_mut(&norm).set_lottie_json(value, json.clone());
    });
    live.bridge_state().notify_ui();
    Ok(json)
}

/// Resolves `DiceGameOptions` via `messages.getEmojiGameInfo`
/// (`DicePacks::resolveGameOptions` in `stickers_dice_pack.cpp:148-193`).
pub fn resolve_dice_game_options(live: &LiveSession) -> Result<DiceGameOptions, String> {
    let resp = invoke_rpc(live, tl_fns::messages::GetEmojiGameInfo {})?;
    let opts = DiceGameOptions::from_tl(&resp);
    with_dice_store(live, |store| store.set_game_options(opts.clone()));
    Ok(opts)
}

/// Applies an incoming `updateEmojiGameInfo` update to the session's [`DicePacksStore`].
pub fn apply_emoji_game_info_update(
    live: &LiveSession,
    info: &tl_enums::messages::EmojiGameInfo,
) -> DiceGameOptions {
    apply_emoji_game_info_to_bridge(live.bridge_state(), info)
}

fn apply_emoji_game_info_to_bridge(
    bridge_state: &Arc<LiveBridgeState>,
    info: &tl_enums::messages::EmojiGameInfo,
) -> DiceGameOptions {
    let key = Arc::as_ptr(bridge_state) as usize;
    let opts = DiceGameOptions::from_tl(info);
    with_dice_store_by_key(key, Some(Arc::downgrade(bridge_state)), |store| {
        store.set_game_options(opts.clone());
    });
    bridge_state.notify_ui();
    opts
}

fn apply_emoji_game_info_updates_to_bridge(
    bridge_state: &Arc<LiveBridgeState>,
    updates: &tl_enums::Updates,
) -> Option<DiceGameOptions> {
    let mut last = None;
    let mut handle_one = |upd: &tl_enums::Update| {
        if let tl_enums::Update::EmojiGameInfo(info_upd) = upd {
            last = Some(apply_emoji_game_info_to_bridge(
                bridge_state,
                &info_upd.info,
            ));
        }
    };
    match updates {
        tl_enums::Updates::UpdateShort(s) => handle_one(&s.update),
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                handle_one(upd);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for upd in &c.updates {
                handle_one(upd);
            }
        }
        _ => {}
    }
    last
}

/// Forwards every `updateEmojiGameInfo` inside a TL [`tl_enums::Updates`] batch to
/// [`apply_emoji_game_info_update`].
pub fn apply_emoji_game_info_updates(
    live: &LiveSession,
    updates: &tl_enums::Updates,
) -> Option<DiceGameOptions> {
    let mut last = None;
    let mut handle_one = |upd: &tl_enums::Update| {
        if let tl_enums::Update::EmojiGameInfo(info_upd) = upd {
            last = Some(apply_emoji_game_info_update(live, &info_upd.info));
        }
    };
    match updates {
        tl_enums::Updates::UpdateShort(s) => handle_one(&s.update),
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                handle_one(upd);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for upd in &c.updates {
                handle_one(upd);
            }
        }
        _ => {}
    }
    last
}

/// Attaches the background live update pump listener on `live.runtime().rpc().subscribe_updates()`
/// that forwards incoming `updateEmojiGameInfo` events to [`apply_emoji_game_info_update`].
pub(crate) fn attach_dice_pack_updates_listener(live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let should_spawn = {
        let mut map = dice_registry().lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, entry| entry.owner.strong_count() > 0);
        let entry = map.entry(key).or_insert_with(|| DicePacksEntry {
            owner: Arc::downgrade(live.bridge_state()),
            updates_listener_started: false,
            store: DicePacksStore::new(),
        });
        if entry.updates_listener_started {
            false
        } else {
            entry.updates_listener_started = true;
            true
        }
    };
    if !should_spawn {
        return;
    }

    let handle = live.runtime().tokio_handle();
    let updates_rx = {
        let _guard = handle.enter();
        live.runtime().rpc().subscribe_updates()
    };
    let Some(mut rx) = updates_rx else {
        return;
    };

    let bridge_weak = Arc::downgrade(live.bridge_state());
    handle.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(updates) => {
                    let Some(bridge) = bridge_weak.upgrade() else {
                        break;
                    };
                    let _ = apply_emoji_game_info_updates_to_bridge(&bridge, &updates);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "dice_pack updateEmojiGameInfo listener lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Returns the cached [`DiceGameOptions`] for `live`, if any.
#[must_use]
pub fn cached_dice_game_options(live: &LiveSession) -> Option<DiceGameOptions> {
    with_dice_store(live, |store| store.game_options().cloned())
}

/// Returns a snapshot of the [`DicePack`] for `(live, emoji)`, if loaded.
#[must_use]
pub fn snapshot_dice_pack(live: &LiveSession, emoji: &str) -> Option<DicePack> {
    with_dice_store(live, |store| store.pack(emoji).cloned())
}

/// Triggers a non-blocking background fetch of `messages.getStickerSet(InputStickerSetDice)`
/// and outcome `.tgs` download when `(emoji, value)` is not yet populated from the server
/// (`DicePack::lookup` auto-load in `stickers_dice_pack.cpp:37-44`).
///
/// Returns `true` if a background fetch task was spawned, or `false` if already loaded /
/// in-flight / local-only (`🎉`).
pub fn prefetch_dice_pack_if_needed(live: &LiveSession, emoji: &str, value: i32) -> bool {
    let norm = normalize_dice_emoji(emoji).to_owned();
    if norm == PARTY_POPPER_EMOJI {
        with_dice_store(live, |store| {
            store.pack_mut(&norm).try_generate_local_zero();
        });
        return false;
    }

    let should_spawn = with_dice_store(live, |store| {
        let pack = store.pack_mut(&norm);
        if pack.request_in_flight {
            return false;
        }
        let already_has_real_lottie = pack.loaded_from_server
            && pack
                .get(value)
                .is_some_and(|e| e.lottie_json.is_some() && !e.is_local_fallback);
        if already_has_real_lottie {
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
    let media_cache = live.media_cache().clone();
    let handle = live.runtime().tokio_handle().clone();

    handle.spawn(async move {
        let need_set = with_dice_store_by_key(bridge_key, Some(owner_weak.clone()), |store| {
            !store.pack_mut(&norm).loaded_from_server
        });
        if need_set {
            let req = tl_fns::messages::GetStickerSet {
                stickerset: tl_enums::InputStickerSet::Dice(tl_types::InputStickerSetDice {
                    emoticon: norm.clone(),
                }),
                hash: 0,
            };
            if let Ok(tl_enums::messages::StickerSet::StickerSet(full)) = rpc.invoke(&req).await {
                with_dice_store_by_key(bridge_key, Some(owner_weak.clone()), |store| {
                    store.pack_mut(&norm).apply_sticker_set(&full);
                });
            }
        }

        let doc_opt = with_dice_store_by_key(bridge_key, Some(owner_weak.clone()), |store| {
            store.pack(&norm).and_then(|p| p.get(value)).and_then(|e| {
                if e.lottie_json.is_some() && !e.is_local_fallback {
                    None
                } else {
                    e.document.clone()
                }
            })
        });

        if let Some(doc) = doc_opt {
            if let Some(cached) = media_cache.cached_lottie_json_for_doc(&doc) {
                with_dice_store_by_key(bridge_key, Some(owner_weak.clone()), |store| {
                    store.pack_mut(&norm).set_lottie_json(value, cached);
                });
            } else {
                let loc = document_to_location(&doc);
                let mut offset = 0i64;
                let mut buf = Vec::new();
                loop {
                    let req = tl_fns::upload::GetFile {
                        precise: false,
                        cdn_supported: false,
                        location: loc.location.clone(),
                        offset,
                        limit: 512 * 1024,
                    };
                    match rpc.invoke_in_dc(loc.dc_id, &req).await {
                        Ok(tl_enums::upload::File::File(f)) => {
                            let n = f.bytes.len();
                            buf.extend_from_slice(&f.bytes);
                            offset += n as i64;
                            if n < 512 * 1024 || (loc.size > 0 && offset as u64 >= loc.size) {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                if !buf.is_empty()
                    && let Ok(json) = decompress_tgs_to_lottie_json(&buf)
                {
                    let cache_key = document_cache_key(doc.dc_id, doc.id.0);
                    let _ = media_cache.put_tgs_sticker(doc.id.0, &cache_key, &buf);
                    with_dice_store_by_key(bridge_key, Some(owner_weak.clone()), |store| {
                        store.pack_mut(&norm).set_lottie_json(value, json);
                    });
                }
            }
        }

        with_dice_store_by_key(bridge_key, Some(owner_weak), |store| {
            store.pack_mut(&norm).request_in_flight = false;
        });
        bridge_state.notify_ui();
    });
    true
}

// =============================================================================
// EmojiSetsManager (`EmojiSetCloudLoader` impl for `LiveSession`)
// =============================================================================

pub use tg_app_core::core_types::emoji_sets::{
    EMOJI_CLOUD_LOCATION_USERNAME, EMOJI_SETS, EmojiCloudLocation, EmojiSetCloudLoader,
    EmojiSetDescriptor, EmojiSetState, EmojiSetsManager, ManageEmojiSetsAction,
    good_emoji_set_part_name, show_emoji_sets_modal_window, unpack_emoji_set_zip,
    unpack_emoji_set_zip_verified,
};

/// Removes stale temporary archive files under an emoji-sets storage directory using
/// `tg_storage::folder_archive::clear_stale_archive_files`.
#[must_use]
pub fn clear_stale_emoji_set_archives(storage_root: &Path) -> usize {
    clear_stale_archive_files(storage_root, STALE_ARCHIVE_TIMEOUT).unwrap_or(0)
}

/// Deterministically archives an unpacked emoji-set directory to `output_zip` using
/// `tg_storage::folder_archive::archive_folder`.
pub fn archive_emoji_set_folder(
    set_dir: &Path,
    output_zip: &Path,
) -> Result<ArchiveWriteResult, String> {
    archive_folder(set_dir, output_zip, &ArchiveLimits::default(), |_| true)
        .map_err(|err| format!("Failed to archive emoji set folder: {err}"))
}

impl EmojiSetCloudLoader for LiveSession {
    fn download_emoji_set_archive(&self, username: &str, post_id: i32) -> Result<Vec<u8>, String> {
        let source = TelegramCloudBlobSource::new(
            self.runtime().rpc().clone(),
            self.runtime().tokio_handle().clone(),
        );
        let location = CloudBlobLocation::new(username, post_id);
        let mut bytes = Vec::new();
        tg_storage::CloudBlobSource::download_to(&source, &location, &mut bytes, &mut |_| true)
            .map_err(|err| format!("Emoji set download failed: {err}"))?;
        Ok(bytes)
    }

    fn download_and_unpack_emoji_set(
        &self,
        id: i32,
        username: &str,
        post_id: i32,
        size_bytes: u64,
        dest_dir: &Path,
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<UnpackReport, String> {
        if let Some(parent) = dest_dir.parent() {
            let _ = clear_stale_emoji_set_archives(parent);
        }
        let source = TelegramCloudBlobSource::new(
            self.runtime().rpc().clone(),
            self.runtime().tokio_handle().clone(),
        );
        let loader = CloudBlobLoader::new(
            id,
            CloudBlobLocation::new(username, post_id),
            dest_dir,
            size_bytes,
            UnpackOptions {
                expected_sha256,
                ..UnpackOptions::default()
            },
        );
        loader
            .download_and_unpack_sync(&source, good_emoji_set_part_name)
            .map_err(|err| format!("Emoji set cloud blob load failed: {err}"))
    }
}

// =============================================================================
// Live Video Stream RPC helpers (`phone.getGroupCall`, `getGroupCallStreamChannels`,
// `getGroupCallStreamRtmpUrl`)
// =============================================================================

/// One live video-stream quality channel returned by `phone.getGroupCallStreamChannels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamChannelInfo {
    /// Channel identifier passed in `inputGroupCallStream.video_channel`.
    pub channel: i32,
    /// Scale exponent (`0` = 1000ms chunks, `1` = 500ms chunks, etc.).
    pub scale: i32,
    /// Last available server timestamp in milliseconds.
    pub last_timestamp_ms: i64,
}

/// Resolved live video-stream metadata from `phone.getGroupCall` + `phone.getGroupCallStreamChannels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVideoStreamInfo {
    /// Group call ID.
    pub call_id: i64,
    /// Group call access hash.
    pub access_hash: u64,
    /// Stream hosting DC ID (`GroupCall.stream_dc_id`).
    pub stream_dc_id: i32,
    /// Stream title (from `GroupCall.title`).
    pub title: String,
    /// Active viewer / participant count.
    pub participants_count: i32,
    /// Whether this call is an RTMP stream.
    pub rtmp_stream: bool,
    /// Available stream channels (video/audio time scales).
    pub channels: Vec<VideoStreamChannelInfo>,
}

/// RTMP ingest endpoint returned by `phone.getGroupCallStreamRtmpUrl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRtmpEndpoint {
    /// RTMP server URL.
    pub url: String,
    /// Stream key.
    pub key: String,
}

/// Fetches live video-stream metadata (`phone.getGroupCall` + `phone.getGroupCallStreamChannels`)
/// for a `MessageMedia::VideoStream` or live story call.
pub fn fetch_video_stream_info(
    live: &LiveSession,
    call_id: i64,
    access_hash: u64,
) -> Result<ResolvedVideoStreamInfo, String> {
    let input_call = tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
        id: call_id,
        access_hash: access_hash as i64,
    });
    let call_resp = invoke_rpc(
        live,
        tl_fns::phone::GetGroupCall {
            call: input_call.clone(),
            limit: 20,
        },
    )?;
    let tl_enums::phone::GroupCall::GroupCall(gc) = call_resp;
    write_session(live, |s| apply_tl_users_and_chats(s, &gc.users, &gc.chats));

    let (title, participants_count, stream_dc_id, rtmp_stream) = match gc.call {
        tl_enums::GroupCall::GroupCall(c) => (
            c.title.unwrap_or_else(|| "Live Stream".to_owned()),
            c.participants_count.max(0),
            c.stream_dc_id.unwrap_or(0),
            c.rtmp_stream,
        ),
        tl_enums::GroupCall::Discarded(d) => {
            return Err(format!(
                "Live stream #{} has ended (duration {}s)",
                d.id, d.duration
            ));
        }
    };

    let channels = match invoke_rpc(
        live,
        tl_fns::phone::GetGroupCallStreamChannels { call: input_call },
    ) {
        Ok(tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(ch)) => ch
            .channels
            .into_iter()
            .map(|c| {
                let tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(item) = c;
                VideoStreamChannelInfo {
                    channel: item.channel,
                    scale: item.scale,
                    last_timestamp_ms: item.last_timestamp_ms,
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    Ok(ResolvedVideoStreamInfo {
        call_id,
        access_hash,
        stream_dc_id,
        title,
        participants_count,
        rtmp_stream,
        channels,
    })
}

/// Fetches RTMP URL + stream key for a peer's live stream or live story via
/// `phone.getGroupCallStreamRtmpUrl`.
pub fn fetch_video_stream_rtmp_url(
    live: &LiveSession,
    peer_id: PeerId,
    live_story: bool,
    revoke: bool,
) -> Result<ResolvedRtmpEndpoint, String> {
    let data_peer: DataPeerId = to_data_peer(peer_id);
    let input_peer = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err(format!("Unknown peer {} for RTMP stream URL", peer_id.0));
    }
    let resp = invoke_rpc(
        live,
        tl_fns::phone::GetGroupCallStreamRtmpUrl {
            live_story,
            peer: input_peer,
            revoke,
        },
    )?;
    let tl_enums::phone::GroupCallStreamRtmpUrl::GroupCallStreamRtmpUrl(rtmp) = resp;
    Ok(ResolvedRtmpEndpoint {
        url: rtmp.url,
        key: rtmp.key,
    })
}

// =============================================================================
// Internal TL / RPC helpers
// =============================================================================

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(DICE_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
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

/// Ensures [`ResolvedMediaLocation`] import stays available for callers building custom loaders.
#[must_use]
pub fn dice_document_location(doc: &DocumentData) -> ResolvedMediaLocation {
    document_to_location(doc)
}
