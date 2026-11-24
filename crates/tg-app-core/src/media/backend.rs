//! `MediaBackend` trait and domain descriptors for the media viewer, PiP, audio player,
//! round-video float, and inline playback subsystems.
//!
//! Port origin: data/document/photo access seams in
//! `Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp` and
//! `Telegram/SourceFiles/media/player/media_player_widget.cpp`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tg_audio::{AudioDataSource, AudioMsgId, PlaylistProvider};
use tg_text::TextWithEntities;
use tg_video::RangeLoader;

/// Unique identifier for a viewable media item (photo, video, GIF, round video, or document).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MediaItemId(pub u64);

/// Media payload classification for viewer & inline playback routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaItemKind {
    /// Still photo (PNG/JPEG/WebP).
    Photo,
    /// Regular video file with audio and playback controls.
    Video,
    /// Silent looping animation (GIF / H.264 GIFv).
    Gif,
    /// Circular round video message (`isVideoMessage`).
    RoundVideo,
    /// Live RTMP/HLS video stream (`MessageMedia::VideoStream` / live story).
    VideoStream,
    /// Telegram Desktop theme / palette file (`.tdesktop-theme` / `.tdesktop-palette`).
    Theme,
    /// Generic non-previewable document attachment.
    Document,
}

impl MediaItemKind {
    /// Returns `true` for items decoded through `tg-video` streaming/clip playback.
    #[must_use]
    pub const fn is_video_like(self) -> bool {
        matches!(self, Self::Video | Self::Gif | Self::RoundVideo)
    }

    /// Returns `true` when video playback controls bar should be shown in the overlay.
    #[must_use]
    pub const fn shows_playback_controls(self) -> bool {
        matches!(self, Self::Video)
    }

    /// Returns `true` when playback loops automatically (GIFs).
    #[must_use]
    pub const fn loops_by_default(self) -> bool {
        matches!(self, Self::Gif)
    }
}

/// Metadata for a single media item opened in the viewer or played inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaItem {
    /// Unique media identifier.
    pub id: MediaItemId,
    /// Owning chat/peer identifier (matches `PeerId` encoding).
    pub peer_id: i64,
    /// Owning message identifier inside `peer_id`.
    pub msg_id: i32,
    /// Media classification.
    pub kind: MediaItemKind,
    /// Natural media width in pixels.
    pub width: u32,
    /// Natural media height in pixels.
    pub height: u32,
    /// Duration in milliseconds (`0` for still photos).
    pub duration_ms: i64,
    /// Encoded file size in bytes.
    pub file_size: u64,
    /// Suggested filename for saving/display (e.g. `"alpine_sunset.png"`).
    pub file_name: String,
    /// MIME type (e.g. `"image/png"`, `"video/mp4"`).
    pub mime_type: String,
    /// Sender display name shown in the viewer header.
    pub sender_name: String,
    /// Formatted send date/time string shown in the viewer header.
    pub date_text: String,
    /// Formatted caption with rich text entities (`tg-text`).
    pub caption: TextWithEntities,
    /// Initial clockwise rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation_degrees: i32,
    /// Whether this item has an alpha channel (e.g. transparent PNG).
    pub has_alpha: bool,
    /// Whether this item allows forwarding (`!forbidsForward`).
    pub allows_forward: bool,
    /// Whether the current user can delete this item (`canDelete`).
    pub allows_delete: bool,
    /// Whether saving/copying is permitted (`!forbidsSaving`).
    pub allows_save: bool,
    /// Optional local file path if already cached on disk.
    pub local_path: Option<PathBuf>,
}

/// Display metadata for a track managed by [`tg_audio::PlayerInstance`] in the top audio player bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackMetadata {
    /// Audio message / document ID matching `PlayerInstance`.
    pub id: AudioMsgId,
    /// Primary title (song title or voice sender name).
    pub title: String,
    /// Secondary subtitle (performer/artist or voice timestamp).
    pub subtitle: String,
    /// Duration in milliseconds.
    pub duration_ms: i64,
}

/// Abstraction seam decoupling all `tg-app::media` UI components from network/storage crates.
///
/// Implemented by [`crate::media::mock::MockMediaBackend`] for standalone previews and tests,
/// and later wired to `tg-api` + `tg-storage` during integration.
pub trait MediaBackend: Send + Sync {
    /// Looks up a single media item by ID.
    fn get_item(&self, id: MediaItemId) -> Option<MediaItem>;

    /// Returns the ordered sibling gallery items in `peer_id`'s shared media containing `item_id`
    /// (oldest to newest, matching tdesktop's `SharedMediaWithLastSlice` order).
    fn siblings_in_chat(&self, peer_id: i64, item_id: MediaItemId) -> Vec<MediaItem>;

    /// Loads the complete raw file bytes for `item_id` (photos, small GIFs, save-as).
    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>>;

    /// Opens a [`RangeLoader`] for progressive video streaming via `tg-video::StreamingPlayer`.
    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>>;

    /// Saves `id` to `destination` on disk and returns the final written path.
    fn save_to_disk(&self, id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf>;

    /// Hook triggered when the user chooses "Copy" / "Copy Frame" on `id`.
    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()>;

    /// Hook triggered when the user chooses "Forward" on `id`.
    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()>;

    /// Hook triggered when the user chooses a specific destination peer for forwarding `id`.
    fn forward_item_to(&self, id: MediaItemId, _to_peer: i64) -> anyhow::Result<()> {
        self.forward_item(id)
    }

    /// Hook triggered when the user confirms "Delete" on `id`.
    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()>;

    /// Hook triggered when the user chooses "Show in Chat" on `id`.
    fn show_in_chat(&self, peer_id: i64, msg_id: i32);

    /// Hook triggered when the user chooses "Open in External App" on `id`.
    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf>;

    /// Looks up display metadata for an audio track in the top audio player bar.
    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata>;

    /// Optional encoded cover art bytes (PNG or JPEG) for OS media controls (Windows SMTC).
    fn audio_track_cover(&self, _id: AudioMsgId) -> Option<Vec<u8>> {
        None
    }

    /// Reports accumulated music listen duration (`>= 3s`) to Telegram via
    /// `messages.reportMusicListen` (`Media::Player::MusicListenTracker`). Returns `Ok(true)` when
    /// a report was sent or queued; live sessions send it in the background.
    fn report_music_listen(
        &self,
        _track_id: AudioMsgId,
        _duration_secs: i32,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Returns the [`AudioDataSource`] used by [`tg_audio::PlayerInstance`].
    fn audio_data_source(&self) -> Arc<dyn AudioDataSource>;

    /// Returns the [`PlaylistProvider`] used by [`tg_audio::PlayerInstance`].
    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider>;
}
