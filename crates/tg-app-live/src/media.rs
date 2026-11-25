//! Live media transfer adapters over `tg-api`'s [`RpcClient`] (`upload.getFile` et al.),
//! implementing `tg-media`'s [`MediaInvoker`], `tg-video`'s [`RangeLoader`], and
//! `tg-audio`'s [`AudioDataSource`] / [`PlaylistProvider`].
//!
//! Port origin: `Telegram/SourceFiles/storage/file_download_mtproto.cpp`,
//! `Telegram/SourceFiles/media/streaming/media_streaming_loader_mtproto.cpp`,
//! and `Telegram/SourceFiles/media/audio/media_audio_loader.cpp`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bridge::LiveBridgeState;
use crate::convert::{history_messages_ordered, known_peer_ids};
use anyhow::anyhow;
use tg_api::RpcClient;
use tg_api::convert::{
    apply_tl_message, apply_tl_users_and_chats, input_channel_for, input_peer_for, input_user_for,
};
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_audio::{
    AudioDataSource, AudioError, AudioInput, AudioMsgId, AudioResult, AudioType, MediaKind,
    PlaylistContext, PlaylistEntry, PlaylistProvider,
};
use tg_crypto::random_u64;
use tg_data::{
    DocumentData, DocumentId, DocumentKind, FullMsgId, MessageContent, MessageFlags, MessageMedia,
    PeerId, PhotoData, Session, UserId,
};
use tg_media::error::MediaError;
use tg_media::transfer::{
    BoxFuture, CdnFileHash, CdnHashVerifier, CdnRedirect, CheckCdnHashResult, DOWNLOAD_PART_SIZE,
    FileOrigin, GetCdnFileRequest, GetCdnFileResponse, GetFileRequest, GetFileResponse,
    GetWebFileRequest, GetWebFileResponse, MediaInvoker, SaveBigFilePartRequest,
    SaveFilePartRequest, TransferLocation, align_download_offset, apply_cdn_aes_ctr,
    validate_get_cdn_file_request, validate_get_file_request,
};
use tg_session::SessionRuntime;
use tg_storage::cache::binlog::CacheKey as StorageCacheKey;
use tg_storage::streamed_downloader::{
    ChunkFuture, ChunkProvider, DEFAULT_MAX_IN_FLIGHT_PARTS, DownloadProgress,
    StreamedDownloadError, StreamedDownloadResult, StreamedDownloaderConfig,
    StreamedFileDownloader,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tg_video::RangeLoader;
use tg_video::error::StreamingError;
use tg_video::streaming::cache::CacheKey;
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::RwLock;
use tokio::task::AbortHandle;

/// Default timeout for a single synchronous media RPC chunk (`upload.getFile`).
pub const MEDIA_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Safely executes an async future on `handle` from synchronous UI or trait methods,
/// whether the caller is on a non-Tokio thread, a multi-thread Tokio worker, or a
/// `current_thread` Tokio test runtime.
pub(crate) fn block_on_runtime<F, T>(handle: &Handle, fut: F) -> T
where
    F: Future<Output = T> + Send,
    T: Send,
{
    match Handle::try_current() {
        Ok(current) => match current.runtime_flavor() {
            RuntimeFlavor::MultiThread => tokio::task::block_in_place(|| handle.block_on(fut)),
            _ => std::thread::scope(|scope| {
                scope
                    .spawn(|| handle.block_on(fut))
                    .join()
                    .unwrap_or_else(|_| panic!("live runtime helper thread panicked"))
            }),
        },
        Err(_) => handle.block_on(fut),
    }
}

/// Runs an RPC future with a bounded timeout so sync trait methods never hang forever.
pub(crate) async fn with_rpc_timeout<F, T, E>(
    timeout: Duration,
    fut: F,
) -> std::result::Result<T, String>
where
    F: Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(format!("request timed out after {}s", timeout.as_secs())),
    }
}

/// Describes a downloadable cloud media payload resolved from a `tg-data` message.
#[derive(Debug, Clone)]
pub struct ResolvedMediaLocation {
    /// Hosting DC ID (`<= 0` falls back to main DC).
    pub dc_id: i32,
    /// TL `InputFileLocation` passed to `upload.getFile`.
    pub location: tl_enums::InputFileLocation,
    /// Known file size in bytes (`0` if unknown).
    pub size: u64,
    /// Stable cache key for streaming range loaders.
    pub cache_key: CacheKey,
    /// Suggested file extension hint (lowercase, without dot).
    pub ext_hint: Option<String>,
}

/// Builds an [`InputFileLocation`] for a [`PhotoData`] selecting the largest downloadable size tag.
#[must_use]
pub fn photo_to_location(photo: &PhotoData) -> ResolvedMediaLocation {
    let best_size = photo
        .sizes
        .iter()
        .filter(|s| !s.kind.is_empty() && s.kind != "i" && s.kind != "j")
        .max_by_key(|s| (i64::from(s.width) * i64::from(s.height), s.size));
    let thumb_size = best_size
        .map(|s| s.kind.clone())
        .unwrap_or_else(|| "y".to_owned());
    let size = best_size
        .and_then(|s| u64::try_from(s.size).ok())
        .unwrap_or(0);
    ResolvedMediaLocation {
        dc_id: photo.dc_id,
        location: tl_enums::InputFileLocation::InputPhotoFileLocation(
            tl_types::InputPhotoFileLocation {
                id: photo.id.0 as i64,
                access_hash: photo.access_hash as i64,
                file_reference: photo.file_reference.clone(),
                thumb_size,
            },
        ),
        size,
        cache_key: CacheKey {
            high: photo.id.0,
            low: 1,
        },
        ext_hint: Some("jpg".to_owned()),
    }
}

/// Builds an [`InputFileLocation`] for a [`PhotoData`]'s grid-appropriate thumbnail size
/// (`"m"` 320px box preferred, then `"x"` 800px, then `"s"` 100px, then smallest downloadable
/// non-stripped size), matching tdesktop's `Info::Media::Photo` thumbnail selection.
#[must_use]
pub fn photo_thumb_location(photo: &PhotoData) -> Option<ResolvedMediaLocation> {
    let downloadable: Vec<_> = photo
        .sizes
        .iter()
        .filter(|s| !s.kind.is_empty() && s.kind != "i" && s.kind != "j")
        .collect();
    if downloadable.is_empty() {
        return None;
    }
    let chosen = ["m", "x", "s"]
        .into_iter()
        .find_map(|pref| downloadable.iter().copied().find(|s| s.kind == pref))
        .or_else(|| {
            downloadable
                .iter()
                .copied()
                .min_by_key(|s| (i64::from(s.width) * i64::from(s.height), s.size))
        })?;
    let size = u64::try_from(chosen.size).unwrap_or(0);
    Some(ResolvedMediaLocation {
        dc_id: photo.dc_id,
        location: tl_enums::InputFileLocation::InputPhotoFileLocation(
            tl_types::InputPhotoFileLocation {
                id: photo.id.0 as i64,
                access_hash: photo.access_hash as i64,
                file_reference: photo.file_reference.clone(),
                thumb_size: chosen.kind.clone(),
            },
        ),
        size,
        cache_key: CacheKey {
            high: photo.id.0,
            low: 4,
        },
        ext_hint: Some("jpg".to_owned()),
    })
}

/// Builds an [`InputFileLocation`] for a [`DocumentData`]'s best downloadable thumbnail, if any.
#[must_use]
pub fn document_thumb_location(doc: &DocumentData) -> Option<ResolvedMediaLocation> {
    let best = doc
        .thumbs
        .iter()
        .filter(|s| !s.kind.is_empty() && s.kind != "i" && s.kind != "j")
        .max_by_key(|s| (i64::from(s.width) * i64::from(s.height), s.size))?;
    let size = u64::try_from(best.size).unwrap_or(0);
    Some(ResolvedMediaLocation {
        dc_id: doc.dc_id,
        location: tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: doc.id.0 as i64,
                access_hash: doc.access_hash as i64,
                file_reference: doc.file_reference.clone(),
                thumb_size: best.kind.clone(),
            },
        ),
        size,
        cache_key: CacheKey {
            high: doc.id.0,
            low: 3,
        },
        ext_hint: Some("jpg".to_owned()),
    })
}

/// Builds an [`InputFileLocation`] for a full [`DocumentData`].
#[must_use]
pub fn document_to_location(doc: &DocumentData) -> ResolvedMediaLocation {
    let ext_hint = std::path::Path::new(&doc.file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| match doc.mime_type.as_str() {
            "audio/ogg" => Some("ogg".to_owned()),
            "audio/mpeg" => Some("mp3".to_owned()),
            "audio/flac" => Some("flac".to_owned()),
            "audio/mp4" | "audio/m4a" => Some("m4a".to_owned()),
            "video/mp4" => Some("mp4".to_owned()),
            "video/webm" => Some("webm".to_owned()),
            "image/png" => Some("png".to_owned()),
            "image/jpeg" => Some("jpg".to_owned()),
            "image/webp" => Some("webp".to_owned()),
            _ => None,
        });
    ResolvedMediaLocation {
        dc_id: doc.dc_id,
        location: tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: doc.id.0 as i64,
                access_hash: doc.access_hash as i64,
                file_reference: doc.file_reference.clone(),
                thumb_size: String::new(),
            },
        ),
        size: u64::try_from(doc.size).unwrap_or(0),
        cache_key: CacheKey {
            high: doc.id.0,
            low: 2,
        },
        ext_hint,
    }
}

/// Resolves a downloadable [`ResolvedMediaLocation`] from a [`MessageMedia`].
#[must_use]
pub fn message_media_to_location(media: &MessageMedia) -> Option<ResolvedMediaLocation> {
    match media {
        MessageMedia::Photo(photo) => Some(photo_to_location(photo)),
        MessageMedia::Document(doc) => Some(document_to_location(doc)),
        _ => None,
    }
}

/// Active CDN session state established by `upload.fileCdnRedirect`
/// (`DownloadMtprotoTask` in `storage/download_manager_mtproto.cpp`).
#[derive(Debug, Clone)]
pub(crate) struct ActiveCdnState {
    cdn_dc_id: i32,
    file_token: Vec<u8>,
    key: [u8; 32],
    iv: [u8; 16],
    verifier: CdnHashVerifier,
}

impl ActiveCdnState {
    fn from_tl(redir: tl_types::upload::FileCdnRedirect) -> anyhow::Result<Self> {
        if redir.dc_id <= 0 {
            return Err(anyhow!("invalid CDN dc_id: {}", redir.dc_id));
        }
        let key: [u8; 32] = redir
            .encryption_key
            .try_into()
            .map_err(|v: Vec<u8>| anyhow!("invalid CDN encryption_key length: {}", v.len()))?;
        let iv: [u8; 16] = redir
            .encryption_iv
            .try_into()
            .map_err(|v: Vec<u8>| anyhow!("invalid CDN encryption_iv length: {}", v.len()))?;
        let mut verifier = CdnHashVerifier::new();
        verifier.add_hashes(map_file_hashes(redir.file_hashes));
        Ok(Self {
            cdn_dc_id: redir.dc_id,
            file_token: redir.file_token,
            key,
            iv,
            verifier,
        })
    }

    fn from_cdn_redirect(redir: CdnRedirect) -> anyhow::Result<Self> {
        if redir.dc_id <= 0 {
            return Err(anyhow!("invalid CDN dc_id: {}", redir.dc_id));
        }
        let key: [u8; 32] = redir
            .encryption_key
            .try_into()
            .map_err(|v: Vec<u8>| anyhow!("invalid CDN encryption_key length: {}", v.len()))?;
        let iv: [u8; 16] = redir
            .encryption_iv
            .try_into()
            .map_err(|v: Vec<u8>| anyhow!("invalid CDN encryption_iv length: {}", v.len()))?;
        let mut verifier = CdnHashVerifier::new();
        verifier.add_hashes(redir.file_hashes);
        Ok(Self {
            cdn_dc_id: redir.dc_id,
            file_token: redir.file_token,
            key,
            iv,
            verifier,
        })
    }
}

fn is_cdn_token_invalid_err(inv: &tg_api::rpc::InvocationError) -> bool {
    matches!(
        inv,
        tg_api::rpc::InvocationError::Rpc(r)
            if r.name == "FILE_TOKEN_INVALID" || r.name == "REQUEST_TOKEN_INVALID"
    )
}

