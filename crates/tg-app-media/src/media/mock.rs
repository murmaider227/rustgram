//! In-memory mock implementation of [`MediaBackend`] with runtime-generated sample media.
//!
//! Generates crisp procedural gradient photos and WAV audio tracks in pure Rust at runtime,
//! and provides a test/example helper that uses `ffmpeg` (from `FFMPEG_DIR/bin` or `PATH`) to
//! synthesize real MP4 video files for streaming & round-video verification.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use image::{ImageFormat, Rgba, RgbaImage};
use tg_audio::{
    AudioDataSource, AudioMsgId, AudioType, DocumentId, FullMsgId, InMemoryPlaylistProvider,
    MediaKind, MemoryAudioSource, MsgId, PeerId, PlaylistContext, PlaylistEntry, PlaylistProvider,
    encode_wav_pcm_s16le,
};
use tg_text::{EntityInText, EntityType, TextWithEntities, utf8_to_utf16_offset};
use tg_video::{LocalFileLoader, MemoryRangeLoader, RangeLoader};

use crate::media::backend::{
    AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId, MediaItemKind,
};

/// Default chat peer ID used by [`MockMediaBackend`] sample items.
pub const MOCK_CHAT_PEER_ID: i64 = 100_420;

/// Sample photo 1 ID (scenic alpine sunset landscape, 1280×800).
pub const MOCK_PHOTO_LANDSCAPE_ID: MediaItemId = MediaItemId(101);
/// Sample photo 2 ID (geometric studio poster with transparency, 840×1050).
pub const MOCK_PHOTO_PORTRAIT_ID: MediaItemId = MediaItemId(102);
/// Sample photo 3 ID (aurora night sky panorama, 1280×720).
pub const MOCK_PHOTO_AURORA_ID: MediaItemId = MediaItemId(103);
/// Sample video ID (populated via [`MockMediaBackend::register_video_file`] or `with_generated_videos`).
pub const MOCK_VIDEO_ID: MediaItemId = MediaItemId(201);
/// Sample GIF animation ID.
pub const MOCK_GIF_ID: MediaItemId = MediaItemId(202);
/// Sample round video message ID.
pub const MOCK_ROUND_VIDEO_ID: MediaItemId = MediaItemId(203);

/// Recorded side-effect action on [`MockMediaBackend`] for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockActionLog {
    /// Item was saved to disk at `path`.
    Saved(MediaItemId, PathBuf),
    /// Item was copied to clipboard.
    Copied(MediaItemId),
    /// Item forward was requested.
    Forwarded(MediaItemId),
    /// Item delete was requested.
    Deleted(MediaItemId),
    /// Jump to message in chat was requested.
    ShowInChat {
        /// Target chat peer ID.
        peer_id: i64,
        /// Target message ID.
        msg_id: i32,
    },
    /// Open in external application was requested.
    OpenedExternal(MediaItemId, PathBuf),
}

enum StoredMediaData {
    Memory(Vec<u8>),
    File(PathBuf),
}

struct MockState {
    items: Vec<MediaItem>,
    data: HashMap<MediaItemId, StoredMediaData>,
    audio_tracks: HashMap<AudioMsgId, AudioTrackMetadata>,
    actions: Vec<MockActionLog>,
}

/// Self-contained [`MediaBackend`] backed by procedurally generated local media.
pub struct MockMediaBackend {
    state: Mutex<MockState>,
    audio_source: Arc<MemoryAudioSource>,
    playlists: Arc<InMemoryPlaylistProvider>,
    sample_song_ids: Vec<AudioMsgId>,
    sample_voice_ids: Vec<AudioMsgId>,
}

impl Default for MockMediaBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Descriptor for registering a local video/GIF/round-video file on [`MockMediaBackend`].
#[derive(Debug, Clone)]
pub struct MockVideoFile {
    /// Target media item ID.
    pub id: MediaItemId,
    /// Video, GIF, or RoundVideo kind.
    pub kind: MediaItemKind,
    /// Local path to the generated media file.
    pub path: PathBuf,
    /// Video width in pixels.
    pub width: u32,
    /// Video height in pixels.
    pub height: u32,
    /// Duration in milliseconds.
    pub duration_ms: i64,
    /// Sender display name.
    pub sender_name: String,
    /// Rich caption with entities.
    pub caption: TextWithEntities,
}

