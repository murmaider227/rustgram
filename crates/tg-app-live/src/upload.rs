//! Live media uploader, album sending, Terms of Service RPCs, clipboard image copy,
//! and media overlay forwarding for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/storage/file_upload.cpp`
//! - `Telegram/SourceFiles/storage/localimageloader.cpp`
//! - `Telegram/SourceFiles/storage/storage_media_prepare.cpp`
//! - `Telegram/SourceFiles/api/api_sending.cpp`
//! - `Telegram/SourceFiles/api/api_media.cpp`
//! - `Telegram/SourceFiles/apiwrap.cpp` (`sendFiles`, `sendMedia`, `sendAlbum`, `requestTermsUpdate`, `acceptTerms`)
//! - `Telegram/SourceFiles/main/main_app_config.cpp`
//! - `Telegram/SourceFiles/boxes/terms_box.cpp`

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use tg_api::RpcClient;
use tg_api::SendMessageOutcome;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for};
use tg_api::rpc::InvocationError;
use tg_crypto::hash::Md5Hasher;
use tg_data::{
    FullMsgId as DataFullMsgId, MessageFlags, MsgId as DataMsgId, PeerId as DataPeerId, Session,
};
use tg_media::transfer::download::CancelToken;
use tg_media::transfer::upload::{
    BIG_FILE_THRESHOLD, MAX_UPLOAD_PER_SESSION, MAX_UPLOAD_SESSIONS, UploadConfig, UploadPlan,
    UploadProgress, UploadSessionBalancer, generate_random_file_id,
};
use tg_text::TextWithEntities;
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::bridge::LiveBridgeState;
use crate::convert::{
    from_data_msg, from_data_peer, message_to_history_message, now_unix_i32, peer_display_title,
    to_data_msg, to_data_peer, ui_text_to_data_text, ui_text_to_tl_entities,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::optimistic_send::SendResolution;
use crate::session::{LiveSession, make_input_reply_to};
use tg_app_core::core_types::misc::transform_dynamic_image;
use tg_app_core::history::backend::{
    HistoryBackend, HistoryMedia, HistoryMessage, MediaUploadStatus, SendMessageExtended,
};
use tg_app_core::history::sections_model::{ContactAttachment, ForwardOptions, LocationAttachment};
use tg_app_core::info::backend::TermsOfServiceData;
use tg_app_core::media::backend::{MediaBackend, MediaItemId};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Minimum Terms of Service re-check interval (`kTermsUpdateTimeoutMin` = 10s in `apiwrap.cpp`).
const TERMS_UPDATE_TIMEOUT_MIN_SECS: i32 = 10;

/// Maximum Terms of Service re-check interval (`kTermsUpdateTimeoutMax` = 86400s in `apiwrap.cpp`).
const TERMS_UPDATE_TIMEOUT_MAX_SECS: i32 = 86_400;

/// Default timeout for a single upload part RPC (`upload.saveFilePart` / `upload.saveBigFilePart`).
const UPLOAD_PART_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Default timeout for send / album / ToS / forward RPCs.
const UPLOAD_SEND_RPC_TIMEOUT: Duration = Duration::from_secs(20);

/// Configuration for [`upload_file_bytes_async`] and [`LiveSession`] media uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveUploadConfig {
    /// Byte threshold above which `upload.saveBigFilePart` / `inputFileBig` is used
    /// (default: [`BIG_FILE_THRESHOLD`] = 10 MiB).
    pub big_file_threshold: u64,
    /// Whether the signed-in account has Telegram Premium (allows up to 4000 MiB / 8000 parts).
    pub is_premium: bool,
    /// Forces 32 KiB slices (`PHOTO_UPLOAD_PART_SIZE`) and `.jpg` suffix for compressed photos.
    pub is_photo: bool,
    /// Maximum concurrent upload sessions (`1..=8`, default `4`).
    pub max_sessions: usize,
    /// Maximum retries per part or send RPC on `FLOOD_WAIT`, `FILE_PART_X_MISSING`, or transient errors.
    pub max_retries: usize,
    /// Upper bound on sleep duration for a single `FLOOD_WAIT_X` retry.
    pub max_flood_wait: Duration,
    /// Optional deterministic `file_id` override (primarily for tests).
    pub explicit_file_id: Option<i64>,
}

impl Default for LiveUploadConfig {
    fn default() -> Self {
        Self {
            big_file_threshold: BIG_FILE_THRESHOLD,
            is_premium: false,
            is_photo: false,
            max_sessions: 4,
            max_retries: 5,
            max_flood_wait: Duration::from_secs(60),
            explicit_file_id: None,
        }
    }
}

impl LiveUploadConfig {
    fn to_media_config(self) -> UploadConfig {
        UploadConfig {
            big_file_threshold: self.big_file_threshold,
            is_premium: self.is_premium,
            is_photo: self.is_photo,
            max_sessions: self.max_sessions.clamp(1, MAX_UPLOAD_SESSIONS),
        }
    }
}

/// Errors returned by the live file uploader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadError {
    /// Invalid file size or upload planning parameter.
    InvalidFile(String),
    /// Upload was cancelled via [`CancelToken`].
    Cancelled,
    /// MTProto RPC failure (including exhausted retries).
    Rpc(String),
    /// Request timed out.
    Timeout(String),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFile(msg) => write!(f, "invalid upload file: {msg}"),
            Self::Cancelled => write!(f, "upload cancelled"),
            Self::Rpc(msg) => write!(f, "upload RPC error: {msg}"),
            Self::Timeout(msg) => write!(f, "upload timed out: {msg}"),
        }
    }
}

impl std::error::Error for UploadError {}

/// Completed upload descriptor ready to embed into `InputMedia`.
#[derive(Debug, Clone)]
pub struct UploadedFileResult {
    /// Resulting TL `InputFile` (`inputFile` with MD5 for small files, `inputFileBig` for big files).
    pub input_file: tl_enums::InputFile,
    /// Upload plan (retained so `FILE_PART_X_MISSING` on send RPCs can re-upload the exact part).
    pub plan: UploadPlan,
    /// Raw bytes retained for on-demand part re-upload on `FILE_PART_X_MISSING`.
    pub bytes: Arc<Vec<u8>>,
}

/// Outgoing media payload for [`LiveSession::send_uploaded_media`] and [`LiveSession::send_album`].
#[derive(Debug, Clone)]
pub enum OutgoingMedia {
    /// Compressed photo (`inputMediaUploadedPhoto`).
    Photo {
        /// Encoded JPEG/PNG/WebP image bytes.
        bytes: Vec<u8>,
        /// File name (`.jpg` is appended if missing, matching `file_upload.cpp:1130`).
        file_name: String,
        /// Whether to mark the photo with a spoiler veil.
        spoiler: bool,
        /// Optional self-destruct TTL in seconds.
        ttl_seconds: Option<i32>,
    },
    /// Generic document sent as file (`inputMediaUploadedDocument`).
    Document {
        /// Raw document bytes.
        bytes: Vec<u8>,
        /// Display file name.
        file_name: String,
        /// MIME type (e.g. `"application/pdf"`).
        mime_type: String,
        /// Optional JPEG/WebP thumbnail bytes uploaded as a secondary small file.
        thumb_bytes: Option<Vec<u8>>,
        /// Sets `force_file` flag (`send as file`).
        force_file: bool,
        /// Whether to mark the document with a spoiler veil.
        spoiler: bool,
    },
    /// Video attachment (`inputMediaUploadedDocument` + `documentAttributeVideo`).
    Video {
        /// Encoded video bytes (e.g. MP4).
        bytes: Vec<u8>,
        /// File name.
        file_name: String,
        /// MIME type (defaults to `"video/mp4"` if empty).
        mime_type: String,
        /// Video width in pixels.
        width: i32,
        /// Video height in pixels.
        height: i32,
        /// Duration in seconds.
        duration_secs: f64,
        /// Whether `supports_streaming` is set.
        supports_streaming: bool,
        /// Whether the video has no audio track (`nosound_video` + `documentAttributeVideo.nosound`).
        nosound: bool,
        /// Whether this is a circular round video note (`round_message`).
        round_message: bool,
        /// Optional JPEG thumbnail bytes.
        thumb_bytes: Option<Vec<u8>>,
        /// Optional custom video cover JPEG/PNG bytes uploaded via `messages.uploadMedia` and
        /// attached to `InputMediaUploadedDocument::video_cover` (`history_view_video_cover_uploader.cpp`).
        video_cover_bytes: Option<Vec<u8>>,
        /// Whether to mark the video with a spoiler veil.
        spoiler: bool,
        /// Optional self-destruct TTL in seconds.
        ttl_seconds: Option<i32>,
    },
    /// Voice note attachment (`documentAttributeAudio` with `voice: true` and 5-bit waveform).
    Voice {
        /// Encoded OGG/Opus audio bytes.
        bytes: Vec<u8>,
        /// File name (defaults to `"voice.ogg"` if empty).
        file_name: String,
        /// Duration in seconds.
        duration_secs: i32,
        /// Telegram 5-bit packed waveform bytes.
        waveform_5bit: Vec<u8>,
    },
    /// Music / audio file (`documentAttributeAudio` with `voice: false`, title, and performer).
    Audio {
        /// Encoded audio bytes.
        bytes: Vec<u8>,
        /// File name.
        file_name: String,
        /// MIME type (defaults to `"audio/mpeg"` if empty).
        mime_type: String,
        /// Duration in seconds.
        duration_secs: i32,
        /// Optional track title.
        title: Option<String>,
        /// Optional performer/artist.
        performer: Option<String>,
        /// Optional album art JPEG thumbnail bytes.
        thumb_bytes: Option<Vec<u8>>,
    },
    /// Sticker attachment (`documentAttributeSticker` + `documentAttributeImageSize`).
    Sticker {
        /// Encoded sticker bytes (WebP, TGS Lottie, or WebM).
        bytes: Vec<u8>,
        /// File name.
        file_name: String,
        /// MIME type (defaults to `"image/webp"` if empty).
        mime_type: String,
        /// Sticker width in pixels.
        width: i32,
        /// Sticker height in pixels.
        height: i32,
        /// Associated emoji (`alt`).
        alt_emoji: String,
    },
    /// Animated GIF / silent looping video (`documentAttributeAnimated` + `nosound_video`).
    Gif {
        /// Encoded GIF or MP4 animation bytes.
        bytes: Vec<u8>,
        /// File name.
        file_name: String,
        /// MIME type (defaults to `"video/mp4"` if empty).
        mime_type: String,
        /// Animation width in pixels.
        width: i32,
        /// Animation height in pixels.
        height: i32,
        /// Optional duration in seconds (for MP4 GIFv).
        duration_secs: Option<f64>,
        /// Optional JPEG thumbnail bytes.
        thumb_bytes: Option<Vec<u8>>,
        /// Whether to mark the GIF with a spoiler veil.
        spoiler: bool,
    },
    /// Static/live geo-point or venue (`inputMediaGeoPoint` / `inputMediaVenue` / `inputMediaGeoLive`).
    Location(LocationAttachment),
    /// Shared contact card (`inputMediaContact`).
    Contact(ContactAttachment),
}

impl OutgoingMedia {
    fn display_name(&self) -> String {
        match self {
            Self::Photo { file_name, .. }
            | Self::Document { file_name, .. }
            | Self::Video { file_name, .. }
            | Self::Voice { file_name, .. }
            | Self::Audio { file_name, .. }
            | Self::Sticker { file_name, .. }
            | Self::Gif { file_name, .. } => file_name.clone(),
            Self::Location(_) => "location".to_owned(),
            Self::Contact(c) => c.full_name(),
        }
    }

    fn total_upload_bytes(&self) -> u64 {
        match self {
            Self::Photo { bytes, .. } | Self::Voice { bytes, .. } | Self::Sticker { bytes, .. } => {
                bytes.len() as u64
            }
            Self::Video {
                bytes,
                thumb_bytes,
                video_cover_bytes,
                ..
            } => {
                (bytes.len()
                    + thumb_bytes.as_ref().map_or(0, Vec::len)
                    + video_cover_bytes.as_ref().map_or(0, Vec::len)) as u64
            }
            Self::Document {
                bytes, thumb_bytes, ..
            }
            | Self::Audio {
                bytes, thumb_bytes, ..
            }
            | Self::Gif {
                bytes, thumb_bytes, ..
            } => (bytes.len() + thumb_bytes.as_ref().map_or(0, Vec::len)) as u64,
            Self::Location(_) | Self::Contact(_) => 0,
        }
    }