/// Returns `true` if `inv` is a Telegram `FILE_REFERENCE_*` RPC error (`FILE_REFERENCE_EXPIRED`,
/// `FILE_REFERENCE_INVALID`, `FILE_REFERENCE_EMPTY`, `FILE_REFERENCE_0_EXPIRED`, ...).
#[must_use]
pub fn is_file_reference_error(inv: &tg_api::rpc::InvocationError) -> bool {
    matches!(
        inv,
        tg_api::rpc::InvocationError::Rpc(r) if r.name.starts_with("FILE_REFERENCE_")
    )
}

/// Collected fresh `file_reference` bytes keyed by photo ID and document ID
/// (`Data::UpdatedFileReferences` / `FileReferenceAccumulator` in `data/data_file_origin.cpp`).
#[derive(Debug, Default, Clone)]
pub struct UpdatedFileReferences {
    /// Refreshed photo `file_reference` bytes by photo ID.
    pub photos: HashMap<u64, Vec<u8>>,
    /// Refreshed document `file_reference` bytes by document ID.
    pub documents: HashMap<u64, Vec<u8>>,
}

impl UpdatedFileReferences {
    /// Collects a photo's `file_reference`.
    pub fn push_photo(&mut self, photo: &tl_enums::Photo) {
        if let tl_enums::Photo::Photo(p) = photo {
            self.photos.insert(p.id as u64, p.file_reference.clone());
        }
    }

    /// Collects a document's `file_reference`.
    pub fn push_document(&mut self, doc: &tl_enums::Document) {
        if let tl_enums::Document::Document(d) = doc {
            self.documents.insert(d.id as u64, d.file_reference.clone());
        }
    }

    /// Collects references nested inside a `WebPage`.
    pub fn push_webpage(&mut self, wp: &tl_enums::WebPage) {
        if let tl_enums::WebPage::WebPage(w) = wp {
            if let Some(p) = &w.photo {
                self.push_photo(p);
            }
            if let Some(d) = &w.document {
                self.push_document(d);
            }
            if let Some(tl_enums::Page::Page(page)) = &w.cached_page {
                for p in &page.photos {
                    self.push_photo(p);
                }
                for d in &page.documents {
                    self.push_document(d);
                }
            }
        }
    }

    /// Collects references nested inside a `MessageMedia`.
    pub fn push_message_media(&mut self, media: &tl_enums::MessageMedia) {
        match media {
            tl_enums::MessageMedia::Photo(p) => {
                if let Some(photo) = &p.photo {
                    self.push_photo(photo);
                }
            }
            tl_enums::MessageMedia::Document(d) => {
                if let Some(doc) = &d.document {
                    self.push_document(doc);
                }
                if let Some(cover) = &d.video_cover {
                    self.push_photo(cover);
                }
                if let Some(alts) = &d.alt_documents {
                    for alt in alts {
                        self.push_document(alt);
                    }
                }
            }
            tl_enums::MessageMedia::WebPage(w) => {
                self.push_webpage(&w.webpage);
            }
            tl_enums::MessageMedia::Game(g) => {
                let tl_enums::Game::Game(game) = &g.game;
                self.push_photo(&game.photo);
                if let Some(doc) = &game.document {
                    self.push_document(doc);
                }
            }
            tl_enums::MessageMedia::Invoice(inv) => {
                if let Some(boxed) = &inv.extended_media
                    && let tl_enums::MessageExtendedMedia::MessageExtendedMedia(ext) =
                        boxed.as_ref()
                {
                    self.push_message_media(&ext.media);
                }
            }
            tl_enums::MessageMedia::PaidMedia(pm) => {
                for item in &pm.extended_media {
                    if let tl_enums::MessageExtendedMedia::MessageExtendedMedia(ext) = item {
                        self.push_message_media(&ext.media);
                    }
                }
            }
            _ => {}
        }
    }

    /// Collects references from a single TL `Message`.
    pub fn push_message(&mut self, msg: &tl_enums::Message) {
        match msg {
            tl_enums::Message::Message(m) => {
                if let Some(media) = &m.media {
                    self.push_message_media(media);
                }
                if let Some(tl_enums::MessageReplyHeader::MessageReplyHeader(rh)) = &m.reply_to
                    && let Some(rm) = &rh.reply_media
                {
                    self.push_message_media(rm);
                }
            }
            tl_enums::Message::Service(s) => match &s.action {
                tl_enums::MessageAction::ChatEditPhoto(a) => self.push_photo(&a.photo),
                tl_enums::MessageAction::SuggestProfilePhoto(a) => self.push_photo(&a.photo),
                tl_enums::MessageAction::SetChatWallPaper(a) => {
                    if let tl_enums::WallPaper::WallPaper(wp) = &a.wallpaper {
                        self.push_document(&wp.document);
                    }
                }
                _ => {}
            },
            tl_enums::Message::Empty(_) => {}
        }
    }

    /// Collects references from `messages.Messages`.
    pub fn push_messages(&mut self, resp: &tl_enums::messages::Messages) {
        let list = match resp {
            tl_enums::messages::Messages::Messages(m) => &m.messages,
            tl_enums::messages::Messages::Slice(m) => &m.messages,
            tl_enums::messages::Messages::ChannelMessages(m) => &m.messages,
            tl_enums::messages::Messages::NotModified(_) => return,
        };
        for m in list {
            self.push_message(m);
        }
    }

    /// Collects references from `photos.Photos`.
    pub fn push_photos(&mut self, resp: &tl_enums::photos::Photos) {
        let list = match resp {
            tl_enums::photos::Photos::Photos(p) => &p.photos,
            tl_enums::photos::Photos::Slice(p) => &p.photos,
        };
        for photo in list {
            self.push_photo(photo);
        }
    }

    /// Collects references from `messages.StickerSet`.
    pub fn push_sticker_set(&mut self, resp: &tl_enums::messages::StickerSet) {
        if let tl_enums::messages::StickerSet::StickerSet(ss) = resp {
            for doc in &ss.documents {
                self.push_document(doc);
            }
        }
    }

    /// Collects references from `users.UserFull`.
    pub fn push_user_full(&mut self, resp: &tl_enums::users::UserFull) {
        let tl_enums::users::UserFull::UserFull(uf) = resp;
        let tl_enums::UserFull::UserFull(full) = &uf.full_user;
        if let Some(p) = &full.profile_photo {
            self.push_photo(p);
        }
        if let Some(p) = &full.personal_photo {
            self.push_photo(p);
        }
        if let Some(p) = &full.fallback_photo {
            self.push_photo(p);
        }
    }

    /// Collects references from `messages.ChatFull`.
    pub fn push_chat_full(&mut self, resp: &tl_enums::messages::ChatFull) {
        let tl_enums::messages::ChatFull::ChatFull(cf) = resp;
        match &cf.full_chat {
            tl_enums::ChatFull::ChatFull(c) => {
                if let Some(p) = &c.chat_photo {
                    self.push_photo(p);
                }
            }
            tl_enums::ChatFull::ChannelFull(c) => {
                self.push_photo(&c.chat_photo);
            }
            tl_enums::ChatFull::CommunityFull(_) => {}
        }
    }

    /// Looks up a refreshed `file_reference` for a [`TransferLocation`].
    #[must_use]
    pub fn lookup_transfer(&self, loc: &TransferLocation) -> Option<Vec<u8>> {
        match loc {
            TransferLocation::Photo { id, .. } => self.photos.get(id).cloned(),
            TransferLocation::Document { id, .. } => self.documents.get(id).cloned(),
            TransferLocation::PeerPhoto { photo_id, .. } => self.photos.get(photo_id).cloned(),
            TransferLocation::StickerSetThumb { .. } | TransferLocation::WebFile { .. } => None,
        }
    }

    /// Looks up a refreshed `file_reference` for a TL [`tl_enums::InputFileLocation`].
    #[must_use]
    pub fn lookup_tl(&self, loc: &tl_enums::InputFileLocation) -> Option<Vec<u8>> {
        match loc {
            tl_enums::InputFileLocation::InputPhotoFileLocation(p) => {
                self.photos.get(&(p.id as u64)).cloned()
            }
            tl_enums::InputFileLocation::InputDocumentFileLocation(d) => {
                self.documents.get(&(d.id as u64)).cloned()
            }
            tl_enums::InputFileLocation::InputPeerPhotoFileLocation(p) => {
                self.photos.get(&(p.photo_id as u64)).cloned()
            }
            _ => None,
        }
    }