impl MockMediaBackend {
    /// Creates a new [`MockMediaBackend`] with runtime-generated sample photos and audio tracks.
    ///
    /// Does not invoke external executables; call [`Self::register_video_file`] or
    /// [`generate_test_mp4`] from tests/examples to attach real MP4 videos.
    #[must_use]
    pub fn new() -> Self {
        let photo1_png = encode_png(&render_alpine_sunset(1280, 800));
        let photo2_png = encode_png(&render_studio_poster(840, 1050));
        let photo3_png = encode_png(&render_aurora_panorama(1280, 720));

        let caption1 = build_caption_with_matches(
            "Alpine Ridge sunset captured at 2,400m — tap #landscape or visit https://rustgram.example/gallery for RAW notes.",
            &[
                ("Alpine Ridge sunset", EntityType::Bold),
                ("#landscape", EntityType::Hashtag),
                ("https://rustgram.example/gallery", EntityType::Url),
                ("RAW notes", EntityType::Italic),
            ],
        );
        let caption2 = build_caption_with_matches(
            "Studio Grid Study (transparent PNG) — check shader_pass.wgsl and hidden spoiler note.",
            &[
                ("Studio Grid Study", EntityType::Bold),
                ("shader_pass.wgsl", EntityType::Code),
                ("hidden spoiler note", EntityType::Spoiler),
            ],
        );
        let caption3 = build_caption_with_matches(
            "Northern horizon luminescence over pine valley #nightsky",
            &[("#nightsky", EntityType::Hashtag)],
        );

        let items = vec![
            MediaItem {
                id: MOCK_PHOTO_LANDSCAPE_ID,
                peer_id: MOCK_CHAT_PEER_ID,
                msg_id: 1001,
                kind: MediaItemKind::Photo,
                width: 1280,
                height: 800,
                duration_ms: 0,
                file_size: photo1_png.len() as u64,
                file_name: "alpine_ridge_sunset.png".to_owned(),
                mime_type: "image/png".to_owned(),
                sender_name: "Alpine Trail Club".to_owned(),
                date_text: tg_lang::tr::lng_mediaview_today("18:42"),
                caption: caption1,
                rotation_degrees: 0,
                has_alpha: false,
                allows_forward: true,
                allows_delete: true,
                allows_save: true,
                local_path: None,
                grouped_id: None,
                attached_stickers_media: Some(
                    tg_app_core::chat_helpers::backend::AttachedStickersMedia::Photo {
                        id: 880_001,
                        access_hash: 0x5A5A_1001,
                        file_reference: vec![0xA1, 0xB2, 0xC3, 0xD4],
                    },
                ),
            },
            MediaItem {
                id: MOCK_PHOTO_PORTRAIT_ID,
                peer_id: MOCK_CHAT_PEER_ID,
                msg_id: 1002,
                kind: MediaItemKind::Photo,
                width: 840,
                height: 1050,
                duration_ms: 0,
                file_size: photo2_png.len() as u64,
                file_name: "studio_grid_poster.png".to_owned(),
                mime_type: "image/png".to_owned(),
                sender_name: "Design Lab".to_owned(),
                date_text: tg_lang::tr::lng_mediaview_today("19:05"),
                caption: caption2,
                rotation_degrees: 0,
                has_alpha: true,
                allows_forward: true,
                allows_delete: true,
                allows_save: true,
                local_path: None,
                grouped_id: None,
                attached_stickers_media: None,
            },
            MediaItem {
                id: MOCK_PHOTO_AURORA_ID,
                peer_id: MOCK_CHAT_PEER_ID,
                msg_id: 1003,
                kind: MediaItemKind::Photo,
                width: 1280,
                height: 720,
                duration_ms: 0,
                file_size: photo3_png.len() as u64,
                file_name: "northern_aurora.png".to_owned(),
                mime_type: "image/png".to_owned(),
                sender_name: "Observatory Desk".to_owned(),
                date_text: tg_lang::tr::lng_mediaview_today("21:14"),
                caption: caption3,
                rotation_degrees: 0,
                has_alpha: false,
                allows_forward: true,
                allows_delete: true,
                allows_save: true,
                local_path: None,
                grouped_id: None,
                attached_stickers_media: None,
            },
        ];

        let mut data = HashMap::new();
        data.insert(MOCK_PHOTO_LANDSCAPE_ID, StoredMediaData::Memory(photo1_png));
        data.insert(MOCK_PHOTO_PORTRAIT_ID, StoredMediaData::Memory(photo2_png));
        data.insert(MOCK_PHOTO_AURORA_ID, StoredMediaData::Memory(photo3_png));

        let audio_source = Arc::new(MemoryAudioSource::new());
        let playlists = Arc::new(InMemoryPlaylistProvider::new());
        let mut audio_tracks = HashMap::new();

        let peer = PeerId(MOCK_CHAT_PEER_ID);
        let ctx = PlaylistContext {
            peer_id: peer,
            ..PlaylistContext::default()
        };

        // Synthesize 2 music tracks and 2 voice notes as pure-Rust WAV buffers.
        let song1 = AudioMsgId::new(
            DocumentId(9001),
            FullMsgId::new(peer, MsgId(2001)),
            MediaKind::AudioFile,
            4_000,
            0,
        );
        let song2 = AudioMsgId::new(
            DocumentId(9002),
            FullMsgId::new(peer, MsgId(2002)),
            MediaKind::AudioFile,
            4_000,
            0,
        );
        let voice1 = AudioMsgId::new(
            DocumentId(9101),
            FullMsgId::new(peer, MsgId(2101)),
            MediaKind::VoiceMessage,
            3_000,
            0,
        );
        let voice2 = AudioMsgId::new(
            DocumentId(9102),
            FullMsgId::new(peer, MsgId(2102)),
            MediaKind::VoiceMessage,
            3_000,
            0,
        );

        audio_source.insert(
            song1.document_id,
            synthesize_chord_wav(440.0, 4.0),
            Some("wav".to_owned()),
        );
        audio_source.insert(
            song2.document_id,
            synthesize_chord_wav(523.25, 4.0),
            Some("wav".to_owned()),
        );
        audio_source.insert(
            voice1.document_id,
            synthesize_chord_wav(330.0, 3.0),
            Some("wav".to_owned()),
        );
        audio_source.insert(
            voice2.document_id,
            synthesize_chord_wav(392.0, 3.0),
            Some("wav".to_owned()),
        );

        playlists.set_playlist(
            ctx,
            AudioType::Song,
            vec![PlaylistEntry::new(song1), PlaylistEntry::new(song2)],
        );
        playlists.set_playlist(
            ctx,
            AudioType::Voice,
            vec![
                PlaylistEntry::new(voice1).with_unread(true),
                PlaylistEntry::new(voice2).with_unread(true),
            ],
        );

        audio_tracks.insert(
            song1,
            AudioTrackMetadata {
                id: song1,
                title: "Midnight Telemetry".to_owned(),
                subtitle: "Synth Lab".to_owned(),
                duration_ms: 4_000,
            },
        );
        audio_tracks.insert(
            song2,
            AudioTrackMetadata {
                id: song2,
                title: "Solar Drift".to_owned(),
                subtitle: "Orbit Notes".to_owned(),
                duration_ms: 4_000,
            },
        );
        audio_tracks.insert(
            voice1,
            AudioTrackMetadata {
                id: voice1,
                title: "Alpine Trail Club".to_owned(),
                subtitle: format!(
                    "{} · {}",
                    tg_lang::tr::lng_in_dlg_audio(),
                    tg_lang::tr::lng_player_message_today("19:20")
                ),
                duration_ms: 3_000,
            },
        );
        audio_tracks.insert(
            voice2,
            AudioTrackMetadata {
                id: voice2,
                title: "Design Lab".to_owned(),
                subtitle: format!(
                    "{} · {}",
                    tg_lang::tr::lng_in_dlg_audio(),
                    tg_lang::tr::lng_player_message_today("19:22")
                ),
                duration_ms: 3_000,
            },
        );

        Self {
            state: Mutex::new(MockState {
                items,
                data,
                audio_tracks,
                actions: Vec::new(),
            }),
            audio_source,
            playlists,
            sample_song_ids: vec![song1, song2],
            sample_voice_ids: vec![voice1, voice2],
        }
    }