    fn is_spoiler(&self) -> bool {
        match self {
            Self::Photo { spoiler, .. }
            | Self::Document { spoiler, .. }
            | Self::Video { spoiler, .. }
            | Self::Gif { spoiler, .. } => *spoiler,
            _ => false,
        }
    }
}

/// Shared options for sending a single media message or grouped album.
#[derive(Debug, Clone, Default)]
pub struct SendMediaOptions {
    /// Formatted caption text and UTF-16 entities.
    pub caption: TextWithEntities,
    /// Optional replied-to message id.
    pub reply_to: Option<MsgId>,
    /// Optional quoted passage within `reply_to`.
    pub reply_quote: Option<String>,
    /// Send silently without notification sound.
    pub silent: bool,
    /// Optional scheduled send Unix timestamp.
    pub schedule_date: Option<i32>,
    /// Optional peer id to send as (channel/alias).
    pub send_as: Option<PeerId>,
    /// Optional message effect id.
    pub effect_id: Option<u64>,
    /// Place caption above media (`invert_media`).
    pub invert_media: bool,
    /// Clear cloud draft on send.
    pub clear_draft: bool,
}

/// One element of a multi-media album sent via `messages.uploadMedia` + `messages.sendMultiMedia`.
#[derive(Debug, Clone)]
pub struct AlbumUploadItem {
    /// Media payload to upload and attach.
    pub media: OutgoingMedia,
    /// Optional per-item caption; when `None` for all items, `SendMediaOptions::caption`
    /// is placed on the first item (matching tdesktop's `SendingAlbum`).
    pub caption: Option<TextWithEntities>,
}

mod album_queue;

// ============================================================================
// Per-session state registry (keeps LiveSession struct untouched for clean merges)
// ============================================================================

struct TrackedMediaUpload {
    status: MediaUploadStatus,
    cancel: CancelToken,
}

/// A registered media upload with its not yet started request
/// (`LiveSession::prepare_media_send`).
struct PreparedMediaSend {
    state: Arc<Mutex<UploadLiveState>>,
    upload_id: u64,
    random_id: i64,
    cancel: CancelToken,
    /// TGB-2998: the file, to seed the media cache under the document the server returns.
    seed_bytes: Option<Vec<u8>>,
    request: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<tl_enums::Updates, String>> + Send>,
    >,
}

/// Ends the tracking of `upload_id`: a success drops it from the uploads bar, a failure stays
/// there marked failed (or cancelled).
fn finish_tracked_upload(state: &Mutex<UploadLiveState>, upload_id: u64, error: Option<&str>) {
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if error.is_none() {
        guard.active_uploads.remove(&upload_id);
    } else if let Some(entry) = guard.active_uploads.get_mut(&upload_id) {
        if entry.cancel.is_cancelled() {
            entry.status.cancelled = true;
        } else {
            entry.status.failed = true;
        }
    }
}

struct CachedTermsOfService {
    data: TermsOfServiceData,
    next_check_unix: i32,
}

/// Optional test hook overriding OS clipboard image writes during unit tests.
type ClipboardTestSink = Arc<dyn Fn(u32, u32, &[u8]) -> anyhow::Result<()> + Send + Sync>;

struct UploadLiveState {
    active_uploads: HashMap<u64, TrackedMediaUpload>,
    next_upload_id: u64,
    next_random_id: u64,
    tos_cache: Option<CachedTermsOfService>,
    media_forward_target: Option<PeerId>,
    staged_media_bytes: HashMap<String, Vec<u8>>,
    upload_config: LiveUploadConfig,
    clipboard_sink_override: Option<ClipboardTestSink>,
}

impl Default for UploadLiveState {
    fn default() -> Self {
        Self {
            active_uploads: HashMap::new(),
            next_upload_id: 1,
            next_random_id: 1,
            tos_cache: None,
            media_forward_target: None,
            staged_media_bytes: HashMap::new(),
            upload_config: LiveUploadConfig::default(),
            clipboard_sink_override: None,
        }
    }
}

impl UploadLiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0x0001_0AD5_CAFE_BABE) as i64
    }
}

struct UploadStateEntry {
    owner: Weak<LiveBridgeState>,
    state: Arc<Mutex<UploadLiveState>>,
}

fn upload_state_registry() -> &'static Mutex<HashMap<usize, UploadStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, UploadStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn upload_state_for(live: &LiveSession) -> Arc<Mutex<UploadLiveState>> {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = upload_state_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    if let Some(entry) = map.get(&key) {
        Arc::clone(&entry.state)
    } else {
        let state = Arc::new(Mutex::new(UploadLiveState::default()));
        map.insert(
            key,
            UploadStateEntry {
                owner: Arc::downgrade(live.bridge_state()),
                state: Arc::clone(&state),
            },
        );
        state
    }
}

// ============================================================================
// Error parsing & retry helpers
// ============================================================================

/// Parses a Telegram `FILE_PART_<index>_MISSING` RPC error name and returns `<index>`.
#[must_use]
pub fn parse_missing_file_part(error_name: &str) -> Option<u32> {
    let rest = error_name.strip_prefix("FILE_PART_")?;
    let digits = rest.strip_suffix("_MISSING")?;
    digits.parse::<u32>().ok()
}

fn flood_wait_duration(err: &InvocationError, max_wait: Duration) -> Option<Duration> {
    if let InvocationError::Rpc(rpc) = err
        && let Some(secs) = rpc.flood_wait_seconds()
    {
        return Some(Duration::from_secs(u64::from(secs)).min(max_wait));
    }
    None
}

fn missing_part_from_error(err: &InvocationError) -> Option<u32> {
    if let InvocationError::Rpc(rpc) = err {
        parse_missing_file_part(&rpc.name)
    } else {
        None
    }
}

async fn sleep_cancellable(duration: Duration, cancel: &CancelToken) -> Result<(), UploadError> {
    if cancel.is_cancelled() {
        return Err(UploadError::Cancelled);
    }
    if duration.is_zero() {
        return Ok(());
    }
    let step = Duration::from_millis(25);
    let mut remaining = duration;
    while !remaining.is_zero() {
        if cancel.is_cancelled() {
            return Err(UploadError::Cancelled);
        }
        let nap = remaining.min(step);
        tokio::time::sleep(nap).await;
        remaining = remaining.saturating_sub(nap);
    }
    if cancel.is_cancelled() {
        return Err(UploadError::Cancelled);
    }
    Ok(())
}

// ============================================================================
// Core async file uploader (`upload.saveFilePart` / `upload.saveBigFilePart`)
// ============================================================================

/// Uploads one specific part `part_index` of `plan` from `data` with `FLOOD_WAIT` retry.
pub(crate) async fn upload_single_part_async<C: RpcClient>(
    rpc: &C,
    plan: &UploadPlan,
    data: &[u8],
    part_index: u32,
    cfg: LiveUploadConfig,
    cancel: &CancelToken,
) -> Result<(), UploadError> {
    if part_index >= plan.total_parts {
        return Err(UploadError::InvalidFile(format!(
            "part index {part_index} out of range (total_parts = {})",
            plan.total_parts
        )));
    }
    let offset = (u64::from(part_index) * u64::from(plan.part_size)) as usize;
    let end = (offset + plan.part_size as usize).min(data.len());
    let slice = data[offset..end].to_vec();

    let mut attempt = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err(UploadError::Cancelled);
        }
        let rpc_res: Result<bool, InvocationError> = if plan.is_big {
            let req = tl_fns::upload::SaveBigFilePart {
                file_id: plan.file_id,
                file_part: part_index as i32,
                file_total_parts: plan.total_parts as i32,
                bytes: slice.clone(),
            };
            tokio::time::timeout(UPLOAD_PART_RPC_TIMEOUT, rpc.invoke(&req))
                .await
                .map_err(|_| {
                    UploadError::Timeout(format!(
                        "upload.saveBigFilePart part {part_index} timed out"
                    ))
                })?
                .map_err(Into::into)
        } else {
            let req = tl_fns::upload::SaveFilePart {
                file_id: plan.file_id,
                file_part: part_index as i32,
                bytes: slice.clone(),
            };
            tokio::time::timeout(UPLOAD_PART_RPC_TIMEOUT, rpc.invoke(&req))
                .await
                .map_err(|_| {
                    UploadError::Timeout(format!("upload.saveFilePart part {part_index} timed out"))
                })?
                .map_err(Into::into)
        };

        match rpc_res {
            Ok(true) => return Ok(()),
            Ok(false) => {
                if attempt >= cfg.max_retries {
                    return Err(UploadError::Rpc(format!(
                        "server rejected upload part {part_index}"
                    )));
                }
                attempt += 1;
            }
            Err(err) => {
                if attempt >= cfg.max_retries {
                    return Err(UploadError::Rpc(err.to_string()));
                }
                if let Some(wait) = flood_wait_duration(&err, cfg.max_flood_wait) {
                    debug!(
                        part = part_index,
                        wait_ms = wait.as_millis() as u64,
                        "retrying upload part after FLOOD_WAIT"
                    );
                    sleep_cancellable(wait, cancel).await?;
                    attempt += 1;
                    continue;
                }
                if matches!(&err, InvocationError::Rpc(r) if r.is_temporary())
                    || missing_part_from_error(&err).is_some()
                {
                    attempt += 1;
                    continue;
                }
                return Err(UploadError::Rpc(err.to_string()));
            }
        }
    }
}

