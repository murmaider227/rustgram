//! Live source of real userpics: downloads each peer's small profile photo with
//! `upload.getFile(inputPeerPhotoFileLocation)` into the media cache and decodes it once
//! (`PeerData::loadUserpic` / `Data::PeerUserpicView`, `data/data_peer.cpp`;
//! `Data::FileOrigin` peer photo, `storage/file_download.cpp`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tg_api::convert::input_peer_for;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::stand_in::types::PeerId;
use tg_data::{PeerId as DataPeerId, Session, UNKNOWN_PHOTO_ID};
use tg_session::SessionRuntime;
use tg_tl::{enums as tl_enums, types as tl_types};
use tg_ui::userpic_images::{UserpicImage, UserpicImageSource};
use tg_video::streaming::cache::CacheKey;

use crate::bridge::LiveBridgeState;
use crate::convert::to_data_peer;
use crate::media::ResolvedMediaLocation;
use crate::media_cache::{CacheFetchState, LiveMediaCache, MediaCacheTag};
use crate::session::LiveSession;

/// Side of the decoded userpic texture: the small (`a`, 160 px) profile photo.
const USERPIC_DECODE_SIDE: u32 = 160;

/// Cache key of `peer`'s small profile photo `photo_id`.
#[must_use]
pub fn userpic_cache_key(peer: DataPeerId, photo_id: u64) -> String {
    format!("userpic_{}_{photo_id}_a", peer.0)
}

/// Builds the `upload.getFile` location of `peer`'s small profile photo `photo_id`.
///
/// The request goes to the main DC; a photo stored elsewhere answers `FILE_MIGRATE_X`, which the
/// MTProto client follows on a download connection to that DC.
#[must_use]
pub fn userpic_location(
    session: &Session,
    peer: DataPeerId,
    photo_id: u64,
) -> ResolvedMediaLocation {
    ResolvedMediaLocation {
        dc_id: 0,
        location: tl_enums::InputFileLocation::InputPeerPhotoFileLocation(
            tl_types::InputPeerPhotoFileLocation {
                big: false,
                peer: input_peer_for(session, peer),
                photo_id: photo_id as i64,
            },
        ),
        size: 0,
        cache_key: CacheKey {
            high: photo_id,
            low: 2,
        },
        ext_hint: Some("jpg".to_owned()),
    }
}

/// Decodes downloaded userpic bytes into a square RGBA image; the painter masks it to a circle
/// at the size it is drawn.
#[must_use]
pub fn decode_userpic(photo_id: i64, bytes: &[u8]) -> Option<UserpicImage> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let side = USERPIC_DECODE_SIDE;
    let rgba = if decoded.width() == side && decoded.height() == side {
        decoded.to_rgba8()
    } else {
        decoded
            .resize_to_fill(side, side, image::imageops::FilterType::Triangle)
            .to_rgba8()
    };
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    Some(UserpicImage {
        photo_id,
        width: w,
        height: h,
        rgba: rgba.into_raw(),
    })
}

/// `(photo_id, image)`; a `None` image means the downloaded bytes could not be decoded.
type DecodedUserpic = (i64, Option<Arc<UserpicImage>>);

/// [`UserpicImageSource`] backed by a live session; holds only weak references so it never keeps
/// a logged-out session alive.
pub struct LiveUserpicSource {
    runtime: Weak<SessionRuntime<ErasedRpcClient>>,
    bridge: Weak<LiveBridgeState>,
    media_cache: LiveMediaCache,
    /// Decoded userpics by full peer id: `(photo_id, image)`; `None` image = undecodable bytes.
    decoded: Mutex<HashMap<i64, DecodedUserpic>>,
}

impl LiveUserpicSource {
    /// Creates a source for `live`.
    #[must_use]
    pub fn new(live: &LiveSession) -> Self {
        Self {
            runtime: Arc::downgrade(live.runtime()),
            bridge: Arc::downgrade(live.bridge_state()),
            media_cache: live.media_cache().clone(),
            decoded: Mutex::new(HashMap::new()),
        }
    }
}

impl UserpicImageSource for LiveUserpicSource {
    fn userpic(&self, peer_id: i64) -> Option<Arc<UserpicImage>> {
        let runtime = self.runtime.upgrade()?;
        let bridge = self.bridge.upgrade()?;
        let data_peer = to_data_peer(PeerId(peer_id));
        // Never block the UI thread: while the session is being written, try on a later frame.
        let (photo_id, location) = {
            let session = runtime.session().try_read().ok()?;
            let photo = session.peer(data_peer)?.common().userpic_photo_id;
            if photo == UNKNOWN_PHOTO_ID || photo.0 == 0 {
                return None;
            }
            (photo.0, userpic_location(&session, data_peer, photo.0))
        };
        if let Ok(map) = self.decoded.lock()
            && let Some((cached_photo, image)) = map.get(&peer_id)
            && *cached_photo == photo_id as i64
        {
            return image.clone();
        }
        let key = userpic_cache_key(data_peer, photo_id);
        let CacheFetchState::Ready(bytes) = self.media_cache.fetch_or_spawn(
            key,
            MediaCacheTag::Photo,
            location,
            None,
            runtime.rpc(),
            runtime.tokio_handle(),
            &bridge,
        ) else {
            return None;
        };
        let image = decode_userpic(photo_id as i64, &bytes).map(Arc::new);
        if let Ok(mut map) = self.decoded.lock() {
            map.insert(peer_id, (photo_id as i64, image.clone()));
        }
        image
    }
}

/// Installs `live`'s userpic source on `ctx` so every widget that paints a peer userpic shows
/// the real photo once it is loaded.
pub fn install_live_userpics(ctx: &egui::Context, live: &LiveSession) {
    tg_ui::userpic_images::install_userpic_source(ctx, Arc::new(LiveUserpicSource::new(live)));
}

/// Installs `live`'s userpic source on `ctx` unless it is already the installed one (cheap;
/// called every frame so an account switch rebinds it).
pub fn ensure_live_userpics(ctx: &egui::Context, live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let id = egui::Id::unique("tg_app_live_userpic_source_key");
    if ctx.data(|d| d.get_temp::<usize>(id)) != Some(key) {
        install_live_userpics(ctx, live);
        ctx.data_mut(|d| d.insert_temp(id, key));
    }
}

#[cfg(test)]
#[path = "tests_userpic_images.rs"]
#[cfg(test)]
mod tests;