    /// Registers a video/GIF/round-video file on disk into the mock gallery.
    pub fn register_video_file(&self, video: MockVideoFile) -> anyhow::Result<()> {
        let file_size = std::fs::metadata(&video.path)
            .with_context(|| format!("stat video file {}", video.path.display()))?
            .len();
        let file_name = video
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sample.mp4")
            .to_owned();
        let id = video.id;
        let path = video.path;
        let item = MediaItem {
            id,
            peer_id: MOCK_CHAT_PEER_ID,
            msg_id: 1000 + id.0 as i32,
            kind: video.kind,
            width: video.width,
            height: video.height,
            duration_ms: video.duration_ms,
            file_size,
            file_name,
            mime_type: "video/mp4".to_owned(),
            sender_name: video.sender_name,
            date_text: tg_lang::tr::lng_mediaview_today("20:15"),
            caption: video.caption,
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: true,
            allows_delete: true,
            allows_save: true,
            local_path: Some(path.clone()),
            grouped_id: None,
            attached_stickers_media: None,
        };
        if let Ok(mut guard) = self.state.lock() {
            if let Some(existing) = guard.items.iter_mut().find(|i| i.id == id) {
                *existing = item;
            } else {
                guard.items.push(item);
            }
            guard.data.insert(id, StoredMediaData::File(path));
        }
        Ok(())
    }

