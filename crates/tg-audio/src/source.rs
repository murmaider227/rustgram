//! Abstraction traits for audio data loading, resume position storage, and playlists.
//!
//! Keeps `tg-audio` independent of network/MTProto and storage crates so
//! callers can inject disk files, in-memory buffers, or streaming downloaders.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{AudioError, AudioResult};
use crate::types::{AudioMsgId, AudioType};
use tg_data::{DocumentId, FullMsgId, PeerId};

/// Combined `Read + Seek + Send + Sync` stream trait for audio inputs.
pub trait ReadSeek: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> ReadSeek for T {}

/// Resolved audio input passed to the decoder.
pub struct AudioInput {
    /// Underlying seekable reader.
    pub reader: Box<dyn ReadSeek>,
    /// Optional total byte length if known.
    pub byte_len: Option<u64>,
    /// Optional filename or file extension hint (e.g. `"ogg"`, `"mp3"`, `"flac"`).
    pub hint_ext: Option<String>,
}

impl AudioInput {
    /// Creates an [`AudioInput`] backed by in-memory bytes.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>, hint_ext: Option<String>) -> Self {
        let len = bytes.len() as u64;
        Self {
            reader: Box::new(Cursor::new(bytes)),
            byte_len: Some(len),
            hint_ext,
        }
    }

    /// Creates an [`AudioInput`] backed by a shared byte slice.
    #[must_use]
    pub fn from_arc_bytes(bytes: Arc<[u8]>, hint_ext: Option<String>) -> Self {
        let len = bytes.len() as u64;
        Self {
            reader: Box::new(Cursor::new(bytes)),
            byte_len: Some(len),
            hint_ext,
        }
    }

    /// Opens an [`AudioInput`] from a local filesystem path.
    pub fn from_path(path: impl AsRef<Path>) -> AudioResult<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let byte_len = file.metadata().ok().map(|m| m.len());
        let hint_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        Ok(Self {
            reader: Box::new(file),
            byte_len,
            hint_ext,
        })
    }
}

/// Trait providing audio data for a given [`AudioMsgId`] or [`DocumentId`].
///
/// Implement this trait to bridge `tg-audio` with local file caches or
/// MTProto streaming downloaders.
pub trait AudioDataSource: Send + Sync {
    /// Opens an [`AudioInput`] for the requested audio track.
    fn open(&self, id: &AudioMsgId) -> AudioResult<AudioInput>;
}

type MemoryAudioEntry = (Arc<[u8]>, Option<String>);

/// In-memory [`AudioDataSource`] mapping [`DocumentId`] to byte buffers.
#[derive(Default)]
pub struct MemoryAudioSource {
    entries: Mutex<HashMap<DocumentId, MemoryAudioEntry>>,
}

impl MemoryAudioSource {
    /// Creates an empty in-memory audio source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces an audio buffer for `document_id`.
    pub fn insert(&self, document_id: DocumentId, bytes: Vec<u8>, hint_ext: Option<String>) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(document_id, (Arc::from(bytes), hint_ext));
        }
    }

    /// Removes the entry for `document_id`.
    pub fn remove(&self, document_id: DocumentId) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.remove(&document_id);
        }
    }
}

impl AudioDataSource for MemoryAudioSource {
    fn open(&self, id: &AudioMsgId) -> AudioResult<AudioInput> {
        let guard = self
            .entries
            .lock()
            .map_err(|_| AudioError::InvalidOperation("audio source lock poisoned".into()))?;
        let (bytes, ext) = guard
            .get(&id.document_id)
            .ok_or_else(|| AudioError::SourceNotFound(format!("document {}", id.document_id.0)))?;
        Ok(AudioInput::from_arc_bytes(Arc::clone(bytes), ext.clone()))
    }
}

/// Filesystem-backed [`AudioDataSource`] mapping [`DocumentId`] to local paths.
#[derive(Default)]
pub struct FileAudioSource {
    paths: Mutex<HashMap<DocumentId, PathBuf>>,
}

impl FileAudioSource {
    /// Creates an empty file-backed audio source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Associates `document_id` with a local file `path`.
    pub fn register(&self, document_id: DocumentId, path: PathBuf) {
        if let Ok(mut guard) = self.paths.lock() {
            guard.insert(document_id, path);
        }
    }
}

impl AudioDataSource for FileAudioSource {
    fn open(&self, id: &AudioMsgId) -> AudioResult<AudioInput> {
        let path = {
            let guard = self
                .paths
                .lock()
                .map_err(|_| AudioError::InvalidOperation("file source lock poisoned".into()))?;
            guard.get(&id.document_id).cloned().ok_or_else(|| {
                AudioError::SourceNotFound(format!("document {}", id.document_id.0))
            })?
        };
        AudioInput::from_path(path)
    }
}

/// Persistent store for per-document resume playback positions (in milliseconds).
///
/// Matches `session.local().mediaLastPlaybackPosition` / `setMediaLastPlaybackPosition`
/// in `media_player_instance.cpp:127-145, 985-987`.
pub trait ResumePositionStore: Send + Sync {
    /// Returns the saved resume position in milliseconds (0 if none).
    fn get_position_ms(&self, document_id: DocumentId) -> i64;