    /// Applies any updated photo/document `file_reference` bytes onto loaded messages in `session`.
    pub fn apply_to_session(&self, session: &mut Session) {
        if self.photos.is_empty() && self.documents.is_empty() {
            return;
        }
        let peers = known_peer_ids(session);
        let mut msg_keys = Vec::new();
        for pid in peers {
            for msg in history_messages_ordered(session, pid) {
                msg_keys.push(FullMsgId::new(pid, msg.id));
            }
        }
        for fid in msg_keys {
            if let Some(msg) = session.message_mut(fid)
                && let MessageContent::Regular { ref mut media, .. } = msg.content
            {
                match media {
                    MessageMedia::Photo(p) => {
                        if let Some(fresh) = self.photos.get(&p.id.0) {
                            p.file_reference = fresh.clone();
                        }
                    }
                    MessageMedia::Document(d) => {
                        if let Some(fresh) = self.documents.get(&d.id.0) {
                            d.file_reference = fresh.clone();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Converts a TL `InputFileLocation` to a `tg_media::TransferLocation` when representable.
#[must_use]
pub fn tl_location_to_transfer(loc: &tl_enums::InputFileLocation) -> Option<TransferLocation> {
    match loc {
        tl_enums::InputFileLocation::InputPhotoFileLocation(p) => Some(TransferLocation::Photo {
            id: p.id as u64,
            access_hash: p.access_hash as u64,
            file_reference: p.file_reference.clone(),
            thumb_size: p.thumb_size.clone(),
        }),
        tl_enums::InputFileLocation::InputDocumentFileLocation(d) => {
            Some(TransferLocation::Document {
                id: d.id as u64,
                access_hash: d.access_hash as u64,
                file_reference: d.file_reference.clone(),
                thumb_size: d.thumb_size.clone(),
            })
        }
        _ => None,
    }
}

/// Updates the `file_reference` field inside a TL [`tl_enums::InputFileLocation`] in place.
/// Returns `true` if the reference changed.
pub fn apply_file_reference_to_tl_location(
    loc: &mut tl_enums::InputFileLocation,
    new_ref: Vec<u8>,
) -> bool {
    match loc {
        tl_enums::InputFileLocation::InputPhotoFileLocation(p) => {
            if p.file_reference == new_ref {
                return false;
            }
            p.file_reference = new_ref;
            true
        }
        tl_enums::InputFileLocation::InputDocumentFileLocation(d) => {
            if d.file_reference == new_ref {
                return false;
            }
            d.file_reference = new_ref;
            true
        }
        _ => false,
    }
}

/// Discovers a [`FileOrigin`] (message or sticker set) and optional sticker set `access_hash`
/// by scanning loaded `Session` histories when the caller did not supply an explicit origin.
fn infer_origin_from_session(
    session: &Session,
    location: &TransferLocation,
) -> (FileOrigin, Option<u64>) {
    let target_photo = match location {
        TransferLocation::Photo { id, .. } | TransferLocation::PeerPhoto { photo_id: id, .. } => {
            Some(*id)
        }
        _ => None,
    };
    let target_doc = match location {
        TransferLocation::Document { id, .. } => Some(*id),
        _ => None,
    };
    let mut sticker_origin: Option<(u64, u64)> = None;

    for peer_id in known_peer_ids(session) {
        for msg in history_messages_ordered(session, peer_id) {
            match msg.media() {
                Some(MessageMedia::Photo(p)) if Some(p.id.0) == target_photo => {
                    return (
                        FileOrigin {
                            peer_id: Some(peer_id.0),
                            msg_id: Some(msg.id.0 as i32),
                            sticker_set_id: None,
                        },
                        None,
                    );
                }
                Some(MessageMedia::Document(d)) if Some(d.id.0) == target_doc => {
                    if let Some(ref sm) = d.sticker
                        && sm.set_id != 0
                    {
                        sticker_origin = Some((sm.set_id, sm.set_access_hash));
                    }
                    return (
                        FileOrigin {
                            peer_id: Some(peer_id.0),
                            msg_id: Some(msg.id.0 as i32),
                            sticker_set_id: sticker_origin.map(|(id, _)| id),
                        },
                        sticker_origin.map(|(_, ah)| ah),
                    );
                }
                _ => {}
            }
        }
    }

    (FileOrigin::default(), None)
}

/// Refetches the origin message, user/chat photo, sticker set, or custom emoji document via
/// MTProto and returns the refreshed `file_reference` bytes for `location`, updating `session`
/// in place (`ApiWrap::refreshFileReference` in `apiwrap.cpp:2886-3100`).
pub async fn refresh_file_reference_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    session: &Arc<RwLock<Session>>,
    origin: &FileOrigin,
    location: &TransferLocation,
) -> tg_media::error::Result<Vec<u8>> {
    let (resolved_origin, inferred_set_hash) =
        if origin.peer_id.is_none() && origin.msg_id.is_none() && origin.sticker_set_id.is_none() {
            let guard = session.read().await;
            infer_origin_from_session(&guard, location)
        } else {
            let set_hash = if let Some(target_set) = origin.sticker_set_id {
                let guard = session.read().await;
                let mut found = None;
                for pid in known_peer_ids(&guard) {
                    for msg in history_messages_ordered(&guard, pid) {
                        if let Some(MessageMedia::Document(d)) = msg.media()
                            && let Some(ref sm) = d.sticker
                            && sm.set_id == target_set
                        {
                            found = Some(sm.set_access_hash);
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }
                found
            } else {
                None
            };
            (origin.clone(), set_hash)
        };

    let mut updated = UpdatedFileReferences::default();

    // 1. Message origin: `channels.getMessages` for channels, `messages.getMessages` otherwise.
    if let (Some(raw_peer), Some(msg_id)) = (resolved_origin.peer_id, resolved_origin.msg_id) {
        let peer_id = PeerId(raw_peer);
        let input_msg = vec![tl_enums::InputMessage::ID(tl_types::InputMessageID {
            id: msg_id,
        })];
        let resp = if let Some(ch_id) = peer_id.to_channel() {
            let input_ch = {
                let guard = session.read().await;
                input_channel_for(&guard, ch_id)
            };
            if matches!(input_ch, tl_enums::InputChannel::Empty) {
                Err(MediaError::Rpc {
                    code: 400,
                    message: "channel not found in session for file reference refresh".into(),
                })
            } else {
                let req = tl_fns::channels::GetMessages {
                    channel: input_ch,
                    id: input_msg,
                };
                tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req))
                    .await
                    .map_err(|_| MediaError::Rpc {
                        code: 408,
                        message: "channels.getMessages timed out during file reference refresh"
                            .into(),
                    })?
                    .map_err(map_rpc_err)
            }
        } else {
            let req = tl_fns::messages::GetMessages { id: input_msg };
            tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req))
                .await
                .map_err(|_| MediaError::Rpc {
                    code: 408,
                    message: "messages.getMessages timed out during file reference refresh".into(),
                })?
                .map_err(map_rpc_err)
        }?;

        updated.push_messages(&resp);
        {
            let mut guard = session.write().await;
            match &resp {
                tl_enums::messages::Messages::Messages(m) => {
                    apply_tl_users_and_chats(&mut guard, &m.users, &m.chats);
                    for msg in &m.messages {
                        apply_tl_message(&mut guard, msg);
                    }
                }
                tl_enums::messages::Messages::Slice(m) => {
                    apply_tl_users_and_chats(&mut guard, &m.users, &m.chats);
                    for msg in &m.messages {
                        apply_tl_message(&mut guard, msg);
                    }
                }
                tl_enums::messages::Messages::ChannelMessages(m) => {
                    apply_tl_users_and_chats(&mut guard, &m.users, &m.chats);
                    for msg in &m.messages {
                        apply_tl_message(&mut guard, msg);
                    }
                }
                tl_enums::messages::Messages::NotModified(_) => {}
            }
            updated.apply_to_session(&mut guard);
        }
        if let Some(fresh) = updated.lookup_transfer(location) {
            return Ok(fresh);
        }
    }

    // 2. Sticker set origin: `messages.getStickerSet`.
    if let Some(set_id) = resolved_origin.sticker_set_id {
        let access_hash = match location {
            TransferLocation::StickerSetThumb { access_hash, .. } => *access_hash,
            _ => inferred_set_hash.unwrap_or(0),
        };
        let req = tl_fns::messages::GetStickerSet {
            stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id as i64,
                access_hash: access_hash as i64,
            }),
            hash: 0,
        };
        let resp = tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req))
            .await
            .map_err(|_| MediaError::Rpc {
                code: 408,
                message: "messages.getStickerSet timed out during file reference refresh".into(),
            })?
            .map_err(map_rpc_err)?;
        updated.push_sticker_set(&resp);
        {
            let mut guard = session.write().await;
            updated.apply_to_session(&mut guard);
        }
        if let Some(fresh) = updated.lookup_transfer(location) {
            return Ok(fresh);
        }
    }

    // 3. Peer/User profile photo origin: `photos.getUserPhotos` / `users.getFullUser` /
    //    `channels.getFullChannel` / `messages.getFullChat`.
    if let Some(raw_peer) = resolved_origin.peer_id
        && resolved_origin.msg_id.is_none()
    {
        let peer_id = PeerId(raw_peer);
        if let Some(uid) = peer_id.to_user() {
            let input_user = {
                let guard = session.read().await;
                input_user_for(&guard, uid)
            };
            if !matches!(input_user, tl_enums::InputUser::Empty) {
                let photo_id = match location {
                    TransferLocation::Photo { id, .. }
                    | TransferLocation::PeerPhoto { photo_id: id, .. } => *id as i64,
                    _ => 0,
                };
                if photo_id != 0 {
                    let req = tl_fns::photos::GetUserPhotos {
                        user_id: input_user.clone(),
                        offset: -1,
                        max_id: photo_id,
                        limit: 1,
                    };
                    if let Ok(Ok(resp)) =
                        tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await
                    {
                        updated.push_photos(&resp);
                    }
                }
                if updated.lookup_transfer(location).is_none() {
                    let req = tl_fns::users::GetFullUser { id: input_user };
                    if let Ok(Ok(resp)) =
                        tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await
                    {
                        updated.push_user_full(&resp);
                    }
                }
            }
        } else if let Some(ch_id) = peer_id.to_channel() {
            let input_ch = {
                let guard = session.read().await;
                input_channel_for(&guard, ch_id)
            };
            if !matches!(input_ch, tl_enums::InputChannel::Empty) {
                let req = tl_fns::channels::GetFullChannel { channel: input_ch };
                if let Ok(Ok(resp)) =
                    tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await
                {
                    updated.push_chat_full(&resp);
                }
            }
        } else if let Some(chat_id) = peer_id.to_chat() {
            let req = tl_fns::messages::GetFullChat {
                chat_id: chat_id.0 as i64,
            };
            if let Ok(Ok(resp)) = tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await {
                updated.push_chat_full(&resp);
            }
        }
        {
            let mut guard = session.write().await;
            updated.apply_to_session(&mut guard);
        }
        if let Some(fresh) = updated.lookup_transfer(location) {
            return Ok(fresh);
        }
    }

    // 4. Document fallback: try `messages.getCustomEmojiDocuments` for standalone custom emoji docs.
    if let TransferLocation::Document { id, .. } = location {
        let req = tl_fns::messages::GetCustomEmojiDocuments {
            document_id: vec![*id as i64],
        };
        if let Ok(Ok(docs)) = tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await {
            for doc in &docs {
                updated.push_document(doc);
            }
            {
                let mut guard = session.write().await;
                updated.apply_to_session(&mut guard);
            }
            if let Some(fresh) = updated.lookup_transfer(location) {
                return Ok(fresh);
            }
        }
    }

    Err(MediaError::Rpc {
        code: 400,
        message: "FILE_REFERENCE_EXPIRED: refreshed origin did not return a new file reference"
            .into(),
    })
}

/// Downloads a single 128 KiB aligned part with full CDN redirect, AES-256-CTR decryption,
/// `upload.reuploadCdnFile`, `upload.getCdnFileHashes`, SHA-256 hash verification, and
/// automatic `FILE_REFERENCE_EXPIRED` refetch-and-retry when `refresh_ctx` is provided.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_part_with_cdn_and_refresh<C: RpcClient + Clone + 'static>(
    rpc: &C,
    home_dc: i32,
    location: &mut tl_enums::InputFileLocation,
    offset: u64,
    part_size: u32,
    cdn: &mut Option<ActiveCdnState>,
    cdn_allowed: &mut bool,
    refresh_ctx: Option<(&Arc<RwLock<Session>>, &FileOrigin)>,
) -> anyhow::Result<Vec<u8>> {
    const MAX_REUPLOADS_PER_PART: usize = 3;
    let mut reuploads = 0usize;
    let mut refreshed_file_ref = false;

    loop {
        if let Some(active) = cdn.as_mut() {
            validate_get_cdn_file_request(offset, part_size)
                .map_err(|e| anyhow!("invalid upload.getCdnFile params: {e}"))?;
            let req = tl_fns::upload::GetCdnFile {
                file_token: active.file_token.clone(),
                offset: i64::try_from(offset).map_err(|_| anyhow!("CDN file offset overflow"))?,
                limit: part_size as i32,
            };
            let cdn_resp =
                tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke_in_dc(active.cdn_dc_id, &req))
                    .await
                    .map_err(|_| anyhow!("upload.getCdnFile timed out at offset {offset}"))?;

            match cdn_resp {
                Ok(tl_enums::upload::CdnFile::CdnFile(file)) => {
                    if file.bytes.is_empty() {
                        return Ok(Vec::new());
                    }
                    if file.bytes.len() > part_size as usize {
                        return Err(anyhow!(
                            "CDN chunk size {} exceeds part size {}",
                            file.bytes.len(),
                            part_size
                        ));
                    }
                    let mut decrypted = file.bytes;
                    apply_cdn_aes_ctr(&mut decrypted, &active.key, &active.iv, offset)
                        .map_err(|e| anyhow!("CDN AES-256-CTR decryption failed: {e}"))?;
                    match active.verifier.check(offset, &decrypted) {
                        CheckCdnHashResult::Good => return Ok(decrypted),
                        CheckCdnHashResult::Invalid => {
                            return Err(anyhow!(
                                "CDN file SHA-256 hash mismatch at offset {offset}"
                            ));
                        }
                        CheckCdnHashResult::NoHash => {
                            let hash_req = tl_fns::upload::GetCdnFileHashes {
                                file_token: active.file_token.clone(),
                                offset: i64::try_from(offset)
                                    .map_err(|_| anyhow!("CDN hash offset overflow"))?,
                            };
                            let hash_resp = tokio::time::timeout(
                                MEDIA_RPC_TIMEOUT,
                                rpc.invoke_in_dc(home_dc, &hash_req),
                            )
                            .await
                            .map_err(|_| {
                                anyhow!("upload.getCdnFileHashes timed out at offset {offset}")
                            })?;
                            let hashes = match hash_resp {
                                Ok(h) => h,
                                Err(e) => {
                                    let inv: tg_api::rpc::InvocationError = e.into();
                                    if is_cdn_token_invalid_err(&inv) {
                                        *cdn = None;
                                        *cdn_allowed = false;
                                        continue;
                                    }
                                    return Err(anyhow!("upload.getCdnFileHashes failed: {inv}"));
                                }
                            };
                            active.verifier.add_hashes(map_file_hashes(hashes));
                            match active.verifier.check(offset, &decrypted) {
                                CheckCdnHashResult::Good => return Ok(decrypted),
                                CheckCdnHashResult::Invalid => {
                                    return Err(anyhow!(
                                        "CDN file SHA-256 hash mismatch at offset {offset}"
                                    ));
                                }
                                CheckCdnHashResult::NoHash => {
                                    return Err(anyhow!(
                                        "missing CDN file hash for offset {offset} after getCdnFileHashes"
                                    ));
                                }
                            }
                        }
                    }
                }
                Ok(tl_enums::upload::CdnFile::ReuploadNeeded(reup)) => {
                    reuploads += 1;
                    if reuploads > MAX_REUPLOADS_PER_PART {
                        return Err(anyhow!(
                            "upload.reuploadCdnFile exceeded retry limit at offset {offset}"
                        ));
                    }
                    let reup_req = tl_fns::upload::ReuploadCdnFile {
                        file_token: active.file_token.clone(),
                        request_token: reup.request_token,
                    };
                    let reup_resp = tokio::time::timeout(
                        MEDIA_RPC_TIMEOUT,
                        rpc.invoke_in_dc(home_dc, &reup_req),
                    )
                    .await
                    .map_err(|_| anyhow!("upload.reuploadCdnFile timed out at offset {offset}"))?;
                    let hashes = match reup_resp {
                        Ok(h) => h,
                        Err(e) => {
                            let inv: tg_api::rpc::InvocationError = e.into();
                            if is_cdn_token_invalid_err(&inv) {
                                *cdn = None;
                                *cdn_allowed = false;
                                continue;
                            }
                            return Err(anyhow!("upload.reuploadCdnFile failed: {inv}"));
                        }
                    };
                    active.verifier.add_hashes(map_file_hashes(hashes));
                    continue;
                }
                Err(e) => {
                    let inv: tg_api::rpc::InvocationError = e.into();
                    if is_cdn_token_invalid_err(&inv) {
                        // Fall back to home DC without CDN on expired/invalid CDN token.
                        *cdn = None;
                        *cdn_allowed = false;
                        continue;
                    }
                    return Err(anyhow!("upload.getCdnFile failed: {inv}"));
                }
            }
        }

        validate_get_file_request(offset, part_size, false)
            .map_err(|e| anyhow!("invalid upload.getFile params: {e}"))?;
        let req = tl_fns::upload::GetFile {
            precise: false,
            cdn_supported: *cdn_allowed,
            location: location.clone(),
            offset: i64::try_from(offset).map_err(|_| anyhow!("file offset overflow"))?,
            limit: part_size as i32,
        };
        let rpc_res = tokio::time::timeout(MEDIA_RPC_TIMEOUT, rpc.invoke_in_dc(home_dc, &req))
            .await
            .map_err(|_| anyhow!("upload.getFile timed out at offset {offset}"))?;

        let resp = match rpc_res {
            Ok(val) => val,
            Err(e) => {
                let inv: tg_api::rpc::InvocationError = e.into();
                if !refreshed_file_ref
                    && is_file_reference_error(&inv)
                    && let Some((session, origin)) = refresh_ctx
                    && let Some(transfer_loc) = tl_location_to_transfer(location)
                {
                    refreshed_file_ref = true;
                    let fresh = refresh_file_reference_async(rpc, session, origin, &transfer_loc)
                        .await
                        .map_err(|re| anyhow!("file reference refresh failed: {re}"))?;
                    if apply_file_reference_to_tl_location(location, fresh) {
                        continue;
                    }
                }
                return Err(anyhow!("upload.getFile failed: {inv}"));
            }
        };

        match resp {
            tl_enums::upload::File::File(file) => return Ok(file.bytes),
            tl_enums::upload::File::CdnRedirect(redir) => {
                *cdn = Some(ActiveCdnState::from_tl(redir)?);
            }
        }
    }
}