    /// Returns the IDs of the sample music tracks.
    #[must_use]
    pub fn sample_song_ids(&self) -> &[AudioMsgId] {
        &self.sample_song_ids
    }

    /// Returns the IDs of the sample voice messages.
    #[must_use]
    pub fn sample_voice_ids(&self) -> &[AudioMsgId] {
        &self.sample_voice_ids
    }

    /// Returns a snapshot of recorded mock actions (save, copy, forward, delete, show-in-chat).
    #[must_use]
    pub fn action_log(&self) -> Vec<MockActionLog> {
        self.state
            .lock()
            .map(|g| g.actions.clone())
            .unwrap_or_default()
    }

    /// Inserts or replaces an in-memory media item for tests.
    pub fn insert_item(&self, item: MediaItem, bytes: Vec<u8>) {
        if let Ok(mut guard) = self.state.lock() {
            let id = item.id;
            if let Some(existing) = guard.items.iter_mut().find(|i| i.id == id) {
                *existing = item;
            } else {
                guard.items.push(item);
            }
            guard.data.insert(id, StoredMediaData::Memory(bytes));
        }
    }

    /// Sets or clears the attached-stickers media reference on a mock item (used in tests).
    pub fn set_item_attached_stickers(
        &self,
        id: MediaItemId,
        media: Option<tg_app_core::chat_helpers::backend::AttachedStickersMedia>,
    ) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(item) = guard.items.iter_mut().find(|i| i.id == id)
        {
            item.attached_stickers_media = media;
        }
    }
}

