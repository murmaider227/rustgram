//! Full pictures of the demo chat photos (`HistoryBackend::inline_media_descriptor`).
//!
//! The demo messages carry only a blurred stripped thumbnail; their full pictures are the
//! rendered images of [`MockMediaBackend`] (the ones the media viewer opens). A chat photo or
//! album tile mapped to one of them is reported as already downloaded, so the history widget
//! decodes and shows it like a photo the live session found in its media cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tg_app_core::core_types::dto::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use tg_app_media::media::backend::{MediaBackend, MediaItemId};
use tg_app_media::media::mock::MockMediaBackend;

/// Encoded bytes of the demo pictures, read once per item and shared by every descriptor (the
/// widget keeps decoded pixels while the same `Arc` comes back).
#[derive(Default)]
pub(super) struct DemoPhotoBytes {
    bytes: Mutex<HashMap<MediaItemId, Option<Arc<[u8]>>>>,
}

impl DemoPhotoBytes {
    /// A downloaded-photo descriptor for the demo picture `media_id`, or `None` when the demo
    /// media backend has no bytes for it.
    pub(super) fn descriptor(
        &self,
        media_id: MediaItemId,
        base: &MockMediaBackend,
    ) -> Option<InlineMediaDescriptor> {
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(media_id)
            .or_insert_with(|| base.load_bytes(media_id).ok().map(Arc::from))
            .clone()?;
        Some(InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Ready { bytes }),
            ..InlineMediaDescriptor::default()
        })
    }
}