/// Uploads `bytes` over `upload.saveFilePart` (for `<= 10 MiB`) or `upload.saveBigFilePart`
/// (for `> 10 MiB`) with tdesktop part sizing, concurrent in-flight sessions, incremental MD5
/// checksum for small files, cooperative cancellation, and `FLOOD_WAIT` / `FILE_PART_X_MISSING`
/// retries.
pub async fn upload_file_bytes_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    file_name: impl Into<String>,
    bytes: Arc<Vec<u8>>,
    cfg: LiveUploadConfig,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(UploadProgress),
) -> Result<UploadedFileResult, UploadError> {
    if cancel.is_cancelled() {
        return Err(UploadError::Cancelled);
    }

    let file_id = cfg.explicit_file_id.unwrap_or_else(generate_random_file_id);
    let plan = UploadPlan::with_file_id(
        file_id,
        file_name,
        bytes.len() as u64,
        cfg.to_media_config(),
    )
    .map_err(|e| UploadError::InvalidFile(e.to_string()))?;

    // Compute MD5 incrementally across parts in sequential order for small files (`file_upload.cpp:665`).
    let md5_checksum = if plan.is_big {
        String::new()
    } else {
        let mut hasher = Md5Hasher::new();
        for part in plan.parts() {
            let start = part.offset as usize;
            let end = start + part.length as usize;
            hasher.update(&bytes[start..end]);
        }
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(32);
        for b in digest {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
        }
        hex
    };

    let mut pending: VecDeque<u32> = (0..plan.total_parts).collect();
    let mut retries_per_part: HashMap<u32, usize> = HashMap::new();
    let mut acked_parts: BTreeSet<u32> = BTreeSet::new();
    let mut in_flight_parts: BTreeSet<u32> = BTreeSet::new();

    let max_sessions = cfg.max_sessions.clamp(1, MAX_UPLOAD_SESSIONS);
    let mut balancer = UploadSessionBalancer::new(max_sessions);
    let started_at = Instant::now();
    let max_concurrent_parts =
        ((MAX_UPLOAD_PER_SESSION / (plan.part_size as usize).max(1)).max(1) * max_sessions).min(32);

    struct PartOutcome {
        part_index: u32,
        part_len: usize,
        session_index: usize,
        queued_before: usize,
        sent_at_ms: u64,
        result: Result<bool, InvocationError>,
    }

    let mut join_set: JoinSet<PartOutcome> = JoinSet::new();
    let mut uploaded_bytes: u64 = 0;

    on_progress(UploadProgress {
        uploaded: 0,
        total: plan.file_size,
    });

    while !pending.is_empty() || !join_set.is_empty() {
        if cancel.is_cancelled() {
            join_set.abort_all();
            return Err(UploadError::Cancelled);
        }

        // Dispatch as many parts as the multi-session balancer and per-session byte budget allow.
        let mut used_sessions = BTreeSet::new();
        while !pending.is_empty() && join_set.len() < max_concurrent_parts {
            let Some(&next_idx) = pending.front() else {
                break;
            };
            let offset = (u64::from(next_idx) * u64::from(plan.part_size)) as usize;
            let end = (offset + plan.part_size as usize).min(bytes.len());
            let part_len = end - offset;
            let now_ms = started_at.elapsed().as_millis() as u64;

            let Some(session_index) = balancer.choose_session(part_len, &used_sessions, now_ms)
            else {
                break;
            };
            let _ = pending.pop_front();
            if in_flight_parts.contains(&next_idx) {
                continue;
            }
            in_flight_parts.insert(next_idx);
            let queued_before = balancer.on_part_sent(session_index, part_len);
            if queued_before + part_len >= MAX_UPLOAD_PER_SESSION {
                used_sessions.insert(session_index);
            }

            let rpc_clone = rpc.clone();
            let bytes_clone = Arc::clone(&bytes);
            let file_id = plan.file_id;
            let total_parts = plan.total_parts;
            let is_big = plan.is_big;

            join_set.spawn(async move {
                let slice = bytes_clone[offset..end].to_vec();
                let res: Result<bool, InvocationError> = if is_big {
                    let req = tl_fns::upload::SaveBigFilePart {
                        file_id,
                        file_part: next_idx as i32,
                        file_total_parts: total_parts as i32,
                        bytes: slice,
                    };
                    match tokio::time::timeout(UPLOAD_PART_RPC_TIMEOUT, rpc_clone.invoke(&req))
                        .await
                    {
                        Ok(inner) => inner.map_err(Into::into),
                        Err(_) => Err(InvocationError::Transport(format!(
                            "upload.saveBigFilePart part {next_idx} timed out"
                        ))),
                    }
                } else {
                    let req = tl_fns::upload::SaveFilePart {
                        file_id,
                        file_part: next_idx as i32,
                        bytes: slice,
                    };
                    match tokio::time::timeout(UPLOAD_PART_RPC_TIMEOUT, rpc_clone.invoke(&req))
                        .await
                    {
                        Ok(inner) => inner.map_err(Into::into),
                        Err(_) => Err(InvocationError::Transport(format!(
                            "upload.saveFilePart part {next_idx} timed out"
                        ))),
                    }
                };
                PartOutcome {
                    part_index: next_idx,
                    part_len,
                    session_index,
                    queued_before,
                    sent_at_ms: now_ms,
                    result: res,
                }
            });
        }

        let Some(joined) = join_set.join_next().await else {
            break;
        };

        if cancel.is_cancelled() {
            join_set.abort_all();
            return Err(UploadError::Cancelled);
        }

        let outcome = joined.map_err(|e| UploadError::Rpc(format!("upload worker failed: {e}")))?;
        in_flight_parts.remove(&outcome.part_index);
        let now_ms = started_at.elapsed().as_millis() as u64;
        let _ = balancer.on_part_finished(
            outcome.session_index,
            outcome.part_len,
            outcome.queued_before,
            outcome.sent_at_ms,
            now_ms,
        );

        match outcome.result {
            Ok(true) => {
                if acked_parts.insert(outcome.part_index) {
                    uploaded_bytes = uploaded_bytes
                        .saturating_add(outcome.part_len as u64)
                        .min(plan.file_size);
                    on_progress(UploadProgress {
                        uploaded: uploaded_bytes,
                        total: plan.file_size,
                    });
                }
            }
            Ok(false) => {
                let count = retries_per_part.entry(outcome.part_index).or_insert(0);
                if *count >= cfg.max_retries {
                    join_set.abort_all();
                    return Err(UploadError::Rpc(format!(
                        "server rejected upload part {}",
                        outcome.part_index
                    )));
                }
                *count += 1;
                pending.push_front(outcome.part_index);
            }
            Err(err) => {
                if let Some(missing_part) = missing_part_from_error(&err)
                    && missing_part < plan.total_parts
                {
                    let count = retries_per_part.entry(missing_part).or_insert(0);
                    if *count >= cfg.max_retries {
                        join_set.abort_all();
                        return Err(UploadError::Rpc(err.to_string()));
                    }
                    *count += 1;
                    if acked_parts.remove(&missing_part) {
                        let mp_offset = u64::from(missing_part) * u64::from(plan.part_size);
                        let mp_len = plan
                            .file_size
                            .saturating_sub(mp_offset)
                            .min(u64::from(plan.part_size));
                        uploaded_bytes = uploaded_bytes.saturating_sub(mp_len);
                    }
                    if !in_flight_parts.contains(&missing_part) && !pending.contains(&missing_part)
                    {
                        pending.push_front(missing_part);
                    }
                    if missing_part != outcome.part_index
                        && !acked_parts.contains(&outcome.part_index)
                        && !pending.contains(&outcome.part_index)
                    {
                        pending.push_front(outcome.part_index);
                    }
                    continue;
                }
                if let Some(wait) = flood_wait_duration(&err, cfg.max_flood_wait) {
                    let count = retries_per_part.entry(outcome.part_index).or_insert(0);
                    if *count >= cfg.max_retries {
                        join_set.abort_all();
                        return Err(UploadError::Rpc(err.to_string()));
                    }
                    *count += 1;
                    debug!(
                        part = outcome.part_index,
                        wait_ms = wait.as_millis() as u64,
                        "upload part hit FLOOD_WAIT; backing off"
                    );
                    if let Err(cancel_err) = sleep_cancellable(wait, cancel).await {
                        join_set.abort_all();
                        return Err(cancel_err);
                    }
                    pending.push_front(outcome.part_index);
                    continue;
                }
                if matches!(&err, InvocationError::Rpc(r) if r.is_temporary()) {
                    let count = retries_per_part.entry(outcome.part_index).or_insert(0);
                    if *count < cfg.max_retries {
                        *count += 1;
                        pending.push_front(outcome.part_index);
                        continue;
                    }
                }
                join_set.abort_all();
                return Err(UploadError::Rpc(err.to_string()));
            }
        }
    }

    let input_file = if plan.is_big {
        tl_enums::InputFile::Big(tl_types::InputFileBig {
            id: plan.file_id,
            parts: plan.total_parts as i32,
            name: plan.filename.clone(),
        })
    } else {
        tl_enums::InputFile::InputFile(tl_types::InputFile {
            id: plan.file_id,
            parts: plan.total_parts as i32,
            name: plan.filename.clone(),
            md5_checksum,
        })
    };

    Ok(UploadedFileResult {
        input_file,
        plan,
        bytes,
    })
}

/// Invokes a post-upload RPC (`messages.sendMedia`, `messages.uploadMedia`,
/// `messages.sendMultiMedia`, `stories.sendStory`) with automatic retry on `FLOOD_WAIT_X`
/// and `FILE_PART_X_MISSING` (re-uploading the missing part from `uploaded_files`).
pub(crate) async fn invoke_with_upload_retry<C, R>(
    rpc: &C,
    req: &R,
    uploaded_files: &[&UploadedFileResult],
    cfg: LiveUploadConfig,
    cancel: &CancelToken,
) -> Result<R::Return, String>
where
    C: RpcClient + Clone + 'static,
    R: RemoteCall + Send + Sync,
{
    let mut attempt = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err("upload cancelled".to_owned());
        }
        let res: Result<R::Return, InvocationError> =
            tokio::time::timeout(UPLOAD_SEND_RPC_TIMEOUT, rpc.invoke(req))
                .await
                .map_err(|_| {
                    format!(
                        "request timed out after {}s",
                        UPLOAD_SEND_RPC_TIMEOUT.as_secs()
                    )
                })?
                .map_err(Into::into);
        match res {
            Ok(val) => return Ok(val),
            Err(err) => {
                if attempt >= cfg.max_retries {
                    return Err(err.to_string());
                }
                if let Some(wait) = flood_wait_duration(&err, cfg.max_flood_wait) {
                    sleep_cancellable(wait, cancel)
                        .await
                        .map_err(|e| e.to_string())?;
                    attempt += 1;
                    continue;
                }
                if let Some(missing_part) = missing_part_from_error(&err) {
                    let mut reuploaded = false;
                    for uploaded in uploaded_files {
                        if missing_part < uploaded.plan.total_parts {
                            upload_single_part_async(
                                rpc,
                                &uploaded.plan,
                                &uploaded.bytes,
                                missing_part,
                                cfg,
                                cancel,
                            )
                            .await
                            .map_err(|e| e.to_string())?;
                            reuploaded = true;
                        }
                    }
                    if reuploaded {
                        attempt += 1;
                        continue;
                    }
                }
                return Err(err.to_string());
            }
        }
    }
}

// ============================================================================
// Media attribute and InputMedia builders (`api_media.cpp`, `api_sending.cpp`)
// ============================================================================

struct PreparedUploadMedia {
    input_media: tl_enums::InputMedia,
    uploaded_files: Vec<UploadedFileResult>,
}