impl MediaBackend for MockMediaBackend {
    fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
        self.state
            .lock()
            .ok()?
            .items
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    fn siblings_in_chat(&self, peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
        self.state
            .lock()
            .map(|g| {
                g.items
                    .iter()
                    .filter(|item| item.peer_id == peer_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        let guard = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("mock media state lock poisoned"))?;
        match guard.data.get(&id) {
            Some(StoredMediaData::Memory(bytes)) => Ok(bytes.clone()),
            Some(StoredMediaData::File(path)) => {
                std::fs::read(path).with_context(|| format!("read media file {}", path.display()))
            }
            None => bail!("mock media item {} not found", id.0),
        }
    }

    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        let guard = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("mock media state lock poisoned"))?;
        match guard.data.get(&id) {
            Some(StoredMediaData::File(path)) => {
                let loader = LocalFileLoader::open(path)
                    .with_context(|| format!("open range loader for {}", path.display()))?;
                Ok(Arc::new(loader))
            }
            Some(StoredMediaData::Memory(bytes)) => {
                let loader = MemoryRangeLoader::new(bytes.clone())
                    .map_err(|e| anyhow::anyhow!("memory range loader failed: {e}"))?;
                Ok(Arc::new(loader))
            }
            None => bail!("mock media item {} not found", id.0),
        }
    }

    fn save_to_disk(&self, id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        let bytes = self.load_bytes(id)?;
        let out = tg_platform::file_safety::write_unique_file_at_destination(destination, &bytes)
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        if let Ok(mut guard) = self.state.lock() {
            guard.actions.push(MockActionLog::Saved(id, out.clone()));
        }
        Ok(out)
    }

    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Ok(mut guard) = self.state.lock() {
            guard.actions.push(MockActionLog::Copied(id));
        }
        Ok(())
    }

    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Ok(mut guard) = self.state.lock() {
            guard.actions.push(MockActionLog::Forwarded(id));
        }
        Ok(())
    }

    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Ok(mut guard) = self.state.lock() {
            guard.items.retain(|i| i.id != id);
            guard.data.remove(&id);
            guard.actions.push(MockActionLog::Deleted(id));
        }
        Ok(())
    }

    fn show_in_chat(&self, peer_id: i64, msg_id: i32) {
        if let Ok(mut guard) = self.state.lock() {
            guard
                .actions
                .push(MockActionLog::ShowInChat { peer_id, msg_id });
        }
    }

    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        let path = self
            .get_item(id)
            .and_then(|item| item.local_path)
            .unwrap_or_else(|| PathBuf::from(format!("mock_media_{}.bin", id.0)));
        if let Ok(mut guard) = self.state.lock() {
            guard
                .actions
                .push(MockActionLog::OpenedExternal(id, path.clone()));
        }
        Ok(path)
    }

    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.state.lock().ok()?.audio_tracks.get(&id).cloned()
    }

    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        Arc::clone(&self.audio_source) as Arc<dyn AudioDataSource>
    }

    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        Arc::clone(&self.playlists) as Arc<dyn PlaylistProvider>
    }
}

/// Default executable name for FFmpeg when resolved via `PATH`.
pub const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";

/// Locates the `ffmpeg` executable via `FFMPEG_EXE`, `FFMPEG_DIR/bin`, or `PATH`.
#[must_use]
pub fn find_ffmpeg_bin() -> PathBuf {
    if let Some(exe) = std::env::var_os("FFMPEG_EXE").filter(|v| !v.is_empty()) {
        let path = PathBuf::from(exe);
        if path.is_file() {
            return path;
        }
    }
    if let Some(dir) = std::env::var_os("FFMPEG_DIR").filter(|v| !v.is_empty()) {
        let bin_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        let candidate = PathBuf::from(dir).join("bin").join(bin_name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(DEFAULT_FFMPEG_BIN)
}

/// Generates a real H.264 + AAC MP4 test video using FFmpeg (for tests and screenshot examples only).
///
/// Uses `-movflags +faststart` so the `moov` atom sits at the start for progressive streaming.
pub fn generate_test_mp4(
    output_path: &Path,
    width: u32,
    height: u32,
    duration_secs: u32,
    with_audio: bool,
) -> anyhow::Result<()> {
    let ffmpeg = find_ffmpeg_bin();
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut cmd = std::process::Command::new(&ffmpeg);
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size={width}x{height}:rate=24:duration={duration_secs}"),
    ]);
    if with_audio {
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={duration_secs}"),
            "-c:a",
            "aac",
            "-b:a",
            "96k",
        ]);
    } else {
        cmd.arg("-an");
    }
    cmd.args([
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-preset",
        "ultrafast",
        "-movflags",
        "+faststart",
    ])
    .arg(output_path);

    let status = cmd
        .status()
        .with_context(|| format!("failed to launch ffmpeg at {}", ffmpeg.display()))?;
    if !status.success() {
        bail!("ffmpeg exited with status {status}");
    }
    Ok(())
}

