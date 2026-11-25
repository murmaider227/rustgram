//! UI parity stage 2, package F: the media viewer navigates only its shared-media list and hides
//! its over-controls like Telegram Desktop 7.4.6 (`OverlayWidget::sharedMediaType`,
//! `activateControls` / `hideControls`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tg_audio::{AudioDataSource, AudioMsgId, PlaylistProvider};
use tg_text::TextWithEntities;
use tg_video::RangeLoader;

use super::{ControlsFade, MediaOverlayWidget, filter_gallery_siblings};
use crate::media::backend::{
    AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId, MediaItemKind,
};
use crate::media::mock::MockMediaBackend;
use crate::media::style::{MEDIAVIEW_HIDE_DURATION_MS, MEDIAVIEW_WAIT_HIDE_MS};

const PEER: i64 = 1001;

fn item(id: u64, kind: MediaItemKind, name: &str) -> MediaItem {
    MediaItem {
        id: MediaItemId(id),
        peer_id: PEER,
        msg_id: id as i32,
        kind,
        width: 800,
        height: 520,
        duration_ms: 0,
        file_size: 88,
        file_name: name.to_owned(),
        mime_type: String::new(),
        sender_name: "Testname Testsurname".to_owned(),
        date_text: String::new(),
        caption: TextWithEntities::default(),
        rotation_degrees: 0,
        has_alpha: false,
        allows_forward: true,
        allows_delete: true,
        allows_save: true,
        local_path: None,
        grouped_id: None,
        attached_stickers_media: None,
    }
}

/// Saved Messages with one photo and one `.txt` file, as on the parity test account.
fn saved_messages_items() -> Vec<MediaItem> {
    vec![
        item(1, MediaItemKind::Photo, "photo_1.jpg"),
        item(2, MediaItemKind::Document, "sample_notes.txt"),
    ]
}

#[test]
fn photo_gallery_skips_documents() {
    let photos = filter_gallery_siblings(saved_messages_items(), MediaItemId(1));
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].kind, MediaItemKind::Photo);

    let files = filter_gallery_siblings(saved_messages_items(), MediaItemId(2));
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].kind, MediaItemKind::Document);

    let mixed = vec![
        item(1, MediaItemKind::Photo, "a.jpg"),
        item(3, MediaItemKind::Video, "b.mp4"),
        item(4, MediaItemKind::Gif, "c.mp4"),
    ];
    let kinds: Vec<_> = filter_gallery_siblings(mixed, MediaItemId(3))
        .into_iter()
        .map(|i| i.kind)
        .collect();
    assert_eq!(kinds, vec![MediaItemKind::Photo, MediaItemKind::Video]);
}

struct SavedMessagesBackend {
    items: Vec<MediaItem>,
    inner: MockMediaBackend,
}

impl MediaBackend for SavedMessagesBackend {
    fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
        self.items.iter().find(|i| i.id == id).cloned()
    }
    fn siblings_in_chat(&self, _peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
        self.items.clone()
    }
    fn load_bytes(&self, _id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("no bytes in this fixture")
    }
    fn open_range_loader(&self, _id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        anyhow::bail!("no video in this fixture")
    }
    fn save_to_disk(&self, _id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        Ok(destination.to_path_buf())
    }
    fn copy_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }
    fn forward_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }
    fn delete_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }
    fn show_in_chat(&self, _peer_id: i64, _msg_id: i32) {}
    fn open_in_external_app(&self, _id: MediaItemId) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from("x"))
    }
    fn audio_track_metadata(&self, _id: AudioMsgId) -> Option<AudioTrackMetadata> {
        None
    }
    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        self.inner.audio_data_source()
    }
    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        self.inner.playlist_provider()
    }
}

#[test]
fn opening_a_photo_next_to_a_file_shows_no_counter_or_filmstrip() {
    let backend = SavedMessagesBackend {
        items: saved_messages_items(),
        inner: MockMediaBackend::new(),
    };
    let mut overlay = MediaOverlayWidget::new();
    let _ = overlay.open_item(&backend, PEER, MediaItemId(1), 1_000);
    assert!(overlay.open);
    assert_eq!(
        overlay.gallery.len(),
        1,
        "the .txt is not part of the photo list"
    );
    assert_eq!(overlay.gallery.counter_text(), None, "no 'Photo 1 of 2'");
    assert!(!overlay.gallery.can_next());
}

#[test]
fn controls_hide_after_wait_and_return_on_activity() {
    let mut fade = ControlsFade::default();
    fade.activate(1_000);
    assert!((fade.step(1_000, false) - 1.0).abs() < f32::EPSILON);
    let wait = MEDIAVIEW_WAIT_HIDE_MS as i64;
    let hide = MEDIAVIEW_HIDE_DURATION_MS as i64;
    assert!((fade.step(1_000 + wait, false) - 1.0).abs() < f32::EPSILON);
    let mid = fade.step(1_000 + wait + hide / 2, false);
    assert!(mid > 0.0 && mid < 1.0, "fading out: {mid}");
    assert!(fade.step(1_000 + wait + hide + 1, false) <= f32::EPSILON);
    assert!(!fade.is_animating(1_000 + wait + hide + 1));

    // Mouse movement shows them again (`mediaviewShowDuration` fade-in).
    let back = 10_000;
    fade.activate(back);
    let _ = fade.step(back, false);
    let shown = fade.step(back + 400, false);
    assert!((shown - 1.0).abs() < f32::EPSILON);

    // An open menu or a pressed mouse keeps them visible.
    let held = fade.step(back + wait + hide + 5_000, true);
    assert!((held - 1.0).abs() < f32::EPSILON);
}

#[test]
fn overlay_opens_with_controls_shown() {
    let backend = SavedMessagesBackend {
        items: saved_messages_items(),
        inner: MockMediaBackend::new(),
    };
    let mut overlay = MediaOverlayWidget::new();
    let _ = overlay.open_item(&backend, PEER, MediaItemId(1), 5_000);
    assert!((overlay.controls_opacity() - 1.0).abs() < f32::EPSILON);
}