async fn upload_outgoing_media_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    peer: &tl_enums::InputPeer,
    media: OutgoingMedia,
    base_cfg: LiveUploadConfig,
    cancel: &CancelToken,
    mut on_bytes_delta: impl FnMut(u64),
) -> Result<PreparedUploadMedia, UploadError>
where
    C::Error: std::fmt::Display,
{
    let mut track_upload = |prev: &mut u64, progress: UploadProgress| {
        if progress.uploaded > *prev {
            let delta = progress.uploaded - *prev;
            *prev = progress.uploaded;
            on_bytes_delta(delta);
        }
    };

    match media {
        OutgoingMedia::Photo {
            bytes,
            file_name,
            spoiler,
            ttl_seconds,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = true;
            let mut prev = 0u64;
            let uploaded = upload_file_bytes_async(
                rpc,
                if file_name.is_empty() {
                    "photo.jpg".to_owned()
                } else {
                    file_name
                },
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            let input_media =
                tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
                    spoiler,
                    live_photo: false,
                    file: uploaded.input_file.clone(),
                    stickers: None,
                    ttl_seconds,
                    video: None,
                });
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files: vec![uploaded],
            })
        }
        OutgoingMedia::Document {
            bytes,
            file_name,
            mime_type,
            thumb_bytes,
            force_file,
            spoiler,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = false;
            let mut uploaded_files = Vec::new();
            let thumb = if let Some(tb) = thumb_bytes
                && !tb.is_empty()
            {
                let mut tcfg = base_cfg;
                tcfg.is_photo = true;
                tcfg.explicit_file_id = None;
                let mut prev = 0u64;
                let res =
                    upload_file_bytes_async(rpc, "thumb.jpg", Arc::new(tb), tcfg, cancel, |p| {
                        track_upload(&mut prev, p)
                    })
                    .await?;
                let f = res.input_file.clone();
                uploaded_files.push(res);
                Some(f)
            } else {
                None
            };
            let effective_name = if file_name.is_empty() {
                "file.bin".to_owned()
            } else {
                file_name
            };
            let mut prev = 0u64;
            let main_upload = upload_file_bytes_async(
                rpc,
                effective_name.clone(),
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            let effective_mime = if mime_type.is_empty() {
                guess_mime_from_filename(&effective_name).to_owned()
            } else {
                mime_type
            };
            let attributes = vec![tl_enums::DocumentAttribute::Filename(
                tl_types::DocumentAttributeFilename {
                    file_name: effective_name,
                },
            )];
            let input_media =
                tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                    nosound_video: false,
                    force_file,
                    spoiler,
                    file: main_upload.input_file.clone(),
                    thumb,
                    mime_type: effective_mime,
                    attributes,
                    stickers: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                });
            uploaded_files.push(main_upload);
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files,
            })
        }
        OutgoingMedia::Video {
            bytes,
            file_name,
            mime_type,
            width,
            height,
            duration_secs,
            supports_streaming,
            nosound,
            round_message,
            thumb_bytes,
            video_cover_bytes,
            spoiler,
            ttl_seconds,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = false;
            let mut uploaded_files = Vec::new();
            let thumb = if let Some(tb) = thumb_bytes
                && !tb.is_empty()
            {
                let mut tcfg = base_cfg;
                tcfg.is_photo = true;
                tcfg.explicit_file_id = None;
                let mut prev = 0u64;
                let res =
                    upload_file_bytes_async(rpc, "thumb.jpg", Arc::new(tb), tcfg, cancel, |p| {
                        track_upload(&mut prev, p)
                    })
                    .await?;
                let f = res.input_file.clone();
                uploaded_files.push(res);
                Some(f)
            } else {
                None
            };
            let video_cover = if let Some(cover_bytes) = video_cover_bytes
                && !cover_bytes.is_empty()
            {
                let mut ccfg = base_cfg;
                ccfg.is_photo = true;
                ccfg.explicit_file_id = None;
                let mut prev = 0u64;
                let cover_res = upload_file_bytes_async(
                    rpc,
                    "cover.jpg",
                    Arc::new(cover_bytes),
                    ccfg,
                    cancel,
                    |p| track_upload(&mut prev, p),
                )
                .await?;
                let cover_input_file = cover_res.input_file.clone();
                uploaded_files.push(cover_res);
                let upload_cover_req = tl_fns::messages::UploadMedia {
                    business_connection_id: None,
                    peer: peer.clone(),
                    media: tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
                        spoiler: false,
                        live_photo: false,
                        file: cover_input_file,
                        stickers: None,
                        ttl_seconds: None,
                        video: None,
                    }),
                };
                let uploaded_cover_media =
                    with_rpc_timeout(UPLOAD_PART_RPC_TIMEOUT, rpc.invoke(&upload_cover_req))
                        .await
                        .map_err(UploadError::Rpc)?;
                match uploaded_cover_media {
                    tl_enums::MessageMedia::Photo(p) => match p.photo {
                        Some(tl_enums::Photo::Photo(photo)) => {
                            Some(tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                                id: photo.id,
                                access_hash: photo.access_hash,
                                file_reference: photo.file_reference,
                            }))
                        }
                        _ => {
                            return Err(UploadError::Rpc(
                                "messages.uploadMedia returned empty video cover photo".to_owned(),
                            ));
                        }
                    },
                    other => {
                        return Err(UploadError::Rpc(format!(
                            "messages.uploadMedia returned unexpected video cover media: {other:?}"
                        )));
                    }
                }
            } else {
                None
            };
            let effective_name = if file_name.is_empty() {
                "video.mp4".to_owned()
            } else {
                file_name
            };
            let mut prev = 0u64;
            let main_upload = upload_file_bytes_async(
                rpc,
                effective_name.clone(),
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            let effective_mime = if mime_type.is_empty() {
                "video/mp4".to_owned()
            } else {
                mime_type
            };
            let attributes = vec![
                tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
                    file_name: effective_name,
                }),
                tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
                    round_message,
                    supports_streaming,
                    nosound,
                    duration: duration_secs.max(0.0),
                    w: width.max(1),
                    h: height.max(1),
                    preload_prefix_size: None,
                    video_start_ts: None,
                    video_codec: None,
                }),
            ];
            let input_media =
                tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                    nosound_video: nosound,
                    force_file: false,
                    spoiler,
                    file: main_upload.input_file.clone(),
                    thumb,
                    mime_type: effective_mime,
                    attributes,
                    stickers: None,
                    video_cover,
                    video_timestamp: None,
                    ttl_seconds,
                });
            uploaded_files.push(main_upload);
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files,
            })
        }
        OutgoingMedia::Voice {
            bytes,
            file_name,
            duration_secs,
            waveform_5bit,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = false;
            let effective_name = if file_name.is_empty() {
                "voice.ogg".to_owned()
            } else {
                file_name
            };
            let mut prev = 0u64;
            let main_upload = upload_file_bytes_async(
                rpc,
                effective_name.clone(),
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            // `FileLoadTask::process` for a voice note keeps only the audio attribute
            // (`attributes.resize(1)`): no file name.
            let attributes = vec![tl_enums::DocumentAttribute::Audio(
                tl_types::DocumentAttributeAudio {
                    voice: true,
                    duration: duration_secs.max(0),
                    title: None,
                    performer: None,
                    waveform: (!waveform_5bit.is_empty()).then_some(waveform_5bit),
                },
            )];
            let input_media =
                tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                    nosound_video: false,
                    force_file: false,
                    spoiler: false,
                    file: main_upload.input_file.clone(),
                    thumb: None,
                    mime_type: "audio/ogg".to_owned(),
                    attributes,
                    stickers: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                });
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files: vec![main_upload],
            })
        }
        OutgoingMedia::Audio {
            bytes,
            file_name,
            mime_type,
            duration_secs,
            title,
            performer,
            thumb_bytes,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = false;
            let mut uploaded_files = Vec::new();
            let thumb = if let Some(tb) = thumb_bytes
                && !tb.is_empty()
            {
                let mut tcfg = base_cfg;
                tcfg.is_photo = true;
                tcfg.explicit_file_id = None;
                let mut prev = 0u64;
                let res =
                    upload_file_bytes_async(rpc, "thumb.jpg", Arc::new(tb), tcfg, cancel, |p| {
                        track_upload(&mut prev, p)
                    })
                    .await?;
                let f = res.input_file.clone();
                uploaded_files.push(res);
                Some(f)
            } else {
                None
            };
            let effective_name = if file_name.is_empty() {
                "audio.mp3".to_owned()
            } else {
                file_name
            };
            let mut prev = 0u64;
            let main_upload = upload_file_bytes_async(
                rpc,
                effective_name.clone(),
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            let effective_mime = if mime_type.is_empty() {
                "audio/mpeg".to_owned()
            } else {
                mime_type
            };
            let attributes = vec![
                tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
                    file_name: effective_name,
                }),
                tl_enums::DocumentAttribute::Audio(tl_types::DocumentAttributeAudio {
                    voice: false,
                    duration: duration_secs.max(0),
                    title,
                    performer,
                    waveform: None,
                }),
            ];
            let input_media =
                tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                    nosound_video: false,
                    force_file: false,
                    spoiler: false,
                    file: main_upload.input_file.clone(),
                    thumb,
                    mime_type: effective_mime,
                    attributes,
                    stickers: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                });
            uploaded_files.push(main_upload);
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files,
            })
        }
        OutgoingMedia::Sticker {
            bytes,
            file_name,
            mime_type,
            width,
            height,
            alt_emoji,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = false;
            let effective_name = if file_name.is_empty() {
                "sticker.webp".to_owned()
            } else {
                file_name
            };
            let mut prev = 0u64;
            let main_upload = upload_file_bytes_async(
                rpc,
                effective_name.clone(),
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            let effective_mime = if mime_type.is_empty() {
                "image/webp".to_owned()
            } else {
                mime_type
            };
            let attributes = vec![
                tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
                    file_name: effective_name,
                }),
                tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                    w: width.max(1),
                    h: height.max(1),
                }),
                tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                    mask: false,
                    alt: alt_emoji,
                    stickerset: tl_enums::InputStickerSet::Empty,
                    mask_coords: None,
                }),
            ];
            let input_media =
                tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                    nosound_video: false,
                    force_file: false,
                    spoiler: false,
                    file: main_upload.input_file.clone(),
                    thumb: None,
                    mime_type: effective_mime,
                    attributes,
                    stickers: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                });
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files: vec![main_upload],
            })
        }
        OutgoingMedia::Gif {
            bytes,
            file_name,
            mime_type,
            width,
            height,
            duration_secs,
            thumb_bytes,
            spoiler,
        } => {
            let mut cfg = base_cfg;
            cfg.is_photo = false;
            let mut uploaded_files = Vec::new();
            let thumb = if let Some(tb) = thumb_bytes
                && !tb.is_empty()
            {
                let mut tcfg = base_cfg;
                tcfg.is_photo = true;
                tcfg.explicit_file_id = None;
                let mut prev = 0u64;
                let res =
                    upload_file_bytes_async(rpc, "thumb.jpg", Arc::new(tb), tcfg, cancel, |p| {
                        track_upload(&mut prev, p)
                    })
                    .await?;
                let f = res.input_file.clone();
                uploaded_files.push(res);
                Some(f)
            } else {
                None
            };
            let effective_name = if file_name.is_empty() {
                "animation.mp4".to_owned()
            } else {
                file_name
            };
            let mut prev = 0u64;
            let main_upload = upload_file_bytes_async(
                rpc,
                effective_name.clone(),
                Arc::new(bytes),
                cfg,
                cancel,
                |p| track_upload(&mut prev, p),
            )
            .await?;
            let effective_mime = if mime_type.is_empty() {
                "video/mp4".to_owned()
            } else {
                mime_type
            };
            let mut attributes = vec![tl_enums::DocumentAttribute::Filename(
                tl_types::DocumentAttributeFilename {
                    file_name: effective_name,
                },
            )];
            if effective_mime == "image/gif" {
                attributes.push(tl_enums::DocumentAttribute::ImageSize(
                    tl_types::DocumentAttributeImageSize {
                        w: width.max(1),
                        h: height.max(1),
                    },
                ));
            } else {
                attributes.push(tl_enums::DocumentAttribute::Video(
                    tl_types::DocumentAttributeVideo {
                        round_message: false,
                        supports_streaming: true,
                        nosound: true,
                        duration: duration_secs.unwrap_or(0.0).max(0.0),
                        w: width.max(1),
                        h: height.max(1),
                        preload_prefix_size: None,
                        video_start_ts: None,
                        video_codec: None,
                    },
                ));
            }
            attributes.push(tl_enums::DocumentAttribute::Animated);
            let input_media =
                tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                    nosound_video: true,
                    force_file: false,
                    spoiler,
                    file: main_upload.input_file.clone(),
                    thumb,
                    mime_type: effective_mime,
                    attributes,
                    stickers: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                });
            uploaded_files.push(main_upload);
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files,
            })
        }
        OutgoingMedia::Location(loc) => {
            let geo_point = tl_enums::InputGeoPoint::InputGeoPoint(tl_types::InputGeoPoint {
                lat: loc.lat,
                long: loc.lon,
                accuracy_radius: None,
            });
            let input_media = if let Some(title) = loc.venue_title {
                tl_enums::InputMedia::Venue(tl_types::InputMediaVenue {
                    geo_point,
                    title,
                    address: loc.venue_address.unwrap_or_default(),
                    provider: String::new(),
                    venue_id: String::new(),
                    venue_type: String::new(),
                })
            } else if let Some(period) = loc.live_period_secs {
                tl_enums::InputMedia::GeoLive(tl_types::InputMediaGeoLive {
                    stopped: false,
                    geo_point,
                    heading: None,
                    period: Some(period as i32),
                    proximity_notification_radius: None,
                })
            } else {
                tl_enums::InputMedia::GeoPoint(tl_types::InputMediaGeoPoint { geo_point })
            };
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files: Vec::new(),
            })
        }
        OutgoingMedia::Contact(c) => {
            let input_media = tl_enums::InputMedia::Contact(tl_types::InputMediaContact {
                phone_number: c.phone_number,
                first_name: c.first_name,
                last_name: c.last_name,
                vcard: String::new(),
            });
            Ok(PreparedUploadMedia {
                input_media,
                uploaded_files: Vec::new(),
            })
        }
    }
}

/// Converts a `messages.uploadMedia` response (`MessageMedia`) into a reusable `InputMedia`
/// for `messages.sendMultiMedia`, matching `ApiWrap::uploadAlbumMedia` (`apiwrap.cpp:5283-5342`).
fn uploaded_message_media_to_input_media(
    media: tl_enums::MessageMedia,
    spoiler: bool,
) -> Result<tl_enums::InputMedia, String> {
    match media {
        tl_enums::MessageMedia::Photo(p) => {
            let Some(tl_enums::Photo::Photo(photo)) = p.photo else {
                return Err("messages.uploadMedia returned empty photo".to_owned());
            };
            Ok(tl_enums::InputMedia::Photo(tl_types::InputMediaPhoto {
                spoiler: spoiler || p.spoiler,
                live_photo: false,
                id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                    id: photo.id,
                    access_hash: photo.access_hash,
                    file_reference: photo.file_reference,
                }),
                ttl_seconds: p.ttl_seconds,
                video: None,
            }))
        }
        tl_enums::MessageMedia::Document(d) => {
            let Some(tl_enums::Document::Document(doc)) = d.document else {
                return Err("messages.uploadMedia returned empty document".to_owned());
            };
            let video_cover = match d.video_cover {
                Some(tl_enums::Photo::Photo(cover)) => {
                    Some(tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                        id: cover.id,
                        access_hash: cover.access_hash,
                        file_reference: cover.file_reference,
                    }))
                }
                _ => None,
            };
            Ok(tl_enums::InputMedia::Document(
                tl_types::InputMediaDocument {
                    spoiler: spoiler || d.spoiler,
                    id: tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                        id: doc.id,
                        access_hash: doc.access_hash,
                        file_reference: doc.file_reference,
                    }),
                    video_cover,
                    video_timestamp: d.video_timestamp,
                    ttl_seconds: d.ttl_seconds,
                    query: None,
                },
            ))
        }
        other => Err(format!(
            "messages.uploadMedia returned unsupported media variant: {other:?}"
        )),
    }
}

