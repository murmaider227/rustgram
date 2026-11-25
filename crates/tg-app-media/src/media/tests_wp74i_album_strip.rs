//! WP-74 follow-up: the media viewer's thumbnail strip shows only the items of the current
//! message's album (`GroupThumbs::RefreshFromSlice`, `ComputeContext` = `groupId`), never every
//! photo of the chat.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use tg_audio::{AudioDataSource, AudioMsgId, PlaylistProvider};
use tg_text::TextWithEntities;
use tg_video::RangeLoader;

use super::MediaOverlayWidget;
use crate::media::backend::{
    AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId, MediaItemKind,
};
use crate::media::group_thumbs::album_strip_range;
use crate::media::mock::MockMediaBackend;

const PEER: i64 = 2002;
const ALBUM: i64 = 0x00A1_B000;

fn photo(id: u64, grouped_id: Option<i64>) -> MediaItem {
    MediaItem {
        id: MediaItemId(id),
        peer_id: PEER,
        msg_id: id as i32,
        kind: MediaItemKind::Photo,
        width: 800,
        height: 520,
        duration_ms: 0,
        file_size: 88,
        file_name: format!("photo_{id}.jpg"),
        mime_type: "image/jpeg".to_owned(),
        sender_name: "Testname Testsurname".to_owned(),
        date_text: String::new(),
        caption: TextWithEntities::default(),
        rotation_degrees: 0,
        has_alpha: false,
        allows_forward: true,
        allows_delete: true,
        allows_save: true,
        local_path: None,
        attached_stickers_media: None,
        grouped_id,
    }
}

/// A chat with a two-photo album (ids 1, 2) followed by two single photos (ids 3, 4).
fn chat_items() -> Vec<MediaItem> {
    vec![
        photo(1, Some(ALBUM)),
        photo(2, Some(ALBUM)),
        photo(3, None),
        photo(4, None),
    ]
}

struct ChatBackend {
    items: Vec<MediaItem>,
    inner: MockMediaBackend,
}

impl MediaBackend for ChatBackend {
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
fn album_strip_range_covers_only_the_current_album() {
    let items = chat_items();
    assert_eq!(album_strip_range(&items, 0), Some(0..2));
    assert_eq!(album_strip_range(&items, 1), Some(0..2));
    assert_eq!(album_strip_range(&items, 2), None, "single photo: no strip");
    assert_eq!(album_strip_range(&items, 3), None);
    assert_eq!(album_strip_range(&items, 9), None);

    // A one-item "album" and two different albums side by side.
    let lone = vec![photo(1, Some(7)), photo(2, Some(8)), photo(3, Some(8))];
    assert_eq!(album_strip_range(&lone, 0), None);
    assert_eq!(album_strip_range(&lone, 2), Some(1..3));
}

/// Opens `item` in the viewer, renders a few frames and returns how many thumbnails the strip
/// painted.
fn strip_slots_after_opening(item: u64) -> (usize, usize) {
    let backend = ChatBackend {
        items: chat_items(),
        inner: MockMediaBackend::new(),
    };
    let mut overlay = MediaOverlayWidget::new();
    overlay
        .open_item(&backend, PEER, MediaItemId(item), 1_000)
        .expect("open item");
    let gallery_len = overlay.gallery().len();
    let mut harness = egui_kittest::Harness::builder()
        .with_size(Vec2::new(1280.0, 800.0))
        .build_ui_state(
            |ui, overlay: &mut MediaOverlayWidget| {
                let ctx = ui.ctx().clone();
                let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0));
                let _ = overlay.show(&ctx, rect, &backend, 1_000);
            },
            overlay,
        );
    for _ in 0..4 {
        harness.step();
    }
    (gallery_len, harness.state().group_thumbs.slots().len())
}

#[test]
fn media_viewer_strip_shows_album_items_only() {
    let (gallery_len, album_slots) = strip_slots_after_opening(1);
    assert_eq!(gallery_len, 4, "arrow navigation still walks every photo");
    assert_eq!(
        album_slots, 2,
        "album photo: strip with its two album items"
    );

    let (gallery_len, single_slots) = strip_slots_after_opening(3);
    assert_eq!(gallery_len, 4);
    assert_eq!(
        single_slots, 0,
        "a photo outside any album shows no strip even in a multi-photo gallery"
    );
}