    /// Saves or clears (when 0) the resume position in milliseconds.
    fn set_position_ms(&self, document_id: DocumentId, position_ms: i64);
}

/// Thread-safe in-memory [`ResumePositionStore`].
#[derive(Default)]
pub struct InMemoryPositionStore {
    positions: Mutex<HashMap<DocumentId, i64>>,
}

impl InMemoryPositionStore {
    /// Creates an empty in-memory resume position store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ResumePositionStore for InMemoryPositionStore {
    fn get_position_ms(&self, document_id: DocumentId) -> i64 {
        self.positions
            .lock()
            .ok()
            .and_then(|g| g.get(&document_id).copied())
            .unwrap_or(0)
    }

    fn set_position_ms(&self, document_id: DocumentId, position_ms: i64) {
        if let Ok(mut guard) = self.positions.lock() {
            if position_ms <= 0 {
                guard.remove(&document_id);
            } else {
                guard.insert(document_id, position_ms);
            }
        }
    }
}

/// Entry in a chat's audio/voice playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistEntry {
    /// Primary audio item for this message.
    pub id: AudioMsgId,
    /// Additional tracks embedded within the same message (e.g. rich page audio).
    pub sub_tracks: Vec<AudioMsgId>,
    /// Whether this entry has a self-destruct TTL (skipped during auto-advance,
    /// matching `media->ttlSeconds()` check in `media_player_instance.cpp:622`).
    pub has_ttl: bool,
    /// Whether this voice/round message is still unread.
    pub unread: bool,
}

impl PlaylistEntry {
    /// Creates a standard single-track playlist entry.
    #[must_use]
    pub fn new(id: AudioMsgId) -> Self {
        Self {
            id,
            sub_tracks: Vec::new(),
            has_ttl: false,
            unread: false,
        }
    }

    /// Marks this entry as unread.
    #[must_use]
    pub const fn with_unread(mut self, unread: bool) -> Self {
        self.unread = unread;
        self
    }

    /// Marks this entry as having a self-destruct TTL.
    #[must_use]
    pub const fn with_ttl(mut self, has_ttl: bool) -> Self {
        self.has_ttl = has_ttl;
        self
    }
}

/// Context identifying which chat/topic/playlist an audio item belongs to.
///
/// Matches `Media::Player::PlaylistContext` in `media_player_instance.h:58`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PlaylistContext {
    /// Peer (dialog) ID.
    pub peer_id: PeerId,
    /// Forum topic root message ID (0 if not in a forum topic).
    pub topic_root_id: i32,
    /// Monoforum sublist peer ID (0 if none).
    pub monoforum_peer_id: PeerId,
}

/// Trait providing playlist items and read-status updates to the player.
pub trait PlaylistProvider: Send + Sync {
    /// Returns the ordered playlist slice for a `(context, audio_type)` pair,
    /// ordered from oldest (index 0) to newest (index `len - 1`), matching
    /// tdesktop's `SparseIdsMergedSlice` ordering.
    fn playlist_for(&self, context: PlaylistContext, audio_type: AudioType) -> Vec<PlaylistEntry>;

    /// Marks a voice or round video message as listened/read.
    fn mark_media_read(&self, document_id: DocumentId, context_id: FullMsgId);
}

/// Simple in-memory [`PlaylistProvider`] for standalone usage and tests.
#[derive(Default)]
pub struct InMemoryPlaylistProvider {
    playlists: Mutex<HashMap<(PlaylistContext, AudioType), Vec<PlaylistEntry>>>,
    read_docs: Mutex<Vec<(DocumentId, FullMsgId)>>,
}

impl InMemoryPlaylistProvider {
    /// Creates an empty playlist provider.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the playlist entries for a given context and audio type.
    pub fn set_playlist(
        &self,
        context: PlaylistContext,
        audio_type: AudioType,
        entries: Vec<PlaylistEntry>,
    ) {
        if let Ok(mut guard) = self.playlists.lock() {
            guard.insert((context, audio_type), entries);
        }
    }

    /// Returns all `(DocumentId, FullMsgId)` pairs that were marked as read.
    #[must_use]
    pub fn marked_read(&self) -> Vec<(DocumentId, FullMsgId)> {
        self.read_docs.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl PlaylistProvider for InMemoryPlaylistProvider {
    fn playlist_for(&self, context: PlaylistContext, audio_type: AudioType) -> Vec<PlaylistEntry> {
        self.playlists
            .lock()
            .ok()
            .and_then(|g| g.get(&(context, audio_type)).cloned())
            .unwrap_or_default()
    }

    fn mark_media_read(&self, document_id: DocumentId, context_id: FullMsgId) {
        if let Ok(mut guard) = self.read_docs.lock() {
            guard.push((document_id, context_id));
        }
        if let Ok(mut playlists) = self.playlists.lock() {
            for entries in playlists.values_mut() {
                for entry in entries.iter_mut() {
                    if entry.id.document_id == document_id && entry.id.context_id == context_id {
                        entry.unread = false;
                    }
                }
            }
        }
    }
}