// ── Procedural Sample Image & Audio Generators ───────────────────────────────

/// Renders a high-contrast scenic alpine sunset gradient image with sun glow and mountain layers.
#[must_use]
pub fn render_alpine_sunset(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    let wf = width.max(1) as f32;
    let hf = height.max(1) as f32;
    let sun_x = wf * 0.62;
    let sun_y = hf * 0.43;
    let sun_r = hf * 0.11;

    for y in 0..height {
        let v = y as f32 / hf;
        for x in 0..width {
            let u = x as f32 / wf;
            // Sky twilight gradient: deep indigo -> magenta dusk -> warm amber horizon.
            let (mut r, mut g, mut b) = if v < 0.55 {
                let t = v / 0.55;
                (
                    lerp(20.0, 178.0, t * t),
                    lerp(32.0, 84.0, t),
                    lerp(74.0, 134.0, t),
                )
            } else {
                let t = ((v - 0.55) / 0.45).min(1.0);
                (
                    lerp(178.0, 246.0, 1.0 - t * 0.4),
                    lerp(84.0, 162.0, 1.0 - t * 0.6),
                    lerp(134.0, 96.0, t),
                )
            };

            // Warm sun disk and radial glow.
            let dx = x as f32 - sun_x;
            let dy = y as f32 - sun_y;
            let dist = (dx * dx + dy * dy).sqrt();
            let glow = (1.0 - (dist / (sun_r * 3.8)).min(1.0)).powi(2);
            r = (r + glow * 115.0).min(255.0);
            g = (g + glow * 92.0).min(255.0);
            b = (b + glow * 45.0).min(255.0);
            if dist < sun_r {
                let core = (1.0 - dist / sun_r).sqrt();
                r = lerp(255.0, 255.0, core);
                g = lerp(218.0, 250.0, core);
                b = lerp(142.0, 226.0, core);
            }

            // Distant ridge silhouette.
            let ridge1 = 0.56
                + 0.08 * (u * 5.2 + 0.4).sin()
                + 0.04 * (u * 13.7).cos()
                + 0.02 * (u * 27.0).sin();
            if v > ridge1 {
                let depth = ((v - ridge1) / 0.35).min(1.0);
                r = lerp(66.0, 38.0, depth);
                g = lerp(52.0, 44.0, depth);
                b = lerp(92.0, 78.0, depth);
            }

            // Midground alpine peaks.
            let ridge2 = 0.66
                + 0.07 * (u * 7.8 + 2.1).sin()
                + 0.045 * (u * 19.3).cos()
                + 0.015 * (u * 41.0).sin();
            if v > ridge2 {
                let depth = ((v - ridge2) / 0.3).min(1.0);
                r = lerp(34.0, 18.0, depth);
                g = lerp(42.0, 28.0, depth);
                b = lerp(66.0, 48.0, depth);
            }

            // Foreground lake reflection band at bottom 16%.
            if v > 0.84 {
                let lake_t = (v - 0.84) / 0.16;
                let ripple = ((x as f32 * 0.07 + y as f32 * 0.65).sin() * 0.5 + 0.5) * 18.0;
                let shimmer = (1.0 - ((u - 0.62).abs() / 0.16).min(1.0)).powi(2);
                r = (lerp(28.0, 18.0, lake_t) + shimmer * 165.0 + ripple * shimmer).min(255.0);
                g = (lerp(40.0, 26.0, lake_t) + shimmer * 110.0 + ripple * 0.4).min(255.0);
                b = (lerp(66.0, 44.0, lake_t) + shimmer * 55.0).min(255.0);
            }

            img.put_pixel(x, y, Rgba([r as u8, g as u8, b as u8, 255]));
        }
    }
    img
}

