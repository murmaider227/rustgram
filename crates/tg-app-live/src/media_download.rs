//! Downloads the user starts or cancels with a click on a message's media.
//!
//! Port origin: `PhotoSaveClickHandler` / `PhotoCancelClickHandler`
//! (`data/data_photo.cpp`, wired as `Photo::_savel` / `_cancell` in
//! `history/view/media/history_view_photo.cpp`) and the first play of a voice message, which
//! tdesktop never downloads automatically (`Data::AutoDownload::Should`).
//!
//! Internal design differs from tdesktop: the cancelled mark lives in the media cache by file
//! key (`LiveMediaCache::cancel_download`) instead of a `CancelledFileLoader` object on the
//! `PhotoData`, so every message that shows the same photo sees one state and a lookup is a
//! set probe.

use std::sync::Arc;

use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_data::FullMsgId;
use tg_media::transfer::FileOrigin;
use tg_video::RangeLoader;

use crate::convert::{to_data_msg, to_data_peer};
use crate::media::{TelegramRangeLoader, message_media_to_location};
use crate::media_cache::{LiveMediaCache, MAX_INLINE_AUTO_DOWNLOAD_BYTES};
use crate::session::LiveSession;
use crate::stream_fill::FillingLoader;

fn message(live: &LiveSession, peer_id: PeerId, msg_id: MsgId) -> Option<tg_data::Message> {
    let id = FullMsgId::new(to_data_peer(peer_id), to_data_msg(msg_id));
    live.read_session(|s| s.message(id).cloned())
}

/// Starts, or restarts after a cancel, the download of the photo or voice file of `msg_id`.
pub fn start(live: &LiveSession, peer_id: PeerId, msg_id: MsgId) {
    let Some(msg) = message(live, peer_id, msg_id) else {
        return;
    };
    live.media_cache().start_media_download_for_message(
        &msg,
        Some(live.runtime().session()),
        live.runtime().rpc(),
        live.runtime().tokio_handle(),
        live.bridge_state(),
    );
}

/// A part-by-part loader of `msg_id`'s media file, for playing a voice message while it
/// downloads (TGB-3000). Parts go to the media cache's `streamed` directory like the media
/// viewer's, so a second play reads them from disk.
pub fn stream(live: &LiveSession, peer_id: PeerId, msg_id: MsgId) -> Option<Arc<dyn RangeLoader>> {
    let msg = message(live, peer_id, msg_id)?;
    let resolved = message_media_to_location(msg.media()?)?;
    if resolved.size == 0 {
        return None;
    }
    let resolved_size = resolved.size;
    let origin = FileOrigin {
        peer_id: Some(to_data_peer(peer_id).0),
        msg_id: Some(msg_id.0),
        sticker_set_id: None,
    };
    let runtime = live.runtime();
    let mut loader = TelegramRangeLoader::new(
        runtime.rpc().clone(),
        runtime.tokio_handle().clone(),
        resolved,
    )
    .with_refresh_origin(Arc::clone(runtime.session()), origin);
    let cache = live.media_cache();
    if cache.is_persistent() {
        loader = loader.with_storage_dir(cache.root_dir().join("streamed"));
    }
    let size = resolved_size;
    let loader: Arc<dyn RangeLoader> = Arc::new(loader);
    // TGB-3115: once the stream has read the whole file it is kept in the media cache, so the
    // message counts as loaded (no size in its status line) and the next play reads the file.
    let Some((key, tag, _)) = LiveMediaCache::download_target(msg.media()?) else {
        return Some(loader);
    };
    if size > MAX_INLINE_AUTO_DOWNLOAD_BYTES {
        return Some(loader);
    }
    let cache = cache.clone();
    let bridge = Arc::clone(live.bridge_state());
    Some(Arc::new(FillingLoader::new(
        loader,
        move |bytes| match cache.put_blob(&key, tag, &bytes) {
            Ok(()) => bridge.notify_ui(),
            Err(err) => tracing::warn!("streamed file not cached: {err}"),
        },
    )))
}

/// Cancels the running download of the photo or voice file of `msg_id`.
pub fn cancel(live: &LiveSession, peer_id: PeerId, msg_id: MsgId) {
    if let Some(msg) = message(live, peer_id, msg_id)
        && live.media_cache().cancel_media_download_for_message(&msg)
    {
        live.bridge_state().notify_ui();
    }
}
