//! Live pictures for the sticker set box (TGB-3126).
//!
//! `messages.getStickerSet` lists the pack's documents with their file references but no
//! picture bytes (only an optional stripped thumbnail, which stickers rarely have), so the box
//! showed a placeholder in every cell. tdesktop's `StickerSetBox::Inner` loads each sticker's
//! media from the pack (`Data::FileOriginStickerSet`) and repaints when it lands; this source
//! does the same through the chat helpers' sticker loader, which downloads with the pack's
//! file reference, and turns what it gets into a still picture: the WebP itself, the first
//! frame of a Lottie sticker, the thumbnail of a video sticker.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tg_app_core::chat_helpers::backend::StickerMedia;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_chat::DocumentId as ChatDocId;
use tg_session::SessionRuntime;
use tg_ui::sticker_thumbs::{StickerThumb, StickerThumbSource};

use crate::bridge::LiveBridgeState;
use crate::chat_helpers::LiveChatHelpersState;
use crate::session::LiveSession;

/// Side the first frame of a Lottie sticker is rendered at (a box cell is about 72 px).
const LOTTIE_SIDE: u32 = 160;

/// [`StickerThumbSource`] of a live session; holds only weak references to the session.
pub struct LiveStickerThumbs {
    runtime: Weak<SessionRuntime<ErasedRpcClient>>,
    bridge: Weak<LiveBridgeState>,
    helpers: Weak<LiveChatHelpersState>,
    /// Converted pictures by document id; `None`: the media could not be turned into one.
    decoded: Mutex<HashMap<u64, Option<Arc<StickerThumb>>>>,
}

impl LiveStickerThumbs {
    /// Creates a source for `live`.
    #[must_use]
    pub fn new(live: &LiveSession) -> Self {
        Self {
            runtime: Arc::downgrade(live.runtime()),
            bridge: Arc::downgrade(live.bridge_state()),
            helpers: Arc::downgrade(&live.chat_helpers.state),
            decoded: Mutex::new(HashMap::new()),
        }
    }
}

impl StickerThumbSource for LiveStickerThumbs {
    fn sticker_thumb(&self, document_id: u64) -> Option<Arc<StickerThumb>> {
        if let Ok(map) = self.decoded.lock()
            && let Some(done) = map.get(&document_id)
        {
            return done.clone();
        }
        let runtime = self.runtime.upgrade()?;
        let bridge = self.bridge.upgrade()?;
        let helpers = self.helpers.upgrade()?;
        let media = crate::chat_helpers::load_sticker_media(
            &helpers,
            &runtime,
            &bridge,
            ChatDocId(document_id),
        )?;
        let thumb = still_picture(media).map(Arc::new);
        if let Ok(mut map) = self.decoded.lock() {
            map.insert(document_id, thumb.clone());
        }
        thumb
    }
}

/// A still picture of a sticker's media.
fn still_picture(media: StickerMedia) -> Option<StickerThumb> {
    match media {
        StickerMedia::StaticRgba {
            width,
            height,
            rgba,
        } => (width > 0 && height > 0 && rgba.len() == (width * height * 4) as usize).then_some(
            StickerThumb {
                width,
                height,
                rgba,
            },
        ),
        StickerMedia::LottieJson(json) => lottie_first_frame(&json),
        StickerMedia::LottieTgs(tgs) => {
            let json = crate::media_cache::decompress_tgs_to_lottie_json(&tgs).ok()?;
            lottie_first_frame(&json)
        }
    }
}

fn lottie_first_frame(json: &str) -> Option<StickerThumb> {
    let comp = tg_lottie::Composition::from_json(json).ok()?;
    let frame =
        tg_lottie::render_composition(&comp, 0.0, tg_lottie::Size::new(LOTTIE_SIDE, LOTTIE_SIDE));
    let (width, height) = (frame.width, frame.height);
    if width == 0 || height == 0 {
        return None;
    }
    // The renderer gives premultiplied pixels; the box uploads unmultiplied ones.
    let mut rgba = frame.data;
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Some(StickerThumb {
        width,
        height,
        rgba,
    })
}

/// Installs `live`'s sticker picture source on `ctx` unless it is already the installed one
/// (cheap; called every frame with the userpic source so an account switch rebinds it).
pub fn ensure_live_sticker_thumbs(ctx: &egui::Context, live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let id = egui::Id::unique("tg_app_live_sticker_thumb_source_key");
    if ctx.data(|d| d.get_temp::<usize>(id)) != Some(key) {
        tg_ui::sticker_thumbs::install_sticker_thumb_source(
            ctx,
            Arc::new(LiveStickerThumbs::new(live)),
        );
        ctx.data_mut(|d| d.insert_temp(id, key));
    }
}

#[cfg(test)]
#[path = "tests_sticker_set_thumbs.rs"]
#[cfg(test)]
mod tests;