/// Renders a portrait geometric studio graphic with transparent margins (exercises checkerboard bg).
#[must_use]
pub fn render_studio_poster(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    let wf = width.max(1) as f32;
    let hf = height.max(1) as f32;
    let cx = wf * 0.5;
    let cy = hf * 0.46;
    let radius = wf.min(hf) * 0.36;

    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / wf;
            let v = y as f32 / hf;
            let margin = 0.07;
            if u < margin || u > 1.0 - margin || v < margin || v > 1.0 - margin {
                img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                continue;
            }
            let mut r = lerp(24.0, 64.0, u);
            let mut g = lerp(110.0, 185.0, v);
            let mut b = lerp(195.0, 238.0, 1.0 - u * 0.4);

            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d < radius {
                let t = d / radius;
                r = lerp(255.0, 232.0, t);
                g = lerp(206.0, 92.0, t);
                b = lerp(92.0, 126.0, t);
            } else if (x % 48 == 0) || (y % 48 == 0) {
                r = (r + 36.0).min(255.0);
                g = (g + 36.0).min(255.0);
                b = (b + 36.0).min(255.0);
            }
            img.put_pixel(x, y, Rgba([r as u8, g as u8, b as u8, 255]));
        }
    }
    img
}

/// Renders a nocturnal aurora borealis gradient panorama.
#[must_use]
pub fn render_aurora_panorama(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    let wf = width.max(1) as f32;
    let hf = height.max(1) as f32;

    for y in 0..height {
        let v = y as f32 / hf;
        for x in 0..width {
            let u = x as f32 / wf;
            let wave = 0.36 + 0.14 * (u * 6.5).sin() + 0.06 * (u * 14.0 + 1.2).cos();
            let band = (-(v - wave).powi(2) / 0.022).exp();
            let r = (10.0 + band * 52.0 + (1.0 - v) * 14.0).min(255.0);
            let g = (18.0 + band * 212.0 + v * 12.0).min(255.0);
            let b = (36.0 + band * 148.0 + (1.0 - v) * 32.0).min(255.0);
            img.put_pixel(x, y, Rgba([r as u8, g as u8, b as u8, 255]));
        }
    }
    img
}

fn encode_png(image: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = image.write_to(&mut Cursor::new(&mut out), ImageFormat::Png);
    out
}

fn synthesize_chord_wav(base_hz: f32, duration_secs: f32) -> Vec<u8> {
    let sample_rate = 48_000_u32;
    let frames = (sample_rate as f32 * duration_secs) as usize;
    let mut pcm = Vec::with_capacity(frames);
    let freqs = [base_hz, base_hz * 1.25, base_hz * 1.5];
    for i in 0..frames {
        let t = i as f32 / sample_rate as f32;
        let env = (t * 8.0).min(1.0) * ((duration_secs - t) * 8.0).clamp(0.0, 1.0);
        let sum: f32 = freqs
            .iter()
            .map(|&f| (2.0 * std::f32::consts::PI * f * t).sin())
            .sum();
        let sample = ((sum / freqs.len() as f32) * 0.35 * env * i16::MAX as f32) as i16;
        pcm.push(sample);
    }
    encode_wav_pcm_s16le(&pcm, sample_rate, 1)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Builds a [`TextWithEntities`] by locating each substring with exact UTF-16 offsets.
#[must_use]
pub fn build_caption_with_matches(text: &str, spans: &[(&str, EntityType)]) -> TextWithEntities {
    let mut entities = Vec::new();
    for &(needle, kind) in spans {
        if let Some(byte_start) = text.find(needle)
            && let Some(utf16_start) = utf8_to_utf16_offset(text, byte_start)
        {
            let utf16_len = needle.encode_utf16().count() as i32;
            entities.push(EntityInText::new(kind, utf16_start as i32, utf16_len));
        }
    }
    TextWithEntities::with_entities(text, entities)
}
