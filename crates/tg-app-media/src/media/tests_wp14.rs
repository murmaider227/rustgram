//! Headless WP-14 unit tests for SMTC wiring on [`AudioPlayerBar`], screenshot protection
//! policy on [`MediaOverlayWidget`], and OCR selection/copy integration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tg_audio::{AudioDataSource, AudioMsgId, OrderMode, PlaylistProvider, RepeatMode};
use tg_platform::ocr::{OcrLine, OcrRect, OcrResult, OcrWord};
use tg_platform::smtc::{SmtcCommand, SmtcPlaybackStatus};
use tg_video::RangeLoader;

use crate::media::backend::{AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId};
use crate::media::mock::{MOCK_CHAT_PEER_ID, MOCK_PHOTO_LANDSCAPE_ID, MockMediaBackend};
use crate::media::overlay::MediaOverlayWidget;
use crate::media::player_bar::AudioPlayerBar;

struct CoverInjectingBackend {
    inner: MockMediaBackend,
    cover_bytes: Vec<u8>,
    force_protected: bool,
}

impl MediaBackend for CoverInjectingBackend {
    fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
        self.inner.get_item(id).map(|mut it| {
            if self.force_protected {
                it.allows_save = false;
                it.allows_forward = false;
            }
            it
        })
    }

    fn siblings_in_chat(&self, peer_id: i64, item_id: MediaItemId) -> Vec<MediaItem> {
        self.inner
            .siblings_in_chat(peer_id, item_id)
            .into_iter()
            .map(|mut it| {
                if self.force_protected {
                    it.allows_save = false;
                    it.allows_forward = false;
                }
                it
            })
            .collect()
    }

    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        self.inner.load_bytes(id)
    }

    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        self.inner.open_range_loader(id)
    }

    fn save_to_disk(&self, id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        self.inner.save_to_disk(id, destination)
    }

    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.inner.copy_item(id)
    }

    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.inner.forward_item(id)
    }

    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.inner.delete_item(id)
    }

    fn show_in_chat(&self, peer_id: i64, msg_id: i32) {
        self.inner.show_in_chat(peer_id, msg_id);
    }

    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        self.inner.open_in_external_app(id)
    }

    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        self.inner.audio_data_source()
    }

    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        self.inner.playlist_provider()
    }

    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.inner.audio_track_metadata(id)
    }

    fn audio_track_cover(&self, _id: AudioMsgId) -> Option<Vec<u8>> {
        Some(self.cover_bytes.clone())
    }
}

#[test]
fn audio_player_bar_smtc_snapshot_and_commands() {
    let backend = CoverInjectingBackend {
        inner: MockMediaBackend::new(),
        cover_bytes: vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        force_protected: false,
    };
    let song_id = *backend
        .inner
        .sample_song_ids()
        .first()
        .expect("mock sample song");

    let mut bar = AudioPlayerBar::new(&backend, false);

    // Before playback, SMTC is disabled with no metadata.
    let (controls, meta) = bar.build_smtc_snapshot(&backend);
    assert!(!controls.enabled);
    assert_eq!(controls.status, SmtcPlaybackStatus::Stopped);
    assert!(meta.is_none());

    // Start playing a song -> SMTC becomes enabled with Playing status, title, and cover bytes.
    bar.play_item(song_id, None).expect("play mock song");
    let (controls, meta) = bar.build_smtc_snapshot(&backend);
    assert!(controls.enabled);
    assert!(controls.play_enabled);
    assert!(controls.pause_enabled);
    assert_eq!(controls.status, SmtcPlaybackStatus::Playing);
    let meta = meta.expect("active track metadata");
    assert!(!meta.title.is_empty());
    assert_eq!(
        meta.thumbnail_bytes.as_deref(),
        Some(backend.cover_bytes.as_slice())
    );

    // Pause command transitions SMTC status to Paused.
    assert!(bar.apply_smtc_command(SmtcCommand::Pause));
    let (controls, _) = bar.build_smtc_snapshot(&backend);
    assert_eq!(controls.status, SmtcPlaybackStatus::Paused);

    // Play command resumes Playing.
    assert!(bar.apply_smtc_command(SmtcCommand::Play));
    let (controls, _) = bar.build_smtc_snapshot(&backend);
    assert_eq!(controls.status, SmtcPlaybackStatus::Playing);

    // Loop & Shuffle commands update PlayerInstance repeat/order modes.
    assert!(bar.apply_smtc_command(SmtcCommand::LoopTrack));
    assert_eq!(bar.player().repeat_mode(), RepeatMode::One);
    assert!(bar.apply_smtc_command(SmtcCommand::LoopPlaylist));
    assert_eq!(bar.player().repeat_mode(), RepeatMode::All);
    assert!(bar.apply_smtc_command(SmtcCommand::LoopNone));
    assert_eq!(bar.player().repeat_mode(), RepeatMode::None);

    assert!(bar.apply_smtc_command(SmtcCommand::Shuffle));
    assert_eq!(bar.player().order_mode(), OrderMode::Shuffle);
    assert!(bar.apply_smtc_command(SmtcCommand::Shuffle));
    assert_eq!(bar.player().order_mode(), OrderMode::Default);

    // Raise command sets the one-shot raise flag.
    assert!(!bar.take_raise_request());
    assert!(bar.apply_smtc_command(SmtcCommand::Raise));
    assert!(bar.take_raise_request());
    assert!(!bar.take_raise_request());

    // Stop disables SMTC.
    assert!(bar.apply_smtc_command(SmtcCommand::Stop));
    let (controls, meta) = bar.build_smtc_snapshot(&backend);
    assert!(!controls.enabled);
    assert!(meta.is_none());
}