/// Downloads a complete file over `upload.getFile` (including CDN redirects and automatic
/// `FILE_REFERENCE_EXPIRED` refetch-and-retry via `session` + `origin`) in aligned 128 KiB chunks.
pub async fn download_file_bytes_with_refresh_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    session: &Arc<RwLock<Session>>,
    origin: FileOrigin,
    dc_id: i32,
    mut location: tl_enums::InputFileLocation,
    expected_size: Option<u64>,
    mut on_progress: impl FnMut(u64, u64),
) -> anyhow::Result<Vec<u8>> {
    let target_dc = if dc_id > 0 { dc_id } else { rpc.main_dc_id() };
    let part_size = DOWNLOAD_PART_SIZE;
    let mut offset: u64 = 0;
    let total_hint = expected_size.unwrap_or(0);
    let mut out = Vec::with_capacity(total_hint.min(4 * 1024 * 1024) as usize);
    let mut cdn: Option<ActiveCdnState> = None;
    let mut cdn_allowed = true;

    const MAX_IN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

    loop {
        let chunk = download_part_with_cdn_and_refresh(
            rpc,
            target_dc,
            &mut location,
            offset,
            part_size,
            &mut cdn,
            &mut cdn_allowed,
            Some((session, &origin)),
        )
        .await?;

        let chunk_len = chunk.len();
        out.extend_from_slice(&chunk);
        offset = offset.saturating_add(chunk_len as u64);
        on_progress(offset, total_hint);

        if chunk_len == 0 || chunk_len < part_size as usize {
            break;
        }
        if let Some(expected) = expected_size
            && expected > 0
            && offset >= expected
        {
            out.truncate(expected as usize);
            on_progress(expected, expected);
            break;
        }
        if offset >= MAX_IN_MEMORY_BYTES {
            return Err(anyhow!("file exceeds in-memory download cap"));
        }
    }

    Ok(out)
}

/// Downloads a complete file over `upload.getFile` (including CDN redirects with AES-256-CTR
/// decryption and SHA-256 hash verification) in aligned 128 KiB chunks, invoking `on_progress`
/// after each chunk.
pub async fn download_file_bytes_with_progress_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    dc_id: i32,
    mut location: tl_enums::InputFileLocation,
    expected_size: Option<u64>,
    mut on_progress: impl FnMut(u64, u64),
) -> anyhow::Result<Vec<u8>> {
    let target_dc = if dc_id > 0 { dc_id } else { rpc.main_dc_id() };
    let part_size = DOWNLOAD_PART_SIZE;
    let mut offset: u64 = 0;
    let total_hint = expected_size.unwrap_or(0);
    let mut out = Vec::with_capacity(total_hint.min(4 * 1024 * 1024) as usize);
    let mut cdn: Option<ActiveCdnState> = None;
    let mut cdn_allowed = true;

    // Guard against unbounded loops on misbehaving servers (~256 MiB safety cap for full-in-memory reads).
    const MAX_IN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

    loop {
        let chunk = download_part_with_cdn_and_refresh(
            rpc,
            target_dc,
            &mut location,
            offset,
            part_size,
            &mut cdn,
            &mut cdn_allowed,
            None,
        )
        .await?;

        let chunk_len = chunk.len();
        out.extend_from_slice(&chunk);
        offset = offset.saturating_add(chunk_len as u64);
        on_progress(offset, total_hint);

        if chunk_len == 0 || chunk_len < part_size as usize {
            break;
        }
        if let Some(expected) = expected_size
            && expected > 0
            && offset >= expected
        {
            out.truncate(expected as usize);
            on_progress(expected, expected);
            break;
        }
        if offset >= MAX_IN_MEMORY_BYTES {
            return Err(anyhow!("file exceeds in-memory download cap"));
        }
    }

    Ok(out)
}

/// Downloads a complete file over `upload.getFile` (with CDN redirect support) in aligned 128 KiB chunks.
pub async fn download_file_bytes_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    dc_id: i32,
    location: tl_enums::InputFileLocation,
    expected_size: Option<u64>,
) -> anyhow::Result<Vec<u8>> {
    download_file_bytes_with_progress_async(rpc, dc_id, location, expected_size, |_, _| {}).await
}

/// Blocking wrapper around [`download_file_bytes_async`].
pub fn download_file_bytes<C: RpcClient + Clone + 'static>(
    rpc: &C,
    handle: &Handle,
    dc_id: i32,
    location: tl_enums::InputFileLocation,
    expected_size: Option<u64>,
) -> anyhow::Result<Vec<u8>> {
    let rpc = rpc.clone();
    block_on_runtime(handle, async move {
        download_file_bytes_async(&rpc, dc_id, location, expected_size).await
    })
}

/// `tg-media` [`MediaInvoker`] implementation backed by any `tg-api` [`RpcClient`].
#[derive(Clone)]
pub struct TelegramMediaInvoker<C: RpcClient + Clone + 'static> {
    rpc: C,
    session: Arc<RwLock<Session>>,
}

impl<C: RpcClient + Clone + 'static> TelegramMediaInvoker<C> {
    /// Creates a new [`TelegramMediaInvoker`] over `rpc` and `session`.
    #[must_use]
    pub fn new(rpc: C, session: Arc<RwLock<Session>>) -> Self {
        Self { rpc, session }
    }

    async fn to_tl_location(
        &self,
        loc: &TransferLocation,
    ) -> std::result::Result<tl_enums::InputFileLocation, MediaError> {
        match loc {
            TransferLocation::Document {
                id,
                access_hash,
                file_reference,
                thumb_size,
            } => Ok(tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: *id as i64,
                    access_hash: *access_hash as i64,
                    file_reference: file_reference.clone(),
                    thumb_size: thumb_size.clone(),
                },
            )),
            TransferLocation::Photo {
                id,
                access_hash,
                file_reference,
                thumb_size,
            } => Ok(tl_enums::InputFileLocation::InputPhotoFileLocation(
                tl_types::InputPhotoFileLocation {
                    id: *id as i64,
                    access_hash: *access_hash as i64,
                    file_reference: file_reference.clone(),
                    thumb_size: thumb_size.clone(),
                },
            )),
            TransferLocation::PeerPhoto {
                peer_id,
                photo_id,
                big,
                ..
            } => {
                let session = self.session.read().await;
                let input_peer = input_peer_for(&session, PeerId(*peer_id));
                Ok(tl_enums::InputFileLocation::InputPeerPhotoFileLocation(
                    tl_types::InputPeerPhotoFileLocation {
                        big: *big,
                        peer: input_peer,
                        photo_id: *photo_id as i64,
                    },
                ))
            }
            TransferLocation::StickerSetThumb {
                set_id,
                access_hash,
                thumb_version,
            } => Ok(tl_enums::InputFileLocation::InputStickerSetThumb(
                tl_types::InputStickerSetThumb {
                    stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                        id: *set_id as i64,
                        access_hash: *access_hash as i64,
                    }),
                    thumb_version: *thumb_version,
                },
            )),
            TransferLocation::WebFile { .. } => Err(MediaError::InvalidTransferParam(
                "WebFile location must use upload.getWebFile".into(),
            )),
        }
    }
}

fn map_rpc_err<E: Into<tg_api::rpc::InvocationError>>(err: E) -> MediaError {
    let inv: tg_api::rpc::InvocationError = err.into();
    let code = match &inv {
        tg_api::rpc::InvocationError::Rpc(r) => r.code,
        _ => 500,
    };
    MediaError::Rpc {
        code,
        message: inv.to_string(),
    }
}

fn map_file_hashes(hashes: Vec<tl_enums::FileHash>) -> Vec<CdnFileHash> {
    hashes
        .into_iter()
        .map(|h| match h {
            tl_enums::FileHash::FileHash(h) => CdnFileHash {
                offset: u64::try_from(h.offset).unwrap_or(0),
                limit: u32::try_from(h.limit).unwrap_or(0),
                hash: h.hash,
            },
        })
        .collect()
}