fn guess_mime_from_filename(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("tgs") => "application/x-tgsticker",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

fn read_session_sync<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
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

fn resolve_input_peer_sync(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    let peer = read_session_sync(live, |s| input_peer_for(s, data_peer));
    if matches!(peer, tl_enums::InputPeer::Empty) {
        None
    } else {
        Some(peer)
    }
}

fn is_self_premium(live: &LiveSession) -> bool {
    read_session_sync(live, |s| {
        s.peer(s.self_peer_id())
            .and_then(|p| p.as_user())
            .is_some_and(tg_data::User::is_premium)
    })
}

fn effective_upload_config(live: &LiveSession) -> LiveUploadConfig {
    let mut cfg = upload_state_for(live)
        .lock()
        .map(|s| s.upload_config)
        .unwrap_or_default();
    if is_self_premium(live) {
        cfg.is_premium = true;
    }
    cfg
}

fn fallback_sent_history_message(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: DataMsgId,
    caption: &TextWithEntities,
    media: Option<HistoryMedia>,
    silent: bool,
    schedule_date: Option<i32>,
) -> HistoryMessage {
    let data_peer = to_data_peer(peer_id);
    read_session_sync(live, |s| {
        if msg_id.0 > 0
            && let Some(m) = s.message(DataFullMsgId::new(data_peer, msg_id))
        {
            return message_to_history_message(s, m);
        }
        let self_pid = s.self_peer_id();
        HistoryMessage {
            id: from_data_msg(msg_id),
            peer_id,
            from_id: from_data_peer(self_pid),
            sender_name: peer_display_title(s, self_pid),
            date_unix: now_unix_i32(),
            outgoing: true,
            is_post: false,
            text: caption.clone(),
            media,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: tg_app_core::history::backend::MessageExtra {
                scheduled_date: schedule_date,
                silent,
                ..Default::default()
            },
        }
    })
}

fn resolve_bytes_for_attachment(
    live: &LiveSession,
    key_or_path: &str,
    fallback: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if !key_or_path.is_empty() {
        if let Ok(state) = upload_state_for(live).lock()
            && let Some(staged) = state.staged_media_bytes.get(key_or_path)
            && !staged.is_empty()
        {
            return Ok(staged.clone());
        }
        let p = Path::new(key_or_path);
        if p.is_file() {
            return std::fs::read(p)
                .map_err(|e| format!("failed to read media file '{}': {e}", p.display()));
        }
    }
    if let Some(fb) = fallback
        && !fb.is_empty()
    {
        return Ok(fb.to_vec());
    }
    Err(format!(
        "no file bytes available for attachment '{key_or_path}'"
    ))
}

fn file_basename(path_or_name: &str, default_name: &str) -> String {
    if path_or_name.is_empty() {
        return default_name.to_owned();
    }
    Path::new(path_or_name)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(path_or_name)
        .to_owned()
}

// ============================================================================
// Public LiveSession upload, ToS, clipboard, and media-forward methods
// ============================================================================

impl LiveSession {
    /// Overrides the per-session [`LiveUploadConfig`] (e.g. concurrency or retry backoff cap).
    pub fn set_upload_config(&self, config: LiveUploadConfig) {
        if let Ok(mut guard) = upload_state_for(self).lock() {
            guard.upload_config = config;
        }
    }

    /// Returns the active per-session [`LiveUploadConfig`].
    #[must_use]
    pub fn upload_config(&self) -> LiveUploadConfig {
        effective_upload_config(self)
    }

    /// Stages in-memory file bytes keyed by attachment path/name for subsequent
    /// [`HistoryBackend::send_message_extended`] calls when bytes are not on disk.
    pub fn stage_media_bytes(&self, key: impl Into<String>, bytes: Vec<u8>) {
        if let Ok(mut guard) = upload_state_for(self).lock() {
            guard.staged_media_bytes.insert(key.into(), bytes);
        }
    }

    /// Sets the target peer used by [`MediaBackend::forward_item`] from the media overlay.
    pub fn set_media_forward_target(&self, peer_id: Option<PeerId>) {
        if let Ok(mut guard) = upload_state_for(self).lock() {
            guard.media_forward_target = peer_id;
        }
    }

    /// Uploads raw `bytes` using `upload.saveFilePart` / `upload.saveBigFilePart` and returns
    /// the resulting [`UploadedFileResult`].
    pub fn upload_file_bytes(
        &self,
        file_name: impl Into<String>,
        bytes: Vec<u8>,
        config: LiveUploadConfig,
        cancel: &CancelToken,
        on_progress: impl FnMut(UploadProgress) + Send,
    ) -> Result<UploadedFileResult, UploadError> {
        let rpc = self.runtime().rpc().clone();
        let name = file_name.into();
        let cancel_clone = cancel.clone();
        block_on_runtime(self.runtime().tokio_handle(), async move {
            upload_file_bytes_async(
                &rpc,
                name,
                Arc::new(bytes),
                config,
                &cancel_clone,
                on_progress,
            )
            .await
        })
    }

    /// Sends an existing Telegram document (sticker or GIF) via `messages.sendMedia`
    /// using [`tl_types::InputMediaDocument`] with its existing `id`, `access_hash`, and
    /// `file_reference` (matching `Api::SendExistingDocument` in `api_sending.cpp:692`).
    pub fn send_existing_document(
        &mut self,
        peer_id: PeerId,
        document: tl_enums::InputDocument,
        update_stickersets_order: bool,
        options: SendMediaOptions,
    ) -> Result<HistoryMessage, String> {
        let input_peer = resolve_input_peer_sync(self, peer_id)
            .ok_or_else(|| "Peer not found in session.".to_owned())?;
        let send_as = options
            .send_as
            .and_then(|pid| resolve_input_peer_sync(self, pid));
        let entities = read_session_sync(self, |s| ui_text_to_tl_entities(&options.caption, s));
        let reply_to = options
            .reply_to
            .map(|mid| make_input_reply_to(mid, options.reply_quote.clone()));

        let state_arc = upload_state_for(self);
        let random_id = {
            let mut guard = state_arc
                .lock()
                .map_err(|_| "upload state lock poisoned".to_owned())?;
            guard.next_random_id()
        };

        let media = tl_enums::InputMedia::Document(tl_types::InputMediaDocument {
            spoiler: false,
            id: document,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
            query: None,
        });

        let req = tl_fns::messages::SendMedia {
            silent: options.silent,
            background: false,
            clear_draft: options.clear_draft,
            noforwards: false,
            update_stickersets_order,
            invert_media: options.invert_media,
            allow_paid_floodskip: false,
            peer: input_peer,
            reply_to,
            media,
            message: options.caption.text.clone(),
            random_id,
            reply_markup: None,
            entities: (!entities.is_empty()).then_some(entities),
            schedule_date: options.schedule_date,
            schedule_repeat_period: None,
            send_as,
            quick_reply_shortcut: None,
            effect: options.effect_id.map(|id| id as i64),
            allow_paid_stars: None,
            suggested_post: None,
        };

        let rpc = self.runtime().rpc().clone();
        let updates = block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(
                UPLOAD_SEND_RPC_TIMEOUT,
                async move { rpc.invoke(&req).await },
            )
            .await
        })?;

        let new_ids = self.apply_updates_to_session(&updates);
        self.bridge_state().notify_ui();
        let msg_id = new_ids.last().copied().unwrap_or(DataMsgId(0));
        Ok(fallback_sent_history_message(
            self,
            peer_id,
            msg_id,
            &options.caption,
            None,
            options.silent,
            options.schedule_date,
        ))
    }

    /// Uploads and sends a single typed media attachment via `messages.sendMedia`, reporting
    /// progress via [`HistoryBackend::pending_media_uploads`].
    pub fn send_uploaded_media(
        &mut self,
        peer_id: PeerId,
        media: OutgoingMedia,
        options: SendMediaOptions,
    ) -> Result<HistoryMessage, String> {
        self.send_uploaded_media_with_cancel(peer_id, media, options, CancelToken::new())
    }

    /// Same as [`Self::send_uploaded_media`], accepting a caller-controlled [`CancelToken`].
    pub fn send_uploaded_media_with_cancel(
        &mut self,
        peer_id: PeerId,
        media: OutgoingMedia,
        options: SendMediaOptions,
        cancel: CancelToken,
    ) -> Result<HistoryMessage, String> {
        let send = self.prepare_media_send(peer_id, media, &options, cancel)?;
        let send_res = block_on_runtime(self.runtime().tokio_handle(), send.request);

        match send_res {
            Ok(updates) => {
                finish_tracked_upload(&send.state, send.upload_id, None);
                let new_ids = self.apply_updates_to_session(&updates);
                let msg_id = new_ids.last().copied().unwrap_or(DataMsgId(0));
                if let Some(bytes) = send.seed_bytes {
                    crate::sent_media_cache::seed(self, to_data_peer(peer_id), msg_id, &bytes);
                }
                Ok(fallback_sent_history_message(
                    self,
                    peer_id,
                    msg_id,
                    &options.caption,
                    None,
                    options.silent,
                    options.schedule_date,
                ))
            }
            Err(err) => {
                finish_tracked_upload(&send.state, send.upload_id, Some(&err));
                self.bridge_state().notify_ui();
                Err(err)
            }
        }
    }

    /// Sends one media attachment the way tdesktop does (`ApiWrap::sendFile` -> `FileLoadTask`
    /// -> `Uploader`): the local message with the clock is stored and returned at once, the
    /// upload and `messages.sendMedia` run on the runtime, and the next history drain turns the
    /// local message into the server one ([`HistoryLiveUpdate::LocalMessageSent`]), marks it
    /// failed, or removes it when the upload was cancelled (TGB-3033). `shown` is the media
    /// the pending bubble shows until then. Upload progress and cancel stay in
    /// [`HistoryBackend::pending_media_uploads`] / [`HistoryBackend::cancel_media_upload`].
    pub(crate) fn send_uploaded_media_queued(
        &self,
        peer_id: PeerId,
        media: OutgoingMedia,
        options: SendMediaOptions,
        shown: HistoryMedia,
    ) -> Result<HistoryMessage, String> {
        let send = self.prepare_media_send(peer_id, media, &options, CancelToken::new())?;
        let data_peer = to_data_peer(peer_id);
        let local_id = self.sends.next_local_id();
        let local_full = DataFullMsgId::new(data_peer, local_id);
        let caption = ui_text_to_data_text(&options.caption);
        let reply_to = options.reply_to;
        let silent = options.silent;
        let effect_id = options.effect_id;
        let random_id = send.random_id;
        self.sends.set_local_media(local_full, shown.clone());
        let stored = self.write_session(move |s| {
            let mut local = tg_data::Message::new_text(
                data_peer,
                local_id,
                Some(s.self_peer_id()),
                now_unix_i32(),
                caption,
            );
            local
                .flags
                .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
            local.flags.set(MessageFlags::SILENT, silent);
            local.effect_id = effect_id.map(tg_data::EffectId);
            local.reply_to = reply_to.map(|id| tg_data::ReplyInfo {
                reply_to_msg_id: Some(to_data_msg(id)),
                ..tg_data::ReplyInfo::default()
            });
            s.register_random_id(random_id, local_full);
            s.insert_message(local);
            s.message(local_full)
                .map(|m| message_to_history_message(s, m))
        });
        let Some(mut pending) = stored else {
            self.sends.take_local_media(local_full);
            finish_tracked_upload(&send.state, send.upload_id, Some("not stored"));
            return Err("sent message was not stored in session".to_owned());
        };
        pending.media = Some(shown.clone());

        let live = self.clone();
        let handle = self.runtime().tokio_handle().clone();
        // Taken here, on the caller's thread, so the queue order is the order of the sends.
        let mut in_flight = self.sends.begin_media_send();
        drop(self.runtime().tokio_handle().spawn_blocking(move || {
            let previous = in_flight.previous();
            let outcome = block_on_runtime(&handle, async move {
                previous.await;
                send.request.await
            });
            let cancelled = send.cancel.is_cancelled();
            finish_tracked_upload(
                &send.state,
                send.upload_id,
                outcome.as_ref().err().map(String::as_str),
            );
            let resolution = match outcome {
                Ok(updates) => {
                    let sent_id = live.apply_updates_to_session(&updates).last().copied();
                    if let (Some(id), Some(bytes)) = (sent_id, send.seed_bytes.as_deref()) {
                        crate::sent_media_cache::seed(&live, data_peer, id, bytes);
                    }
                    // A server message that did not replace the local one through
                    // `updateMessageID` still replaces it on screen.
                    live.write_session(move |s| {
                        s.unregister_random_id(random_id);
                        s.delete_messages(data_peer, &[local_id]);
                    });
                    live.sends.take_local_media(local_full);
                    let fallback = fallback_sent_history_message(
                        &live,
                        peer_id,
                        sent_id.unwrap_or(DataMsgId(0)),
                        &options.caption,
                        Some(shown),
                        silent,
                        options.schedule_date,
                    );
                    SendResolution {
                        peer: data_peer,
                        local_ids: vec![local_id],
                        outcome: Ok(SendMessageOutcome {
                            messages: Vec::new(),
                            updates: vec![updates],
                        }),
                        sent_ids: vec![sent_id],
                        effect_id,
                        fallback: sent_id.map(|_| fallback).into_iter().collect(),
                        cancelled: false,
                    }
                }
                Err(err) => {
                    live.write_session(move |s| {
                        s.unregister_random_id(random_id);
                        if cancelled {
                            // tdesktop destroys the item when its upload is cancelled.
                            s.delete_messages(data_peer, &[local_id]);
                        } else if let Some(msg) = s.message_mut(local_full) {
                            msg.flags.remove(MessageFlags::BEING_SENT);
                            msg.flags.insert(MessageFlags::SENDING_FAILED);
                        }
                    });
                    if cancelled {
                        live.sends.take_local_media(local_full);
                    }
                    SendResolution {
                        peer: data_peer,
                        local_ids: vec![local_id],
                        outcome: Err(err),
                        sent_ids: vec![None],
                        effect_id,
                        fallback: Vec::new(),
                        cancelled,
                    }
                }
            };
            live.sends.push_resolution(resolution);
            live.bridge_state().mark_views_changed();
            live.bridge_state().notify_ui();
        }));
        self.bridge_state().notify_ui();
        Ok(pending)
    }

    /// Waits until the media sends started without waiting have finished. `false` after
    /// `limit`. For tests only.
    #[doc(hidden)]
    pub fn wait_media_sends(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while self.sends.media_sends_in_flight() > 0 {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    /// Registers the upload for progress and cancel and builds the request future that
    /// uploads `media` and sends it with `messages.sendMedia`. Nothing is sent before the
    /// future runs.
    fn prepare_media_send(
        &self,
        peer_id: PeerId,
        media: OutgoingMedia,
        options: &SendMediaOptions,
        cancel: CancelToken,
    ) -> Result<PreparedMediaSend, String> {
        let input_peer = resolve_input_peer_sync(self, peer_id)
            .ok_or_else(|| "Peer not found in session.".to_owned())?;
        let send_as = options
            .send_as
            .and_then(|pid| resolve_input_peer_sync(self, pid));
        let entities = read_session_sync(self, |s| ui_text_to_tl_entities(&options.caption, s));
        let reply_to = options
            .reply_to
            .map(|mid| make_input_reply_to(mid, options.reply_quote.clone()));

        let state_arc = upload_state_for(self);
        let total_bytes = media.total_upload_bytes();
        let display_name = media.display_name();
        // TGB-2998: kept to seed the media cache under the document the server returns.
        let seed_bytes = crate::sent_media_cache::seedable_bytes(&media);
        let (upload_id, random_id) = {
            let mut guard = state_arc
                .lock()
                .map_err(|_| "upload state lock poisoned".to_owned())?;
            let uid = guard.next_upload_id;
            guard.next_upload_id = guard.next_upload_id.wrapping_add(1);
            let rid = guard.next_random_id();
            if total_bytes > 0 {
                guard.active_uploads.insert(
                    uid,
                    TrackedMediaUpload {
                        status: MediaUploadStatus {
                            upload_id: uid,
                            peer_id,
                            file_name: display_name,
                            uploaded_bytes: 0,
                            total_bytes,
                            failed: false,
                            cancelled: false,
                        },
                        cancel: cancel.clone(),
                    },
                );
            }
            (uid, rid)
        };
        if total_bytes > 0 {
            self.bridge_state().notify_ui();
        }

        let cfg = effective_upload_config(self);
        let rpc = self.runtime().rpc().clone();
        let bridge = Arc::clone(self.bridge_state());
        let state_for_progress = Arc::clone(&state_arc);
        let caption_text = options.caption.text.clone();
        let silent = options.silent;
        let schedule_date = options.schedule_date;
        let effect = options.effect_id.map(|id| id as i64);
        let invert_media = options.invert_media;
        let clear_draft = options.clear_draft;
        let request_cancel = cancel.clone();

        let request = Box::pin(async move {
            let cancel = request_cancel;
            let prepared =
                upload_outgoing_media_async(&rpc, &input_peer, media, cfg, &cancel, |delta| {
                    if let Ok(mut guard) = state_for_progress.lock()
                        && let Some(entry) = guard.active_uploads.get_mut(&upload_id)
                    {
                        entry.status.uploaded_bytes = entry
                            .status
                            .uploaded_bytes
                            .saturating_add(delta)
                            .min(entry.status.total_bytes);
                    }
                    bridge.notify_ui();
                })
                .await
                .map_err(|e| e.to_string())?;

            let req = tl_fns::messages::SendMedia {
                silent,
                background: false,
                clear_draft,
                noforwards: false,
                update_stickersets_order: false,
                invert_media,
                allow_paid_floodskip: false,
                peer: input_peer,
                reply_to,
                media: prepared.input_media,
                message: caption_text,
                random_id,
                reply_markup: None,
                entities: (!entities.is_empty()).then_some(entities),
                schedule_date,
                schedule_repeat_period: None,
                send_as,
                quick_reply_shortcut: None,
                effect,
                allow_paid_stars: None,
                suggested_post: None,
            };
            let refs: Vec<&UploadedFileResult> = prepared.uploaded_files.iter().collect();
            invoke_with_upload_retry(&rpc, &req, &refs, cfg, &cancel).await
        });
        Ok(PreparedMediaSend {
            state: state_arc,
            upload_id,
            random_id,
            cancel,
            seed_bytes,
            request,
        })
    }

    /// Uploads a grouped album via `upload.save*FilePart` -> `messages.uploadMedia` ->
    /// `messages.sendMultiMedia`, matching `ApiWrap::uploadAlbumMedia` / `sendAlbumIfReady`.
    pub fn send_album(
        &mut self,
        peer_id: PeerId,
        items: Vec<AlbumUploadItem>,
        options: SendMediaOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        self.send_album_with_cancel(peer_id, items, options, CancelToken::new())
    }

    /// Same as [`Self::send_album`], accepting a caller-controlled [`CancelToken`].
    pub fn send_album_with_cancel(
        &mut self,
        peer_id: PeerId,
        mut items: Vec<AlbumUploadItem>,
        options: SendMediaOptions,
        cancel: CancelToken,
    ) -> Result<Vec<HistoryMessage>, String> {
        if items.is_empty() {
            return Err("Album must contain at least one item.".to_owned());
        }
        // Single-item album falls back to `messages.sendMedia`, matching `apiwrap.cpp:5648`.
        if items.len() == 1 {
            let item = items.remove(0);
            let mut single_opts = options;
            if let Some(cap) = item.caption {
                single_opts.caption = cap;
            }
            let sent =
                self.send_uploaded_media_with_cancel(peer_id, item.media, single_opts, cancel)?;
            return Ok(vec![sent]);
        }

        let input_peer = resolve_input_peer_sync(self, peer_id)
            .ok_or_else(|| "Peer not found in session.".to_owned())?;
        let send_as = options
            .send_as
            .and_then(|pid| resolve_input_peer_sync(self, pid));
        let reply_to = options
            .reply_to
            .map(|mid| make_input_reply_to(mid, options.reply_quote.clone()));

        let has_per_item_caption = items.iter().any(|i| i.caption.is_some());
        let prepared_captions: Vec<(String, Vec<tl_enums::MessageEntity>)> =
            read_session_sync(self, |s| {
                items
                    .iter()
                    .enumerate()
                    .map(|(idx, item)| {
                        let text_ref = if let Some(ref cap) = item.caption {
                            cap
                        } else if !has_per_item_caption && idx == 0 {
                            &options.caption
                        } else {
                            return (String::new(), Vec::new());
                        };
                        (text_ref.text.clone(), ui_text_to_tl_entities(text_ref, s))
                    })
                    .collect()
            });

        let total_bytes: u64 = items.iter().map(|i| i.media.total_upload_bytes()).sum();
        let state_arc = upload_state_for(self);
        let (upload_id, random_ids) = {
            let mut guard = state_arc
                .lock()
                .map_err(|_| "upload state lock poisoned".to_owned())?;
            let uid = guard.next_upload_id;
            guard.next_upload_id = guard.next_upload_id.wrapping_add(1);
            let rids: Vec<i64> = items.iter().map(|_| guard.next_random_id()).collect();
            if total_bytes > 0 {
                guard.active_uploads.insert(
                    uid,
                    TrackedMediaUpload {
                        status: MediaUploadStatus {
                            upload_id: uid,
                            peer_id,
                            file_name: format!("Album ({} items)", items.len()),
                            uploaded_bytes: 0,
                            total_bytes,
                            failed: false,
                            cancelled: false,
                        },
                        cancel: cancel.clone(),
                    },
                );
            }
            (uid, rids)
        };
        if total_bytes > 0 {
            self.bridge_state().notify_ui();
        }

        let cfg = effective_upload_config(self);
        let rpc = self.runtime().rpc().clone();
        let bridge = Arc::clone(self.bridge_state());
        let state_for_progress = Arc::clone(&state_arc);
        let silent = options.silent;
        let schedule_date = options.schedule_date;
        let effect = options.effect_id.map(|id| id as i64);
        let invert_media = options.invert_media;
        let clear_draft = options.clear_draft;

        let send_res: Result<tl_enums::Updates, String> =
            block_on_runtime(self.runtime().tokio_handle(), async move {
                let mut multi_media = Vec::with_capacity(items.len());

                for ((item, (cap_text, cap_entities)), random_id) in
                    items.into_iter().zip(prepared_captions).zip(random_ids)
                {
                    if cancel.is_cancelled() {
                        return Err("upload cancelled".to_owned());
                    }
                    let spoiler = item.media.is_spoiler();
                    let prepared = upload_outgoing_media_async(
                        &rpc,
                        &input_peer,
                        item.media,
                        cfg,
                        &cancel,
                        |delta| {
                            if let Ok(mut guard) = state_for_progress.lock()
                                && let Some(entry) = guard.active_uploads.get_mut(&upload_id)
                            {
                                entry.status.uploaded_bytes = entry
                                    .status
                                    .uploaded_bytes
                                    .saturating_add(delta)
                                    .min(entry.status.total_bytes);
                            }
                            bridge.notify_ui();
                        },
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                    let upload_req = tl_fns::messages::UploadMedia {
                        business_connection_id: None,
                        peer: input_peer.clone(),
                        media: prepared.input_media,
                    };
                    let refs: Vec<&UploadedFileResult> = prepared.uploaded_files.iter().collect();
                    let uploaded_msg_media =
                        invoke_with_upload_retry(&rpc, &upload_req, &refs, cfg, &cancel).await?;
                    let reusable_media =
                        uploaded_message_media_to_input_media(uploaded_msg_media, spoiler)?;

                    multi_media.push(tl_enums::InputSingleMedia::InputSingleMedia(
                        tl_types::InputSingleMedia {
                            media: reusable_media,
                            random_id,
                            message: cap_text,
                            entities: (!cap_entities.is_empty()).then_some(cap_entities),
                        },
                    ));
                }

                let send_multi = tl_fns::messages::SendMultiMedia {
                    silent,
                    background: false,
                    clear_draft,
                    noforwards: false,
                    update_stickersets_order: false,
                    invert_media,
                    allow_paid_floodskip: false,
                    peer: input_peer,
                    reply_to,
                    multi_media,
                    schedule_date,
                    send_as,
                    quick_reply_shortcut: None,
                    effect,
                    allow_paid_stars: None,
                };
                invoke_with_upload_retry(&rpc, &send_multi, &[], cfg, &cancel).await
            });

        match send_res {
            Ok(updates) => {
                if let Ok(mut guard) = state_arc.lock() {
                    guard.active_uploads.remove(&upload_id);
                }
                let new_ids = self.apply_updates_to_session(&updates);
                if new_ids.is_empty() {
                    Ok(vec![fallback_sent_history_message(
                        self,
                        peer_id,
                        DataMsgId(0),
                        &options.caption,
                        None,
                        silent,
                        schedule_date,
                    )])
                } else {
                    Ok(new_ids
                        .into_iter()
                        .map(|mid| {
                            fallback_sent_history_message(
                                self,
                                peer_id,
                                mid,
                                &options.caption,
                                None,
                                silent,
                                schedule_date,
                            )
                        })
                        .collect())
                }
            }
            Err(err) => {
                if let Ok(mut guard) = state_arc.lock()
                    && let Some(entry) = guard.active_uploads.get_mut(&upload_id)
                {
                    if entry.cancel.is_cancelled() {
                        entry.status.cancelled = true;
                    } else {
                        entry.status.failed = true;
                    }
                }
                self.bridge_state().notify_ui();
                Err(err)
            }
        }
    }

    /// Forces a fresh `help.getTermsOfServiceUpdate` RPC and updates the cached ToS document.
    pub fn refresh_terms_of_service(&self) -> Result<TermsOfServiceData, String> {
        let rpc = self.runtime().rpc().clone();
        let resp = block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(UPLOAD_SEND_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::help::GetTermsOfServiceUpdate {}).await
            })
            .await
        })?;
        let now = now_unix_i32();
        let (data, expires) = match resp {
            tl_enums::help::TermsOfServiceUpdate::Empty(empty) => (
                TermsOfServiceData {
                    id: String::new(),
                    title: "Terms of Service".to_owned(),
                    body: String::new(),
                    min_age_confirm: None,
                    accepted: true,
                },
                empty.expires,
            ),
            tl_enums::help::TermsOfServiceUpdate::TermsOfServiceUpdate(upd) => {
                let tl_enums::help::TermsOfService::TermsOfService(tos) = upd.terms_of_service;
                let tl_enums::DataJSON::DataJSON(id_json) = tos.id;
                let min_age_confirm = tos.min_age_confirm.and_then(|v| u8::try_from(v).ok());
                (
                    TermsOfServiceData {
                        id: id_json.data,
                        title: "Terms of Service".to_owned(),
                        body: tos.text,
                        min_age_confirm,
                        accepted: false,
                    },
                    upd.expires,
                )
            }
        };
        let timeout = (expires.saturating_sub(now))
            .clamp(TERMS_UPDATE_TIMEOUT_MIN_SECS, TERMS_UPDATE_TIMEOUT_MAX_SECS);
        let next_check_unix = now.saturating_add(timeout);
        if let Ok(mut guard) = upload_state_for(self).lock() {
            guard.tos_cache = Some(CachedTermsOfService {
                data: data.clone(),
                next_check_unix,
            });
        }
        Ok(data)
    }

    /// Forwards media item `id` to `to_peer` via `messages.forwardMessages`.
    pub fn forward_media_item_to_peer(
        &self,
        id: MediaItemId,
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        forward_media_item_impl(self, id, Some(to_peer), options)
    }

    #[cfg(test)]
    pub(crate) fn set_clipboard_test_sink(&self, sink: Option<ClipboardTestSink>) {
        if let Ok(mut guard) = upload_state_for(self).lock() {
            guard.clipboard_sink_override = sink;
        }
    }
}