#[test]
fn media_overlay_screenshot_protection_and_ocr_selection() {
    let normal_backend = CoverInjectingBackend {
        inner: MockMediaBackend::new(),
        cover_bytes: Vec::new(),
        force_protected: false,
    };
    let protected_backend = CoverInjectingBackend {
        inner: MockMediaBackend::new(),
        cover_bytes: Vec::new(),
        force_protected: true,
    };

    let mut overlay = MediaOverlayWidget::new();
    assert!(!overlay.content_needs_screenshot_protection());

    // Normal photo does not require screenshot protection.
    overlay
        .open_item(
            &normal_backend,
            MOCK_CHAT_PEER_ID,
            MOCK_PHOTO_LANDSCAPE_ID,
            0,
        )
        .expect("open normal photo");
    assert!(!overlay.content_needs_screenshot_protection());

    // Inject OCR result and verify selection + copyable text.
    let ocr = OcrResult {
        language_tag: "en-US".to_owned(),
        text: "HELLO WORLD".to_owned(),
        text_angle: None,
        coordinate_width: 400,
        coordinate_height: 200,
        lines: vec![OcrLine {
            text: "HELLO WORLD".to_owned(),
            rect: OcrRect {
                x: 20.0,
                y: 20.0,
                width: 200.0,
                height: 30.0,
            },
            words: vec![
                OcrWord {
                    text: "HELLO".to_owned(),
                    rect: OcrRect {
                        x: 20.0,
                        y: 20.0,
                        width: 90.0,
                        height: 30.0,
                    },
                },
                OcrWord {
                    text: "WORLD".to_owned(),
                    rect: OcrRect {
                        x: 120.0,
                        y: 20.0,
                        width: 100.0,
                        height: 30.0,
                    },
                },
            ],
        }],
    };
    overlay.set_ocr_result_for_test(ocr);
    assert!(overlay.ocr_state().active);
    assert_eq!(
        overlay.ocr_state().copyable_text().as_deref(),
        Some("HELLO WORLD")
    );
    let result_clone = overlay
        .ocr_state()
        .result
        .clone()
        .expect("ocr result present");
    overlay.ocr_state_mut().selection.select_all(&result_clone);
    assert_eq!(
        overlay.ocr_state().selection.selected_text(&result_clone),
        "HELLO WORLD"
    );

    // Protected photo (`allows_save == false`) requires screenshot protection and refuses OCR.
    overlay
        .open_item(
            &protected_backend,
            MOCK_CHAT_PEER_ID,
            MOCK_PHOTO_LANDSCAPE_ID,
            100,
        )
        .expect("open protected photo");
    assert!(overlay.content_needs_screenshot_protection());
    assert!(overlay.recognize_current_photo(100).is_none());

    overlay.close();
    assert!(!overlay.content_needs_screenshot_protection());
}