impl<C: RpcClient + Clone + 'static> MediaInvoker for TelegramMediaInvoker<C> {
    fn get_file(
        &self,
        dc_id: i32,
        _session_index: usize,
        req: GetFileRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<GetFileResponse>> {
        Box::pin(async move {
            validate_get_file_request(req.offset, req.limit, req.precise)?;
            let location = self.to_tl_location(&req.location).await?;
            let target_dc = if dc_id > 0 {
                dc_id
            } else {
                self.rpc.main_dc_id()
            };
            let tl_req = tl_fns::upload::GetFile {
                precise: req.precise,
                cdn_supported: req.cdn_supported,
                location,
                offset: i64::try_from(req.offset)
                    .map_err(|_| MediaError::InvalidTransferParam("offset overflow".into()))?,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(target_dc, &tl_req)
                .await
                .map_err(map_rpc_err)?;
            match resp {
                tl_enums::upload::File::File(file) => {
                    Ok(GetFileResponse::File { bytes: file.bytes })
                }
                tl_enums::upload::File::CdnRedirect(redir) => {
                    Ok(GetFileResponse::CdnRedirect(CdnRedirect {
                        dc_id: redir.dc_id,
                        file_token: redir.file_token,
                        encryption_key: redir.encryption_key,
                        encryption_iv: redir.encryption_iv,
                        file_hashes: map_file_hashes(redir.file_hashes),
                    }))
                }
            }
        })
    }

    fn get_web_file(
        &self,
        dc_id: i32,
        _session_index: usize,
        req: GetWebFileRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<GetWebFileResponse>> {
        Box::pin(async move {
            let TransferLocation::WebFile { url, access_hash } = req.location else {
                return Err(MediaError::InvalidTransferParam(
                    "upload.getWebFile requires TransferLocation::WebFile".into(),
                ));
            };
            let url_str = String::from_utf8(url).map_err(|_| {
                MediaError::InvalidTransferParam("WebFile URL is not valid UTF-8".into())
            })?;
            let target_dc = if dc_id > 0 {
                dc_id
            } else {
                self.rpc.main_dc_id()
            };
            let tl_req = tl_fns::upload::GetWebFile {
                location: tl_enums::InputWebFileLocation::InputWebFileLocation(
                    tl_types::InputWebFileLocation {
                        url: url_str,
                        access_hash: access_hash as i64,
                    },
                ),
                offset: i32::try_from(req.offset).map_err(|_| {
                    MediaError::InvalidTransferParam("web file offset overflow".into())
                })?,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(target_dc, &tl_req)
                .await
                .map_err(map_rpc_err)?;
            let tl_enums::upload::WebFile::WebFile(wf) = resp;
            Ok(GetWebFileResponse {
                size: u64::try_from(wf.size).unwrap_or(0),
                mime_type: wf.mime_type,
                bytes: wf.bytes,
            })
        })
    }

    fn get_cdn_file(
        &self,
        cdn_dc_id: i32,
        _session_index: usize,
        req: GetCdnFileRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<GetCdnFileResponse>> {
        Box::pin(async move {
            let tl_req = tl_fns::upload::GetCdnFile {
                file_token: req.file_token,
                offset: i64::try_from(req.offset)
                    .map_err(|_| MediaError::InvalidTransferParam("cdn offset overflow".into()))?,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(cdn_dc_id, &tl_req)
                .await
                .map_err(map_rpc_err)?;
            match resp {
                tl_enums::upload::CdnFile::CdnFile(file) => {
                    Ok(GetCdnFileResponse::Bytes(file.bytes))
                }
                tl_enums::upload::CdnFile::ReuploadNeeded(reupload) => {
                    Ok(GetCdnFileResponse::ReuploadNeeded {
                        request_token: reupload.request_token,
                    })
                }
            }
        })
    }

    fn get_cdn_file_hashes(
        &self,
        dc_id: i32,
        _session_index: usize,
        file_token: Vec<u8>,
        offset: u64,
    ) -> BoxFuture<'_, tg_media::error::Result<Vec<CdnFileHash>>> {
        Box::pin(async move {
            let target_dc = if dc_id > 0 {
                dc_id
            } else {
                self.rpc.main_dc_id()
            };
            let tl_req = tl_fns::upload::GetCdnFileHashes {
                file_token,
                offset: i64::try_from(offset)
                    .map_err(|_| MediaError::InvalidTransferParam("hash offset overflow".into()))?,
            };
            let hashes = self
                .rpc
                .invoke_in_dc(target_dc, &tl_req)
                .await
                .map_err(map_rpc_err)?;
            Ok(map_file_hashes(hashes))
        })
    }

    fn reupload_cdn_file(
        &self,
        dc_id: i32,
        _session_index: usize,
        file_token: Vec<u8>,
        request_token: Vec<u8>,
    ) -> BoxFuture<'_, tg_media::error::Result<Vec<CdnFileHash>>> {
        Box::pin(async move {
            let target_dc = if dc_id > 0 {
                dc_id
            } else {
                self.rpc.main_dc_id()
            };
            let tl_req = tl_fns::upload::ReuploadCdnFile {
                file_token,
                request_token,
            };
            let hashes = self
                .rpc
                .invoke_in_dc(target_dc, &tl_req)
                .await
                .map_err(map_rpc_err)?;
            Ok(map_file_hashes(hashes))
        })
    }

    fn refresh_file_reference(
        &self,
        origin: FileOrigin,
        location: TransferLocation,
    ) -> BoxFuture<'_, tg_media::error::Result<Vec<u8>>> {
        Box::pin(async move {
            refresh_file_reference_async(&self.rpc, &self.session, &origin, &location).await
        })
    }

    fn save_file_part(
        &self,
        _session_index: usize,
        req: SaveFilePartRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<bool>> {
        Box::pin(async move {
            let tl_req = tl_fns::upload::SaveFilePart {
                file_id: req.file_id,
                file_part: req.file_part as i32,
                bytes: req.bytes,
            };
            self.rpc.invoke(&tl_req).await.map_err(map_rpc_err)
        })
    }

    fn save_big_file_part(
        &self,
        _session_index: usize,
        req: SaveBigFilePartRequest,
    ) -> BoxFuture<'_, tg_media::error::Result<bool>> {
        Box::pin(async move {
            let tl_req = tl_fns::upload::SaveBigFilePart {
                file_id: req.file_id,
                file_part: req.file_part as i32,
                file_total_parts: req.file_total_parts as i32,
                bytes: req.bytes,
            };
            self.rpc.invoke(&tl_req).await.map_err(map_rpc_err)
        })
    }
}

fn apply_file_reference_to_transfer(loc: &mut TransferLocation, new_ref: Vec<u8>) -> bool {
    match loc {
        TransferLocation::Photo { file_reference, .. }
        | TransferLocation::Document { file_reference, .. } => {
            if *file_reference == new_ref {
                return false;
            }
            *file_reference = new_ref;
            true
        }
        _ => false,
    }
}

fn is_media_cdn_token_invalid(err: &MediaError) -> bool {
    matches!(
        err,
        MediaError::Rpc { message, .. }
            if message.contains("FILE_TOKEN_INVALID") || message.contains("REQUEST_TOKEN_INVALID")
    )
}

fn is_media_file_ref_err(err: &MediaError) -> bool {
    matches!(err, MediaError::Rpc { message, .. } if message.contains("FILE_REFERENCE_"))
}

/// [`tg_storage::streamed_downloader::ChunkProvider`] backed by [`TelegramMediaInvoker`]
/// (`upload.getFile` with spec-compliant part alignment, CDN AES-256-CTR + SHA-256 verification,
/// automatic `FILE_REFERENCE_*` refresh, and stale-chunk cancellation tracking).
pub struct TelegramStreamChunkProvider<C: RpcClient + Clone + 'static> {
    invoker: TelegramMediaInvoker<C>,
    dc_id: i32,
    part_size: u32,
    location: Mutex<TransferLocation>,
    origin: Option<FileOrigin>,
    refresh_lock: tokio::sync::Mutex<()>,
    cdn: Mutex<Option<ActiveCdnState>>,
    cdn_allowed: AtomicBool,
    cancelled_offsets: Mutex<Vec<u64>>,
}

impl<C: RpcClient + Clone + 'static> TelegramStreamChunkProvider<C> {
    /// Creates a new [`TelegramStreamChunkProvider`] over `invoker` for `location`.
    #[must_use]
    pub fn new(
        invoker: TelegramMediaInvoker<C>,
        dc_id: i32,
        part_size: u32,
        location: TransferLocation,
        origin: Option<FileOrigin>,
    ) -> Self {
        Self {
            invoker,
            dc_id,
            part_size,
            location: Mutex::new(location),
            origin,
            refresh_lock: tokio::sync::Mutex::new(()),
            cdn: Mutex::new(None),
            cdn_allowed: AtomicBool::new(true),
            cancelled_offsets: Mutex::new(Vec::new()),
        }
    }

    /// Returns all chunk byte offsets cancelled via [`ChunkProvider::cancel_chunk`].
    #[must_use]
    pub fn cancelled_offsets(&self) -> Vec<u64> {
        self.cancelled_offsets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Returns the current [`TransferLocation`] (including any refreshed `file_reference`).
    #[must_use]
    pub fn current_location(&self) -> TransferLocation {
        self.location
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl<C: RpcClient + Clone + 'static> ChunkProvider for TelegramStreamChunkProvider<C> {
    fn fetch_chunk(&self, offset: u64, length: usize) -> ChunkFuture<'_> {
        Box::pin(async move {
            const MAX_REUPLOADS_PER_PART: usize = 3;
            let part_size = self.part_size;
            validate_get_file_request(offset, part_size, false).map_err(|err| {
                StreamedDownloadError::InvalidRange(format!(
                    "invalid upload.getFile params ({offset}, {part_size}): {err}"
                ))
            })?;

            let mut reuploads = 0usize;
            let mut refreshed_file_ref = false;

            loop {
                let active_cdn = self.cdn.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(mut active) = active_cdn {
                    validate_get_cdn_file_request(offset, part_size).map_err(|err| {
                        StreamedDownloadError::ProviderFailed {
                            offset,
                            message: format!("invalid upload.getCdnFile params: {err}"),
                        }
                    })?;
                    let cdn_req = GetCdnFileRequest {
                        file_token: active.file_token.clone(),
                        offset,
                        limit: part_size,
                    };
                    let cdn_res = tokio::time::timeout(
                        MEDIA_RPC_TIMEOUT,
                        self.invoker.get_cdn_file(active.cdn_dc_id, 0, cdn_req),
                    )
                    .await
                    .map_err(|_| StreamedDownloadError::ProviderFailed {
                        offset,
                        message: format!("upload.getCdnFile timed out at offset {offset}"),
                    })?;

                    match cdn_res {
                        Ok(GetCdnFileResponse::Bytes(mut decrypted)) => {
                            if decrypted.len() > part_size as usize {
                                return Err(StreamedDownloadError::ProviderFailed {
                                    offset,
                                    message: format!(
                                        "CDN chunk size {} exceeds part size {part_size}",
                                        decrypted.len()
                                    ),
                                });
                            }
                            apply_cdn_aes_ctr(&mut decrypted, &active.key, &active.iv, offset)
                                .map_err(|err| StreamedDownloadError::ProviderFailed {
                                    offset,
                                    message: format!("CDN AES-256-CTR decryption failed: {err}"),
                                })?;
                            let verified = match active.verifier.check(offset, &decrypted) {
                                CheckCdnHashResult::Good => true,
                                CheckCdnHashResult::Invalid => {
                                    return Err(StreamedDownloadError::ProviderFailed {
                                        offset,
                                        message: format!(
                                            "CDN file SHA-256 hash mismatch at offset {offset}"
                                        ),
                                    });
                                }
                                CheckCdnHashResult::NoHash => {
                                    let hashes_res = tokio::time::timeout(
                                        MEDIA_RPC_TIMEOUT,
                                        self.invoker.get_cdn_file_hashes(
                                            self.dc_id,
                                            0,
                                            active.file_token.clone(),
                                            offset,
                                        ),
                                    )
                                    .await
                                    .map_err(|_| StreamedDownloadError::ProviderFailed {
                                        offset,
                                        message: format!(
                                            "upload.getCdnFileHashes timed out at offset {offset}"
                                        ),
                                    })?;
                                    match hashes_res {
                                        Ok(hashes) => {
                                            active.verifier.add_hashes(hashes);
                                            *self.cdn.lock().unwrap_or_else(|e| e.into_inner()) =
                                                Some(active.clone());
                                            match active.verifier.check(offset, &decrypted) {
                                                CheckCdnHashResult::Good => true,
                                                CheckCdnHashResult::Invalid => {
                                                    return Err(
                                                        StreamedDownloadError::ProviderFailed {
                                                            offset,
                                                            message: format!(
                                                                "CDN file SHA-256 hash mismatch at offset {offset}"
                                                            ),
                                                        },
                                                    );
                                                }
                                                CheckCdnHashResult::NoHash => {
                                                    return Err(
                                                        StreamedDownloadError::ProviderFailed {
                                                            offset,
                                                            message: format!(
                                                                "missing CDN file hash for offset {offset}"
                                                            ),
                                                        },
                                                    );
                                                }
                                            }
                                        }
                                        Err(err) if is_media_cdn_token_invalid(&err) => {
                                            *self.cdn.lock().unwrap_or_else(|e| e.into_inner()) =
                                                None;
                                            self.cdn_allowed.store(false, Ordering::Relaxed);
                                            false
                                        }
                                        Err(err) => {
                                            return Err(StreamedDownloadError::ProviderFailed {
                                                offset,
                                                message: format!(
                                                    "upload.getCdnFileHashes failed: {err}"
                                                ),
                                            });
                                        }
                                    }
                                }
                            };
                            if !verified {
                                continue;
                            }
                            if decrypted.len() > length {
                                decrypted.truncate(length);
                            }
                            return Ok(decrypted);
                        }
                        Ok(GetCdnFileResponse::ReuploadNeeded { request_token }) => {
                            reuploads += 1;
                            if reuploads > MAX_REUPLOADS_PER_PART {
                                return Err(StreamedDownloadError::ProviderFailed {
                                    offset,
                                    message: format!(
                                        "upload.reuploadCdnFile exceeded retry limit at offset {offset}"
                                    ),
                                });
                            }
                            let reup_res = tokio::time::timeout(
                                MEDIA_RPC_TIMEOUT,
                                self.invoker.reupload_cdn_file(
                                    self.dc_id,
                                    0,
                                    active.file_token.clone(),
                                    request_token,
                                ),
                            )
                            .await
                            .map_err(|_| {
                                StreamedDownloadError::ProviderFailed {
                                    offset,
                                    message: format!(
                                        "upload.reuploadCdnFile timed out at offset {offset}"
                                    ),
                                }
                            })?;
                            match reup_res {
                                Ok(hashes) => {
                                    active.verifier.add_hashes(hashes);
                                    *self.cdn.lock().unwrap_or_else(|e| e.into_inner()) =
                                        Some(active);
                                    continue;
                                }
                                Err(err) if is_media_cdn_token_invalid(&err) => {
                                    *self.cdn.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                    self.cdn_allowed.store(false, Ordering::Relaxed);
                                    continue;
                                }
                                Err(err) => {
                                    return Err(StreamedDownloadError::ProviderFailed {
                                        offset,
                                        message: format!("upload.reuploadCdnFile failed: {err}"),
                                    });
                                }
                            }
                        }
                        Err(err) if is_media_cdn_token_invalid(&err) => {
                            *self.cdn.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            self.cdn_allowed.store(false, Ordering::Relaxed);
                            continue;
                        }
                        Err(err) => {
                            return Err(StreamedDownloadError::ProviderFailed {
                                offset,
                                message: format!("upload.getCdnFile failed: {err}"),
                            });
                        }
                    }
                }

                let current_loc = self.current_location();
                let req = GetFileRequest {
                    location: current_loc.clone(),
                    offset,
                    limit: part_size,
                    precise: false,
                    cdn_supported: self.cdn_allowed.load(Ordering::Relaxed),
                };
                let resp = tokio::time::timeout(
                    MEDIA_RPC_TIMEOUT,
                    self.invoker.get_file(self.dc_id, 0, req),
                )
                .await
                .map_err(|_| StreamedDownloadError::ProviderFailed {
                    offset,
                    message: format!("upload.getFile timed out at offset {offset}"),
                })?;

                match resp {
                    Ok(GetFileResponse::File { mut bytes }) => {
                        if bytes.len() > length {
                            bytes.truncate(length);
                        }
                        return Ok(bytes);
                    }
                    Ok(GetFileResponse::CdnRedirect(redir)) => {
                        let state = ActiveCdnState::from_cdn_redirect(redir).map_err(|err| {
                            StreamedDownloadError::ProviderFailed {
                                offset,
                                message: err.to_string(),
                            }
                        })?;
                        *self.cdn.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
                    }
                    Err(err) => {
                        if !refreshed_file_ref
                            && is_media_file_ref_err(&err)
                            && let Some(origin) = self.origin.clone()
                        {
                            refreshed_file_ref = true;
                            let _refresh_guard = self.refresh_lock.lock().await;
                            // Coalesce concurrent refreshes: if another chunk task already updated
                            // `self.location`, retry immediately without a duplicate origin RPC.
                            if self.current_location() != current_loc {
                                continue;
                            }
                            let fresh = self
                                .invoker
                                .refresh_file_reference(origin, current_loc)
                                .await
                                .map_err(|re| StreamedDownloadError::ProviderFailed {
                                    offset,
                                    message: format!("file reference refresh failed: {re}"),
                                })?;
                            let changed = {
                                let mut guard =
                                    self.location.lock().unwrap_or_else(|e| e.into_inner());
                                apply_file_reference_to_transfer(&mut guard, fresh)
                            };
                            if changed {
                                continue;
                            }
                        }
                        return Err(StreamedDownloadError::ProviderFailed {
                            offset,
                            message: err.to_string(),
                        });
                    }
                }
            }
        })
    }

    fn cancel_chunk(&self, offset: u64) {
        tracing::debug!(offset, dc_id = self.dc_id, "cancelled streamed media chunk");
        self.cancelled_offsets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(offset);
    }
}

type StreamedHandle<C> = (
    StreamedFileDownloader<TelegramStreamChunkProvider<C>>,
    Arc<TelegramStreamChunkProvider<C>>,
);

struct StreamedLoaderState<C: RpcClient + Clone + 'static> {
    downloader: StreamedFileDownloader<TelegramStreamChunkProvider<C>>,
    provider: Arc<TelegramStreamChunkProvider<C>>,
    ephemeral_dir: Option<PathBuf>,
}