// ============================================================================
// Delegation entry points called by `live/session.rs`
// ============================================================================

/// Sends one uploaded attachment from the compose box without waiting: with a pending bubble
/// (TGB-3033), or for a scheduled send into the scheduled section (TGB-3130).
fn send_single_media(
    shown: HistoryMedia,
    live: &mut LiveSession,
    peer_id: PeerId,
    media: OutgoingMedia,
    opts: SendMediaOptions,
) -> Result<HistoryMessage, String> {
    if opts.schedule_date.is_some() {
        live.send_scheduled_media_queued(peer_id, media, opts, shown)
    } else {
        live.send_uploaded_media_queued(peer_id, media, opts, shown)
    }
}

/// Handles `HistoryBackend::send_message_extended` when `request.media.is_some()`.
pub(super) fn send_history_media_extended(
    live: &mut LiveSession,
    peer_id: PeerId,
    request: SendMessageExtended,
) -> Result<HistoryMessage, String> {
    let Some(media) = request.media else {
        return Err("send_history_media_extended called without media".to_owned());
    };
    let opts = SendMediaOptions {
        caption: request.text,
        reply_to: request.reply_to,
        reply_quote: request.reply_quote.map(|q| q.text),
        silent: request.silent,
        schedule_date: request.schedule_date,
        send_as: request.send_as,
        effect_id: request.effect_id,
        invert_media: false,
        clear_draft: true,
    };

    let req_ttl_seconds = request.ttl_seconds;
    let req_round_video_path = request.round_video_path.clone();
    let req_video_cover_jpeg = request.video_cover_jpeg;
    let req_voice_ogg = request.voice_ogg.filter(|bytes| !bytes.is_empty());
    let shown = media.clone();

    match media {
        HistoryMedia::Photo(photo) => {
            let bytes = resolve_bytes_for_attachment(
                live,
                &photo.file_name,
                photo.stripped_thumb.as_deref(),
            )?;
            let name = file_basename(&photo.file_name, "photo.jpg");
            send_single_media(
                shown,
                live,
                peer_id,
                OutgoingMedia::Photo {
                    bytes,
                    file_name: name,
                    spoiler: false,
                    ttl_seconds: req_ttl_seconds,
                },
                opts,
            )
        }
        HistoryMedia::Document(doc) => {
            let bytes = resolve_bytes_for_attachment(live, &doc.file_name, None)?;
            let name = file_basename(&doc.file_name, "file.bin");
            send_single_media(
                shown,
                live,
                peer_id,
                OutgoingMedia::Document {
                    bytes,
                    file_name: name,
                    mime_type: doc.mime_type,
                    thumb_bytes: None,
                    force_file: true,
                    spoiler: false,
                },
                opts,
            )
        }
        HistoryMedia::Video(video) => {
            let bytes = resolve_bytes_for_attachment(live, &video.file_name, None)?;
            let name = file_basename(
                &video.file_name,
                if video.is_gif {
                    "animation.mp4"
                } else {
                    "video.mp4"
                },
            );
            let outgoing = if video.is_gif {
                OutgoingMedia::Gif {
                    bytes,
                    file_name: name,
                    mime_type: "video/mp4".to_owned(),
                    width: video.width,
                    height: video.height,
                    duration_secs: Some(f64::from(video.duration_secs)),
                    thumb_bytes: video.stripped_thumb,
                    spoiler: false,
                }
            } else {
                OutgoingMedia::Video {
                    bytes,
                    file_name: name,
                    mime_type: "video/mp4".to_owned(),
                    width: video.width,
                    height: video.height,
                    duration_secs: f64::from(video.duration_secs),
                    supports_streaming: true,
                    nosound: false,
                    round_message: false,
                    thumb_bytes: video.stripped_thumb,
                    video_cover_bytes: req_video_cover_jpeg,
                    spoiler: false,
                    ttl_seconds: req_ttl_seconds,
                }
            };
            send_single_media(shown, live, peer_id, outgoing, opts)
        }
        HistoryMedia::Voice(voice) => {
            // The recorder hands over the encoded file (`sendVoiceMessage(result.bytes, ..)`);
            // bytes staged under "voice.ogg" remain the fallback for programmatic sends.
            let bytes = match req_voice_ogg {
                Some(bytes) => bytes,
                None => resolve_bytes_for_attachment(live, "voice.ogg", None)?,
            };
            send_single_media(
                shown,
                live,
                peer_id,
                OutgoingMedia::Voice {
                    bytes,
                    file_name: "voice.ogg".to_owned(),
                    duration_secs: voice.duration_secs as i32,
                    waveform_5bit: voice.waveform_5bit,
                },
                opts,
            )
        }
        HistoryMedia::Sticker(sticker) => {
            let fallback =
                (!sticker.lottie_json.is_empty()).then_some(sticker.lottie_json.as_bytes());
            let key = format!("sticker_{}", sticker.sticker_id);
            let bytes = resolve_bytes_for_attachment(live, &key, fallback)?;
            let is_lottie = !sticker.lottie_json.is_empty();
            send_single_media(
                shown,
                live,
                peer_id,
                OutgoingMedia::Sticker {
                    bytes,
                    file_name: if is_lottie {
                        "sticker.tgs".to_owned()
                    } else {
                        "sticker.webp".to_owned()
                    },
                    mime_type: if is_lottie {
                        "application/x-tgsticker".to_owned()
                    } else {
                        "image/webp".to_owned()
                    },
                    width: sticker.width,
                    height: sticker.height,
                    alt_emoji: sticker.emoji,
                },
                opts,
            )
        }
        HistoryMedia::RoundVideo(rv) => {
            let source_key = req_round_video_path
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("round_video.mp4");
            let bytes = resolve_bytes_for_attachment(live, source_key, None)?;
            send_single_media(
                shown,
                live,
                peer_id,
                OutgoingMedia::Video {
                    bytes,
                    file_name: "round_video.mp4".to_owned(),
                    mime_type: "video/mp4".to_owned(),
                    width: 384,
                    height: 384,
                    duration_secs: f64::from(rv.duration_secs),
                    supports_streaming: true,
                    nosound: false,
                    round_message: true,
                    thumb_bytes: None,
                    video_cover_bytes: None,
                    spoiler: false,
                    ttl_seconds: req_ttl_seconds,
                },
                opts,
            )
        }
        HistoryMedia::Album(album_items) => {
            let mut upload_items = Vec::with_capacity(album_items.len());
            for item in album_items {
                let bytes = resolve_bytes_for_attachment(
                    live,
                    &item.label,
                    item.stripped_thumb.as_deref(),
                )?;
                let name = file_basename(&item.label, "album_item.jpg");
                let size_bytes = bytes.len() as u64;
                let shown = match item.video_duration_secs {
                    Some(duration_secs) => {
                        HistoryMedia::Video(tg_app_core::history::backend::VideoAttachment {
                            width: item.width,
                            height: item.height,
                            duration_secs,
                            size_bytes,
                            is_gif: false,
                            stripped_thumb: item.stripped_thumb.clone(),
                            fallback_rgb: item.fallback_rgb,
                            file_name: item.label.clone(),
                        })
                    }
                    None => HistoryMedia::Photo(tg_app_core::history::backend::PhotoAttachment {
                        width: item.width,
                        height: item.height,
                        stripped_thumb: item.stripped_thumb.clone(),
                        fallback_rgb: item.fallback_rgb,
                        file_name: item.label.clone(),
                        size_bytes,
                    }),
                };
                let media = if let Some(dur) = item.video_duration_secs {
                    OutgoingMedia::Video {
                        bytes,
                        file_name: name,
                        mime_type: "video/mp4".to_owned(),
                        width: item.width,
                        height: item.height,
                        duration_secs: f64::from(dur),
                        supports_streaming: true,
                        nosound: false,
                        round_message: false,
                        thumb_bytes: item.stripped_thumb,
                        video_cover_bytes: None,
                        spoiler: false,
                        ttl_seconds: None,
                    }
                } else {
                    OutgoingMedia::Photo {
                        bytes,
                        file_name: name,
                        spoiler: false,
                        ttl_seconds: None,
                    }
                };
                upload_items.push(album_queue::QueuedAlbumItem {
                    media,
                    caption: None,
                    shown,
                });
            }
            live.send_album_queued(peer_id, upload_items, opts)
        }
        HistoryMedia::Location(loc) => {
            live.send_uploaded_media(peer_id, OutgoingMedia::Location(loc), opts)
        }
        HistoryMedia::Contact(c) => {
            live.send_uploaded_media(peer_id, OutgoingMedia::Contact(c), opts)
        }
        HistoryMedia::Poll(poll) => {
            let create = tg_app_core::history::sections_model::CreatePollRequest {
                question: poll.question,
                options: poll.options.iter().map(|o| o.text.clone()).collect(),
                is_anonymous: poll.is_anonymous,
                multiple_choice: poll.multiple_choice,
                is_quiz: poll.is_quiz,
                correct_option: poll.options.iter().position(|o| o.correct),
                explanation: poll.explanation,
                silent: opts.silent,
                schedule_date: opts.schedule_date,
            };
            HistoryBackend::send_poll(live, peer_id, None, create)
        }
        HistoryMedia::Game(_) => {
            Err("Sending bot game media directly is not supported.".to_owned())
        }
        HistoryMedia::TodoList(_) => {
            Err("Checklists are sent through the checklist box (create_todo_list).".to_owned())
        }
        HistoryMedia::Widget(_) => {
            Err("Sending HTML widget media directly is not supported.".to_owned())
        }
        HistoryMedia::Story(_)
        | HistoryMedia::Giveaway(_)
        | HistoryMedia::GiveawayResults(_)
        | HistoryMedia::VideoStream(_) => {
            Err("Sending this attachment type directly from history is not supported.".to_owned())
        }
    }
}