/// Progressive byte-range loader implementing [`tg_video::RangeLoader`] over MTProto
/// `upload.getFile`, backed by [`tg_storage::streamed_downloader::StreamedFileDownloader`] and
/// [`TelegramMediaInvoker`] with 128 KiB part alignment, priority seek preemption, stale chunk
/// cancellation, automatic `FILE_REFERENCE_EXPIRED` refresh, and resumable `.part` / `.part.meta`
/// persistence.
pub struct TelegramRangeLoader<C: RpcClient + Clone + 'static> {
    rpc: C,
    handle: Handle,
    dc_id: i32,
    location: Mutex<tl_enums::InputFileLocation>,
    total_size: u64,
    cache_key: CacheKey,
    parts: Mutex<HashMap<u64, Vec<u8>>>,
    session_origin: Option<(Arc<RwLock<Session>>, FileOrigin)>,
    storage_dir: Option<PathBuf>,
    part_size: u32,
    max_in_flight: usize,
    expected_sha256: Option<[u8; 32]>,
    streamed: Mutex<Option<StreamedLoaderState<C>>>,
    bg_task: Mutex<Option<AbortHandle>>,
}

impl<C: RpcClient + Clone + 'static> TelegramRangeLoader<C> {
    /// Creates a new [`TelegramRangeLoader`] for `resolved`.
    #[must_use]
    pub fn new(rpc: C, handle: Handle, resolved: ResolvedMediaLocation) -> Self {
        Self {
            rpc,
            handle,
            dc_id: resolved.dc_id,
            location: Mutex::new(resolved.location),
            total_size: resolved.size,
            cache_key: resolved.cache_key,
            parts: Mutex::new(HashMap::new()),
            session_origin: None,
            storage_dir: None,
            part_size: DOWNLOAD_PART_SIZE,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT_PARTS,
            expected_sha256: None,
            streamed: Mutex::new(None),
            bg_task: Mutex::new(None),
        }
    }

    /// Attaches a session and [`FileOrigin`] so expired file references refresh automatically.
    #[must_use]
    pub fn with_refresh_origin(
        mut self,
        session: Arc<RwLock<Session>>,
        origin: FileOrigin,
    ) -> Self {
        self.session_origin = Some((session, origin));
        self
    }

    /// Configures a persistent directory for resumable `.part` / `.part.meta` and finalized
    /// media files so reopening a [`TelegramRangeLoader`] for the same media resumes downloaded
    /// parts without re-fetching them over MTProto.
    #[must_use]
    pub fn with_storage_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.storage_dir = Some(dir.into());
        self
    }

    /// Overrides the aligned `upload.getFile` part size (defaults to `128 KiB`).
    /// Invalid part sizes are ignored and keep the default `DOWNLOAD_PART_SIZE`.
    #[must_use]
    pub fn with_part_size(mut self, part_size: u32) -> Self {
        if validate_get_file_request(0, part_size, false).is_ok() {
            self.part_size = part_size;
        }
        self
    }

    /// Overrides the maximum concurrent in-flight part requests in [`StreamedFileDownloader`].
    #[must_use]
    pub fn with_max_in_flight(mut self, max_in_flight: usize) -> Self {
        if max_in_flight > 0 {
            self.max_in_flight = max_in_flight;
        }
        self
    }

    /// Configures an optional expected SHA-256 digest checked before finalizing the downloaded
    /// file in [`StreamedFileDownloader::download_all`].
    #[must_use]
    pub fn with_expected_sha256(mut self, sha256: [u8; 32]) -> Self {
        self.expected_sha256 = Some(sha256);
        self
    }

    fn persistent_target_path(&self) -> Option<PathBuf> {
        self.storage_dir.as_ref().map(|dir| {
            dir.join(format!(
                "stream_{:016x}_{:016x}.bin",
                self.cache_key.high, self.cache_key.low
            ))
        })
    }

    fn read_from_finalized_target(
        &self,
        offset: u64,
        len: usize,
    ) -> Option<tg_video::error::Result<Vec<u8>>> {
        let target = self.persistent_target_path()?;
        let meta = fs::metadata(&target).ok()?;
        if !meta.is_file() || meta.len() != self.total_size {
            return None;
        }
        let res = (|| -> std::io::Result<Vec<u8>> {
            let mut file = File::open(&target)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf)?;
            Ok(buf)
        })();
        Some(res.map_err(|_| StreamingError::LoadFailed.into()))
    }

    fn ensure_streamed(&self) -> Option<StreamedHandle<C>> {
        if self.total_size == 0 {
            return None;
        }
        let mut guard = self.streamed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref state) = *guard {
            return Some((state.downloader.clone(), Arc::clone(&state.provider)));
        }

        let tl_loc = self
            .location
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let transfer_loc = tl_location_to_transfer(&tl_loc)?;

        let (session, origin) = match &self.session_origin {
            Some((s, o)) => (Arc::clone(s), Some(o.clone())),
            None => (Arc::new(RwLock::new(Session::new(UserId(0)))), None),
        };
        let invoker = TelegramMediaInvoker::new(self.rpc.clone(), session);
        let provider = Arc::new(TelegramStreamChunkProvider::new(
            invoker,
            self.dc_id,
            self.part_size,
            transfer_loc,
            origin,
        ));

        let (target_path, ephemeral_dir) = if let Some(target) = self.persistent_target_path() {
            (target, None)
        } else {
            let dir = std::env::temp_dir().join(format!(
                "rustgram_stream_{:016x}_{:016x}_{:016x}",
                self.cache_key.high,
                self.cache_key.low,
                random_u64()
            ));
            let target = dir.join("stream.bin");
            (target, Some(dir))
        };

        let mut cfg =
            StreamedDownloaderConfig::new(self.cache_key.high, self.total_size, target_path);
        cfg.cache_key = StorageCacheKey::new(self.cache_key.high, self.cache_key.low);
        cfg.part_size = self.part_size as usize;
        cfg.max_in_flight = self.max_in_flight;
        cfg.expected_sha256 = self.expected_sha256;

        let downloader = StreamedFileDownloader::open(cfg, Arc::clone(&provider)).ok()?;
        let out = (downloader.clone(), Arc::clone(&provider));
        *guard = Some(StreamedLoaderState {
            downloader,
            provider,
            ephemeral_dir,
        });
        Some(out)
    }

    /// Returns the underlying [`StreamedFileDownloader`], initializing it if `total_size > 0`.
    #[must_use]
    pub fn downloader(&self) -> Option<StreamedFileDownloader<TelegramStreamChunkProvider<C>>> {
        self.ensure_streamed().map(|(dl, _)| dl)
    }

    /// Returns the current [`DownloadProgress`] snapshot from [`StreamedFileDownloader`], if active.
    #[must_use]
    pub fn progress(&self) -> Option<DownloadProgress> {
        self.ensure_streamed().map(|(dl, _)| dl.progress())
    }

    /// Returns the sparse `.part` file path managed by [`StreamedFileDownloader`], if active.
    #[must_use]
    pub fn part_path(&self) -> Option<PathBuf> {
        self.ensure_streamed()
            .map(|(dl, _)| dl.part_path().to_path_buf())
    }

    /// Returns the resumable `.part.meta` file path managed by [`StreamedFileDownloader`], if active.
    #[must_use]
    pub fn meta_path(&self) -> Option<PathBuf> {
        self.ensure_streamed()
            .map(|(dl, _)| dl.meta_path().to_path_buf())
    }

    /// Returns the finalized destination file path managed by [`StreamedFileDownloader`], if active.
    #[must_use]
    pub fn target_path(&self) -> Option<PathBuf> {
        self.ensure_streamed()
            .map(|(dl, _)| dl.target_path().to_path_buf())
    }

    /// Returns all chunk byte offsets whose in-flight requests were cancelled by seek preemption
    /// or [`RangeLoader::stop`].
    #[must_use]
    pub fn cancelled_chunks(&self) -> Vec<u64> {
        let guard = self.streamed.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .map(|s| s.provider.cancelled_offsets())
            .unwrap_or_default()
    }

    /// Prioritizes byte range `[offset, offset + len)` in [`StreamedFileDownloader`], preempting
    /// and cancelling lower-priority background in-flight chunks outside the range.
    pub fn seek_and_prioritize(&self, offset: u64, len: usize) -> StreamedDownloadResult<Vec<u64>> {
        let Some((dl, _)) = self.ensure_streamed() else {
            return Ok(Vec::new());
        };
        let clamped = self.total_size.saturating_sub(offset).min(len as u64) as usize;
        dl.seek_and_prioritize(offset, clamped)
    }

    /// Starts background sequential completion via [`StreamedFileDownloader::download_all`] on
    /// this loader's Tokio runtime (idempotent; aborted automatically on [`RangeLoader::stop`]
    /// or `Drop`).
    pub fn start_background_download(&self) -> bool {
        let Some((dl, _)) = self.ensure_streamed() else {
            return false;
        };
        let mut bg = self.bg_task.lock().unwrap_or_else(|e| e.into_inner());
        if bg.is_some() {
            return false;
        }
        let handle = self.handle.spawn(async move {
            let _ = dl.download_all().await;
        });
        *bg = Some(handle.abort_handle());
        true
    }

    /// Drives [`StreamedFileDownloader::download_all`] to completion and returns the finalized
    /// file path on disk.
    pub fn download_to_file(&self) -> StreamedDownloadResult<PathBuf> {
        if let Some(target) = self.persistent_target_path()
            && fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() == self.total_size)
        {
            return Ok(target);
        }
        let Some((dl, _)) = self.ensure_streamed() else {
            return Err(StreamedDownloadError::InvalidConfig(
                "streamed downloader unavailable for unknown-size media".into(),
            ));
        };
        block_on_runtime(&self.handle, dl.download_all())
    }

    fn fetch_aligned_part_fallback(&self, part_offset: u64) -> tg_video::error::Result<Vec<u8>> {
        if let Ok(guard) = self.parts.lock()
            && let Some(cached) = guard.get(&part_offset)
        {
            return Ok(cached.clone());
        }

        let part_size = self.part_size;
        validate_get_file_request(part_offset, part_size, false)
            .map_err(|_| StreamingError::LoadFailed)?;

        let rpc = self.rpc.clone();
        let target_dc = if self.dc_id > 0 {
            self.dc_id
        } else {
            rpc.main_dc_id()
        };
        let mut location = self
            .location
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let session_origin = self.session_origin.clone();
        let (bytes, updated_loc) = block_on_runtime(&self.handle, async move {
            let mut cdn = None;
            let mut cdn_allowed = true;
            let refresh_ref = session_origin.as_ref().map(|(s, o)| (s, o));
            let res = download_part_with_cdn_and_refresh(
                &rpc,
                target_dc,
                &mut location,
                part_offset,
                part_size,
                &mut cdn,
                &mut cdn_allowed,
                refresh_ref,
            )
            .await
            .map_err(|_| StreamingError::LoadFailed)?;
            Ok::<_, tg_video::error::VideoError>((res, location))
        })?;

        if let Ok(mut guard) = self.location.lock() {
            *guard = updated_loc;
        }
        if let Ok(mut guard) = self.parts.lock() {
            guard.insert(part_offset, bytes.clone());
        }
        Ok(bytes)
    }
}

impl<C: RpcClient + Clone + 'static> Drop for TelegramRangeLoader<C> {
    fn drop(&mut self) {
        if let Some(abort) = self
            .bg_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            abort.abort();
        }
        let ephemeral = {
            let mut guard = self.streamed.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = guard.take() {
                state.downloader.cancel();
                state.ephemeral_dir
            } else {
                None
            }
        };
        if let Some(dir) = ephemeral {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

impl<C: RpcClient + Clone + 'static> RangeLoader for TelegramRangeLoader<C> {
    fn size(&self) -> u64 {
        self.total_size
    }

    fn base_cache_key(&self) -> CacheKey {
        self.cache_key
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::error::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        if self.total_size > 0 && offset >= self.total_size {
            return Err(StreamingError::LoadFailed.into());
        }

        let target_len = if self.total_size > 0 {
            (self.total_size - offset).min(len as u64) as usize
        } else {
            len
        };

        if self.total_size > 0 {
            if let Some(res) = self.read_from_finalized_target(offset, target_len) {
                return res;
            }
            if let Some((dl, _)) = self.ensure_streamed() {
                return block_on_runtime(&self.handle, dl.read_range(offset, target_len))
                    .map_err(|_| StreamingError::LoadFailed.into());
            }
        }

        let mut out = Vec::with_capacity(target_len);
        let mut cursor = offset;

        while out.len() < target_len {
            let part_offset = align_download_offset(cursor, self.part_size);
            let part = self.fetch_aligned_part_fallback(part_offset)?;
            let inner_offset = (cursor - part_offset) as usize;
            if inner_offset >= part.len() {
                break;
            }
            let remaining = target_len - out.len();
            let take = remaining.min(part.len() - inner_offset);
            out.extend_from_slice(&part[inner_offset..inner_offset + take]);
            cursor = cursor.saturating_add(take as u64);

            if part.len() < self.part_size as usize {
                break;
            }
        }

        if out.is_empty() && target_len > 0 {
            return Err(StreamingError::LoadFailed.into());
        }
        Ok(out)
    }

    fn prefetch(&self, offset: u64, len: usize) {
        if self.total_size == 0 || offset >= self.total_size || len == 0 {
            return;
        }
        if let Some((dl, _)) = self.ensure_streamed() {
            let clamped = (self.total_size - offset).min(len as u64) as usize;
            let _guard = self.handle.enter();
            let _ = dl.prefetch_range(offset, clamped);
        }
    }

    fn cancel(&self, offset: u64) {
        if let Ok(guard) = self.streamed.lock()
            && let Some(ref state) = *guard
        {
            let _ = state.downloader.cancel_part(offset);
        }
    }

    fn reset_priorities(&self) {
        if let Ok(guard) = self.streamed.lock()
            && let Some(ref state) = *guard
        {
            let _ = state.downloader.reset_priorities();
        }
    }

    fn stop(&self) {
        if let Some(abort) = self
            .bg_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            abort.abort();
        }
        if let Ok(guard) = self.streamed.lock()
            && let Some(ref state) = *guard
        {
            state.downloader.cancel();
        }
        if let Ok(mut guard) = self.parts.lock() {
            guard.clear();
        }
    }
}

/// [`tg_audio::AudioDataSource`] backed by `tg_data::Session` + `upload.getFile`
/// (with optional resumable [`StreamedFileDownloader`] range storage via [`Self::with_storage_dir`]).
pub struct LiveAudioDataSource<C: RpcClient + Clone + 'static> {
    rpc: C,
    handle: Handle,
    session: Arc<RwLock<Session>>,
    storage_dir: Option<PathBuf>,
}

impl<C: RpcClient + Clone + 'static> LiveAudioDataSource<C> {
    /// Creates a new live audio data source.
    #[must_use]
    pub fn new(rpc: C, handle: Handle, session: Arc<RwLock<Session>>) -> Self {
        Self {
            rpc,
            handle,
            session,
            storage_dir: None,
        }
    }

    /// Configures a persistent directory for resumable [`StreamedFileDownloader`] audio caching.
    #[must_use]
    pub fn with_storage_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.storage_dir = Some(dir.into());
        self
    }

    fn resolve_audio_location(&self, id: &AudioMsgId) -> AudioResult<ResolvedMediaLocation> {
        let session = Arc::clone(&self.session);
        let target = *id;
        block_on_runtime(&self.handle, async move {
            let guard = session.read().await;
            if let Some(msg) = guard.message(target.context_id)
                && let Some(MessageMedia::Document(doc)) = msg.media()
                && doc.id == target.document_id
            {
                return Some(document_to_location(doc));
            }
            // Fallback scan across loaded histories for matching document_id.
            for peer_id in known_peer_ids(&guard) {
                for msg in history_messages_ordered(&guard, peer_id) {
                    if let Some(MessageMedia::Document(doc)) = msg.media()
                        && doc.id == target.document_id
                    {
                        return Some(document_to_location(doc));
                    }
                }
            }
            None
        })
        .ok_or_else(|| {
            AudioError::SourceNotFound(format!("document {} not loaded", id.document_id.0))
        })
    }

    /// Opens a seekable [`TelegramRangeLoader`] (backed by [`StreamedFileDownloader`] +
    /// [`TelegramMediaInvoker`]) for audio track `id`.
    pub fn open_range_loader(&self, id: &AudioMsgId) -> AudioResult<TelegramRangeLoader<C>> {
        let resolved = self.resolve_audio_location(id)?;
        let origin = FileOrigin {
            peer_id: Some(id.context_id.peer.0),
            msg_id: Some(id.context_id.msg.0 as i32),
            sticker_set_id: None,
        };
        let mut loader = TelegramRangeLoader::new(self.rpc.clone(), self.handle.clone(), resolved)
            .with_refresh_origin(Arc::clone(&self.session), origin);
        if let Some(ref dir) = self.storage_dir {
            loader = loader.with_storage_dir(dir);
        }
        Ok(loader)
    }
}

impl<C: RpcClient + Clone + 'static> AudioDataSource for LiveAudioDataSource<C> {
    fn open(&self, id: &AudioMsgId) -> AudioResult<AudioInput> {
        let resolved = self.resolve_audio_location(id)?;
        let ext_hint = resolved.ext_hint.clone();

        if self.storage_dir.is_some() && resolved.size > 0 {
            let loader = self.open_range_loader(id)?;
            let path = loader
                .download_to_file()
                .map_err(|e| AudioError::SourceNotFound(e.to_string()))?;
            let bytes = fs::read(&path).map_err(|e| AudioError::SourceNotFound(e.to_string()))?;
            return Ok(AudioInput::from_bytes(bytes, ext_hint));
        }

        let origin = FileOrigin {
            peer_id: Some(id.context_id.peer.0),
            msg_id: Some(id.context_id.msg.0 as i32),
            sticker_set_id: None,
        };
        let rpc = self.rpc.clone();
        let session = Arc::clone(&self.session);
        let bytes = block_on_runtime(&self.handle, async move {
            download_file_bytes_with_refresh_async(
                &rpc,
                &session,
                origin,
                resolved.dc_id,
                resolved.location,
                (resolved.size > 0).then_some(resolved.size),
                |_, _| {},
            )
            .await
        })
        .map_err(|e| AudioError::SourceNotFound(e.to_string()))?;

        Ok(AudioInput::from_bytes(bytes, ext_hint))
    }
}

/// [`tg_audio::PlaylistProvider`] scanning loaded chat messages in `tg_data::Session` and
/// forwarding voice/round-video playback read receipts (`messages.readMessageContents` /
/// `channels.readMessageContents`) through `SessionRuntime`.
///
/// Port origin: `Telegram/SourceFiles/media/player/media_player_instance.cpp` (`Instance::play`)
/// and `Telegram/SourceFiles/apiwrap.cpp` (`ApiWrap::markContentsRead`).
pub struct LivePlaylistProvider {
    handle: Handle,
    session: Arc<RwLock<Session>>,
    runtime: Option<Arc<SessionRuntime<ErasedRpcClient>>>,
    bridge_state: Option<Arc<LiveBridgeState>>,
}

impl LivePlaylistProvider {
    /// Creates a new playlist provider backed by `session` (without RPC runtime wiring).
    #[must_use]
    pub fn new(handle: Handle, session: Arc<RwLock<Session>>) -> Self {
        Self {
            handle,
            session,
            runtime: None,
            bridge_state: None,
        }
    }

    /// Creates a live playlist provider backed by `runtime` so [`Self::mark_media_read`] sends
    /// `messages.readMessageContents` / `channels.readMessageContents` and feeds returned PTS
    /// updates through `SessionRuntime::apply_affected_history`.
    #[must_use]
    pub fn with_runtime(
        runtime: Arc<SessionRuntime<ErasedRpcClient>>,
        bridge_state: Option<Arc<LiveBridgeState>>,
    ) -> Self {
        let handle = runtime.tokio_handle().clone();
        let session = Arc::clone(runtime.session());
        Self {
            handle,
            session,
            runtime: Some(runtime),
            bridge_state,
        }
    }
}