/// Returns active outgoing media uploads for `peer_id`.
pub(super) fn pending_media_uploads_for(
    live: &LiveSession,
    peer_id: PeerId,
) -> Vec<MediaUploadStatus> {
    upload_state_for(live)
        .lock()
        .map(|guard| {
            let mut out: Vec<MediaUploadStatus> = guard
                .active_uploads
                .values()
                .filter(|u| u.status.peer_id == peer_id)
                .map(|u| u.status.clone())
                .collect();
            out.sort_by_key(|s| s.upload_id);
            out
        })
        .unwrap_or_default()
}

/// Cancels an active media upload by `upload_id`.
pub(super) fn cancel_media_upload_by_id(live: &LiveSession, upload_id: u64) -> bool {
    let cancelled = if let Ok(mut guard) = upload_state_for(live).lock()
        && let Some(entry) = guard.active_uploads.get_mut(&upload_id)
    {
        entry.cancel.cancel();
        entry.status.cancelled = true;
        true
    } else {
        false
    };
    if cancelled {
        live.bridge_state().notify_ui();
    }
    cancelled
}

/// Registers an active synthetic upload entry on `live` for UI/cancellation tests.
#[doc(hidden)]
pub fn register_test_upload_for(
    live: &LiveSession,
    peer_id: PeerId,
    file_name: impl Into<String>,
    uploaded_bytes: u64,
    total_bytes: u64,
) -> (u64, CancelToken) {
    let cancel = CancelToken::new();
    let state_arc = upload_state_for(live);
    let uid = {
        let mut guard = state_arc.lock().expect("lock upload state");
        let uid = guard.next_upload_id;
        guard.next_upload_id = guard.next_upload_id.wrapping_add(1);
        guard.active_uploads.insert(
            uid,
            TrackedMediaUpload {
                status: MediaUploadStatus {
                    upload_id: uid,
                    peer_id,
                    file_name: file_name.into(),
                    uploaded_bytes,
                    total_bytes,
                    failed: false,
                    cancelled: false,
                },
                cancel: cancel.clone(),
            },
        );
        uid
    };
    live.bridge_state().notify_ui();
    (uid, cancel)
}