impl PlaylistProvider for LivePlaylistProvider {
    fn playlist_for(&self, context: PlaylistContext, audio_type: AudioType) -> Vec<PlaylistEntry> {
        let session = Arc::clone(&self.session);
        block_on_runtime(&self.handle, async move {
            let guard = session.read().await;
            let mut entries = Vec::new();
            for msg in history_messages_ordered(&guard, context.peer_id) {
                let Some(MessageMedia::Document(doc)) = msg.media() else {
                    continue;
                };
                let matches_type = match audio_type {
                    AudioType::Voice => {
                        matches!(doc.kind, DocumentKind::Voice | DocumentKind::RoundVideo)
                    }
                    AudioType::Song => matches!(doc.kind, DocumentKind::Audio),
                    _ => false,
                };
                if !matches_type {
                    continue;
                }
                let media_kind = match doc.kind {
                    DocumentKind::Voice => MediaKind::VoiceMessage,
                    DocumentKind::RoundVideo => MediaKind::VideoMessage,
                    DocumentKind::Audio => MediaKind::AudioFile,
                    _ => MediaKind::Other,
                };
                let duration_ms = (doc.duration.unwrap_or(0.0) * 1000.0).max(0.0) as i64;
                let audio_id = AudioMsgId::new(
                    doc.id,
                    FullMsgId::new(context.peer_id, msg.id),
                    media_kind,
                    duration_ms,
                    0,
                );
                let mut entry = PlaylistEntry::new(audio_id);
                entry.unread = msg.flags.contains(MessageFlags::MEDIA_IS_UNREAD);
                entries.push(entry);
            }
            entries
        })
    }

    fn mark_media_read(&self, _document_id: DocumentId, context_id: FullMsgId) {
        // Port of `ApiWrap::markContentsRead(not_null<HistoryItem*>)` (`apiwrap.cpp:1510-1527`)
        // and `HistoryItem::markContentsRead(true)` (`history/history_item.cpp:1995-2013`).
        if !context_id.msg.is_server() {
            return;
        }
        let session = Arc::clone(&self.session);
        let runtime = self.runtime.as_ref().map(Arc::clone);
        let bridge_state = self.bridge_state.as_ref().map(Arc::clone);

        block_on_runtime(&self.handle, async move {
            let channel_input = {
                let mut guard = session.write().await;
                let is_self = guard.self_peer_id() == context_id.peer;
                let is_incoming_unread = guard.message(context_id).is_some_and(|msg| {
                    msg.flags.contains(MessageFlags::MEDIA_IS_UNREAD)
                        && (!msg.flags.contains(MessageFlags::OUTGOING) || is_self)
                });
                if !is_incoming_unread {
                    return;
                }
                let _ = guard.mark_messages_contents_read(Some(context_id.peer), &[context_id.msg]);
                context_id
                    .peer
                    .to_channel()
                    .map(|ch_id| input_channel_for(&guard, ch_id))
            };

            if let Some(runtime) = runtime {
                let rpc = runtime.rpc().clone();
                if let Some(channel) = channel_input {
                    if !matches!(channel, tl_enums::InputChannel::Empty) {
                        let req = tg_api::channels::build_read_channel_message_contents(
                            channel,
                            &[context_id.msg],
                        );
                        if let Err(err) =
                            with_rpc_timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await
                        {
                            tracing::warn!(
                                peer = context_id.peer.0,
                                msg_id = context_id.msg.0,
                                error = %err,
                                "channels.readMessageContents failed"
                            );
                        }
                    }
                } else {
                    let req =
                        tg_api::channels::build_read_common_message_contents(&[context_id.msg]);
                    match with_rpc_timeout(MEDIA_RPC_TIMEOUT, rpc.invoke(&req)).await {
                        Ok(tl_enums::messages::AffectedMessages::AffectedMessages(aff)) => {
                            if let Err(err) = runtime
                                .apply_affected_history(None, aff.pts, aff.pts_count)
                                .await
                            {
                                tracing::warn!(
                                    msg_id = context_id.msg.0,
                                    error = %err,
                                    "apply_affected_history after messages.readMessageContents failed"
                                );
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                msg_id = context_id.msg.0,
                                error = %err,
                                "messages.readMessageContents failed"
                            );
                        }
                    }
                }
            }

            if let Some(bridge) = bridge_state {
                bridge.notify_ui();
            }
        });
    }
}

/// Returns `true` if launching `path` externally requires explicit user confirmation
/// (`Data::LaunchWithWarning` in `data/data_document_resolver.cpp:91`).
///
/// Covers Windows executable/script types (`WINDOWS_EXECUTABLE_EXTS`), unknown extensions,
/// extensionless files (which can resolve sibling `.bat`/`.exe` on Windows), and IP-revealing
/// extensions (`.svg`, `.html`, `.m3u`, ...).
#[must_use]
pub fn path_requires_launch_confirmation(path: &std::path::Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    let ext = tg_media::document::file_extension(file_name);
    if ext.is_empty() {
        return true;
    }
    if tg_media::document::is_ip_revealing_filename(file_name) {
        return true;
    }
    matches!(
        tg_media::document::detect_name_type(file_name),
        tg_media::document::NameType::Executable | tg_media::document::NameType::Unknown
    )
}

fn default_shell_launcher(p: &std::path::Path) -> anyhow::Result<()> {
    if std::env::var_os("RUSTGRAM_NO_SHELL_OPEN").is_some()
        || std::env::var_os("TGFORK_NO_SHELL_OPEN").is_some()
    {
        return Ok(());
    }
    tg_platform::system_info::open_file(p).map_err(|err| anyhow!("{err}"))
}

/// Launches a downloaded benign local file in the OS default application via `ShellExecuteW`
/// (`tg_platform::system_info::open_file`), refusing executable, unknown, extensionless, or
/// IP-revealing files unless [`launch_external_file_confirmed`] is used, and failing closed if
/// marking the NTFS Internet Zone identifier (`:Zone.Identifier`) fails.
///
/// When `RUSTGRAM_NO_SHELL_OPEN=1` (or legacy `TGFORK_NO_SHELL_OPEN=1`) is set (e.g. headless CI/tests),
/// validates policy, file existence, and Zone.Identifier marking, then skips invoking the desktop shell.
pub fn launch_external_file(path: &std::path::Path) -> anyhow::Result<()> {
    launch_external_file_with(path, default_shell_launcher)
}

/// Launches a downloaded local file AFTER the user has explicitly confirmed the launch warning
/// dialog (`Data::LaunchWithWarning`), still enforcing file existence and NTFS `:Zone.Identifier`
/// marking before invoking the shell.
pub fn launch_external_file_confirmed(path: &std::path::Path) -> anyhow::Result<()> {
    launch_external_file_confirmed_with_zone_marker(
        path,
        tg_platform::system_info::mark_downloaded_zone_identifier,
        default_shell_launcher,
    )
}

/// Testable core of [`launch_external_file`] accepting a custom shell launcher closure.
pub fn launch_external_file_with<F>(path: &std::path::Path, launcher: F) -> anyhow::Result<()>
where
    F: FnOnce(&std::path::Path) -> anyhow::Result<()>,
{
    launch_external_file_with_zone_marker(
        path,
        tg_platform::system_info::mark_downloaded_zone_identifier,
        launcher,
    )
}

/// Testable core of [`launch_external_file`] accepting custom Zone.Identifier and launcher hooks.
pub fn launch_external_file_with_zone_marker<Z, F>(
    path: &std::path::Path,
    zone_marker: Z,
    launcher: F,
) -> anyhow::Result<()>
where
    Z: FnOnce(&std::path::Path) -> tg_platform::Result<()>,
    F: FnOnce(&std::path::Path) -> anyhow::Result<()>,
{
    if !path.is_file() {
        return Err(anyhow!(
            "cannot open external file: '{}' does not exist",
            path.display()
        ));
    }
    if path_requires_launch_confirmation(path) {
        return Err(anyhow!(
            "confirmation required before launching '{}'",
            path.display()
        ));
    }
    zone_marker(path).map_err(|err| {
        anyhow!(
            "failed to mark Zone.Identifier on '{}': {err}",
            path.display()
        )
    })?;
    launcher(path)
}

/// Testable core of [`launch_external_file_confirmed`] accepting custom Zone.Identifier and launcher hooks.
pub fn launch_external_file_confirmed_with_zone_marker<Z, F>(
    path: &std::path::Path,
    zone_marker: Z,
    launcher: F,
) -> anyhow::Result<()>
where
    Z: FnOnce(&std::path::Path) -> tg_platform::Result<()>,
    F: FnOnce(&std::path::Path) -> anyhow::Result<()>,
{
    if !path.is_file() {
        return Err(anyhow!(
            "cannot open external file: '{}' does not exist",
            path.display()
        ));
    }
    zone_marker(path).map_err(|err| {
        anyhow!(
            "failed to mark Zone.Identifier on '{}': {err}",
            path.display()
        )
    })?;
    launcher(path)
}

/// Starts a background `messages.reportMusicListen` for `track_id` on `handle` and returns `true`
/// when a report was queued (`false` for listens shorter than the 3 s minimum).
///
/// The `InputDocument` is resolved from `session`; an expired `FILE_REFERENCE_*` is refreshed once.
/// Like `Media::Player::MusicListenTracker::report`, the UI never waits for the reply.
pub fn spawn_report_music_listen<C>(
    rpc: &C,
    handle: &Handle,
    session: &Arc<RwLock<Session>>,
    track_id: AudioMsgId,
    duration_secs: i32,
) -> bool
where
    C: RpcClient + Clone + 'static,
{
    if duration_secs < tg_app_core::core_types::listen_tracker::MIN_REPORT_DURATION_SECS {
        return false;
    }
    let rpc = rpc.clone();
    let session = Arc::clone(session);
    handle.spawn(async move {
        let result = with_rpc_timeout(
            MEDIA_RPC_TIMEOUT,
            report_music_listen_async(&rpc, &session, track_id, duration_secs),
        )
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "messages.reportMusicListen failed");
        }
    });
    true
}

async fn report_music_listen_async<C>(
    rpc: &C,
    session: &Arc<RwLock<Session>>,
    track_id: AudioMsgId,
    duration_secs: i32,
) -> Result<bool, String>
where
    C: RpcClient + Clone + 'static,
{
    let (mut input_doc, origin, location) = {
        let guard = session.read().await;
        resolve_listen_document(&guard, track_id).ok_or_else(|| {
            format!(
                "audio document {} not found in session",
                track_id.document_id.0
            )
        })?
    };

    let req = tl_fns::messages::ReportMusicListen {
        id: input_doc.clone(),
        listened_duration: duration_secs,
    };
    match rpc.invoke(&req).await {
        Ok(ok) => Ok(ok),
        Err(err) => {
            let inv: tg_api::InvocationError = err.into();
            let err_text = inv.to_string();
            if err_text.contains("FILE_REFERENCE_")
                && let Ok(fresh_ref) =
                    refresh_file_reference_async(rpc, session, &origin, &location).await
            {
                if let tl_enums::InputDocument::InputDocument(ref mut inner) = input_doc {
                    inner.file_reference = fresh_ref;
                }
                return tg_app_core::core_types::listen_tracker::report_music_listen(
                    rpc,
                    input_doc,
                    duration_secs,
                )
                .await;
            }
            Err(format!("messages.reportMusicListen failed: {err_text}"))
        }
    }
}

/// Finds the audio document of `track_id` (its own message first, then any loaded history)
/// and returns its TL `InputDocument` with the origin and location used to refresh the
/// file reference.
fn resolve_listen_document(
    session: &Session,
    track_id: AudioMsgId,
) -> Option<(tl_enums::InputDocument, FileOrigin, TransferLocation)> {
    let build = |doc: &DocumentData, peer: i64, msg: i32| {
        let input = tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
            id: doc.id.0 as i64,
            access_hash: doc.access_hash as i64,
            file_reference: doc.file_reference.clone(),
        });
        let origin = FileOrigin {
            peer_id: Some(peer),
            msg_id: Some(msg),
            sticker_set_id: None,
        };
        let location = TransferLocation::Document {
            id: doc.id.0,
            access_hash: doc.access_hash,
            file_reference: doc.file_reference.clone(),
            thumb_size: String::new(),
        };
        (input, origin, location)
    };
    if let Some(msg) = session.message(track_id.context_id)
        && let Some(MessageMedia::Document(doc)) = msg.media()
        && doc.id == track_id.document_id
    {
        return Some(build(
            doc,
            track_id.context_id.peer.0,
            track_id.context_id.msg.0 as i32,
        ));
    }
    for pid in known_peer_ids(session) {
        for msg in history_messages_ordered(session, pid) {
            if let Some(MessageMedia::Document(doc)) = msg.media()
                && doc.id == track_id.document_id
            {
                return Some(build(doc, pid.0, msg.id.0 as i32));
            }
        }
    }
    None
}