/// Fetches or returns cached Terms of Service via `help.getTermsOfServiceUpdate`.
pub(super) fn fetch_terms_of_service(live: &LiveSession) -> TermsOfServiceData {
    let now = now_unix_i32();
    if let Ok(guard) = upload_state_for(live).lock()
        && let Some(ref cached) = guard.tos_cache
        && now < cached.next_check_unix
    {
        return cached.data.clone();
    }
    match live.refresh_terms_of_service() {
        Ok(data) => data,
        Err(err) => {
            warn!(error = %err, "help.getTermsOfServiceUpdate failed");
            // Cache a short backoff so 60fps UI frames don't spam the RPC on error.
            let fallback = TermsOfServiceData {
                id: String::new(),
                title: "Terms of Service".to_owned(),
                body: String::new(),
                min_age_confirm: None,
                accepted: true,
            };
            if let Ok(mut guard) = upload_state_for(live).lock() {
                guard.tos_cache = Some(CachedTermsOfService {
                    data: fallback.clone(),
                    next_check_unix: now.saturating_add(TERMS_UPDATE_TIMEOUT_MIN_SECS),
                });
            }
            fallback
        }
    }
}

/// Accepts a Terms of Service revision via `help.acceptTermsOfService`.
pub(super) fn accept_terms_of_service(live: &mut LiveSession, tos_id: &str) -> Result<(), String> {
    let effective_id = if !tos_id.trim().is_empty() {
        tos_id.to_owned()
    } else if let Ok(guard) = upload_state_for(live).lock()
        && let Some(ref cached) = guard.tos_cache
        && !cached.data.id.is_empty()
    {
        cached.data.id.clone()
    } else {
        return Err("Terms of Service ID is empty.".to_owned());
    };

    let req = tl_fns::help::AcceptTermsOfService {
        id: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: effective_id }),
    };
    let rpc = live.runtime().rpc().clone();
    let accepted = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            UPLOAD_SEND_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })?;
    if !accepted {
        return Err("Server rejected Terms of Service acceptance.".to_owned());
    }
    if let Ok(mut guard) = upload_state_for(live).lock()
        && let Some(ref mut cached) = guard.tos_cache
    {
        cached.data.accepted = true;
        cached.next_check_unix = 0;
    }
    live.bridge_state().notify_ui();
    Ok(())
}

/// Decodes raw image bytes into top-down RGBA8 `(width, height, rgba_bytes)` applying
/// `rotation_degrees`, enforcing [`tg_media::image::READ_BYTES_LIMIT`] and
/// [`tg_media::image::READ_MAX_AREA`] before pixel decoding.
pub fn decode_image_for_clipboard(
    bytes: &[u8],
    rotation_degrees: i32,
) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let decoded = tg_media::image::read_image(tg_media::image::ReadArgs {
        content: bytes.to_vec(),
        ..Default::default()
    })
    .map_err(|e| anyhow!("failed to decode media image for clipboard: {e}"))?;
    let rgba = if rotation_degrees % 360 != 0 {
        transform_dynamic_image(
            &image::DynamicImage::ImageRgba8(decoded.image),
            rotation_degrees,
            false,
            false,
        )
        .to_rgba8()
    } else {
        decoded.image
    };
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(anyhow!("decoded media image has zero dimensions"));
    }
    Ok((width, height, rgba.into_raw()))
}

/// Copies an RGBA8 image into the OS clipboard using `arboard`.
pub fn copy_rgba_to_clipboard(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| anyhow!("clipboard image dimensions overflow"))?;
    if rgba.len() != expected {
        return Err(anyhow!(
            "clipboard RGBA buffer length mismatch: expected {expected}, got {}",
            rgba.len()
        ));
    }
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| anyhow!("failed to open OS clipboard: {e}"))?;
    clipboard
        .set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: Cow::Borrowed(rgba),
        })
        .map_err(|e| anyhow!("failed to write image to OS clipboard: {e}"))
}

/// Downloads, decodes, and copies media item `id` to the OS clipboard.
pub(super) fn copy_media_item_to_clipboard(
    live: &LiveSession,
    id: MediaItemId,
) -> anyhow::Result<()> {
    let item = live.get_item(id);
    if let Some(ref meta) = item
        && !meta.allows_save
    {
        return Err(anyhow!("Saving or copying this media is restricted."));
    }
    let bytes = live.load_bytes(id)?;
    let rotation = item.map_or(0, |m| m.rotation_degrees);
    let (width, height, rgba) = decode_image_for_clipboard(&bytes, rotation)?;

    let override_sink = upload_state_for(live)
        .lock()
        .ok()
        .and_then(|g| g.clipboard_sink_override.clone());
    if let Some(sink) = override_sink {
        return sink(width, height, &rgba);
    }
    copy_rgba_to_clipboard(width, height, &rgba)
}

/// Forwards media item `id` from the media overlay via `messages.forwardMessages`.
pub(super) fn forward_media_item(
    live: &LiveSession,
    id: MediaItemId,
    explicit_target: Option<PeerId>,
) -> anyhow::Result<()> {
    let _ = forward_media_item_impl(live, id, explicit_target, ForwardOptions::default())?;
    Ok(())
}

fn forward_media_item_impl(
    live: &LiveSession,
    id: MediaItemId,
    explicit_target: Option<PeerId>,
    options: ForwardOptions,
) -> anyhow::Result<Vec<HistoryMessage>> {
    if let Some(meta) = live.get_item(id)
        && !meta.allows_forward
    {
        return Err(anyhow!("Forwarding this media is restricted."));
    }
    let (from_data_peer, msg_id, _) = live
        .resolve_media_by_id(id)
        .ok_or_else(|| anyhow!("media item {} is not loaded in session", id.0))?;

    let target_peer = explicit_target
        .or_else(|| {
            upload_state_for(live)
                .lock()
                .ok()
                .and_then(|g| g.media_forward_target)
        })
        .unwrap_or_else(|| read_session_sync(live, |s| from_data_peer_id(s.self_peer_id())));

    let from_input = read_session_sync(live, |s| input_peer_for(s, from_data_peer));
    if matches!(from_input, tl_enums::InputPeer::Empty) {
        return Err(anyhow!(
            "source peer {} not found in session",
            from_data_peer.raw()
        ));
    }
    let to_data_peer = to_data_peer(target_peer);
    let to_input = read_session_sync(live, |s| input_peer_for(s, to_data_peer));
    if matches!(to_input, tl_enums::InputPeer::Empty) {
        return Err(anyhow!(
            "destination peer {} not found in session",
            target_peer.0
        ));
    }

    let random_id = upload_state_for(live)
        .lock()
        .map(|mut g| g.next_random_id())
        .unwrap_or(1);

    let req = tl_fns::messages::ForwardMessages {
        silent: false,
        background: false,
        with_my_score: false,
        drop_author: options.drop_author,
        drop_media_captions: options.drop_media_captions,
        noforwards: false,
        allow_paid_floodskip: false,
        from_ephemeral: false,
        from_peer: from_input,
        id: vec![msg_id.0 as i32],
        random_id: vec![random_id],
        to_peer: to_input,
        top_msg_id: None,
        reply_to: None,
        schedule_date: None,
        schedule_repeat_period: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        video_timestamp: None,
        allow_paid_stars: None,
        suggested_post: None,
    };

    let rpc = live.runtime().rpc().clone();
    let updates = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            UPLOAD_SEND_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
    .map_err(|e| anyhow!("messages.forwardMessages failed: {e}"))?;

    let new_ids = live.apply_updates_to_session(&updates);
    let forwarded = read_session_sync(live, |s| {
        new_ids
            .into_iter()
            .filter_map(|mid| {
                s.message(DataFullMsgId::new(to_data_peer, mid))
                    .map(|m| message_to_history_message(s, m))
            })
            .collect()
    });
    Ok(forwarded)
}

#[inline]
const fn from_data_peer_id(peer: DataPeerId) -> PeerId {
    from_data_peer(peer)
}

/// Applies `users` and `chats` from an RPC response into `session` (helper for story/upload callers).
#[allow(dead_code)]
pub(crate) fn apply_users_and_chats_sync(
    live: &LiveSession,
    users: &[tl_enums::User],
    chats: &[tl_enums::Chat],
) {
    if users.is_empty() && chats.is_empty() {
        return;
    }
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        apply_tl_users_and_chats(&mut guard, users, chats);
        return;
    }
    let arc = Arc::clone(lock);
    let users_owned = users.to_vec();
    let chats_owned = chats.to_vec();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        apply_tl_users_and_chats(&mut guard, &users_owned, &chats_owned);
    });
}

#[cfg(test)]
mod sec2c_tests {
    use super::*;

    fn encode_1bit_gray_png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::One);
            let mut writer = encoder.write_header().expect("png header");
            let row_bytes = (width as usize).div_ceil(8);
            let mut stream = writer.stream_writer().expect("png stream writer");
            let row = vec![0_u8; row_bytes];
            for _ in 0..height {
                std::io::Write::write_all(&mut stream, &row).expect("write png row");
            }
            stream.finish().expect("finish png");
        }
        out
    }

    #[test]
    fn sec2c_w5_decode_image_for_clipboard_rejects_oversized_pixel_dimensions() {
        // 12_100 * 9_100 = 110_110_000 pixels > tg_media::image::READ_MAX_AREA (108_576_768).
        let oversized_png = encode_1bit_gray_png(12_100, 9_100);
        let err = decode_image_for_clipboard(&oversized_png, 0)
            .expect_err("oversized clipboard image must be rejected before pixel decode");
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("limit") || msg.contains("area"),
            "expected area limit error, got: {err}"
        );

        // Small image with 90-degree rotation still decodes cleanly.
        let small_png = encode_1bit_gray_png(8, 4);
        let (w, h, rgba) =
            decode_image_for_clipboard(&small_png, 90).expect("small clipboard image must decode");
        assert_eq!((w, h), (4, 8));
        assert_eq!(rgba.len(), 4 * 8 * 4);
    }
}
