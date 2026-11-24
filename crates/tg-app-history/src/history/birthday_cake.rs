//! Birthday Suggestion `cake.tgs` Lottie emblem decoder, frame cache, and texture uploader.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/media/history_view_birthday_suggestion.cpp`
//!   (`ChatHelpers::GenerateLocalTgsSticker(session, u"cake"_q)`, `stopOnLastFrame = true`)
//! - `Telegram/Resources/animations/cake.tgs` (GPL-3.0 Telegram Desktop asset)

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use tg_lottie::{FrameBuffer, Instance, Size as LottieSize};

/// Bundled GPL-3.0 `cake.tgs` Lottie animation from `Telegram/Resources/animations/cake.tgs`.
pub const BUNDLED_CAKE_TGS: &[u8] = include_bytes!("../../assets/animations/cake.tgs");

/// Rasterized texture resolution for the birthday cake emblem (2x crispness for ~84-120px display).
const CAKE_RASTER_PX: u32 = 160;
/// Quantized animation frame rate matching `tg-app-history`'s Lottie pipeline.
const CAKE_ANIM_FPS: f64 = 24.0;

#[derive(Default)]
struct CakeLottieCache {
    /// Compiled Lottie instances keyed by content hash.
    instances: HashMap<u64, Option<Instance>>,
    /// First-seen playback start time per message id so each card plays once and stops on last frame.
    start_times: HashMap<i32, f64>,
    /// Last render source flag (`true` if Lottie texture was used, `false` if vector fallback).
    last_rendered_lottie: bool,
}

thread_local! {
    static CAKE_LOTTIE_CACHE: RefCell<CakeLottieCache> = RefCell::new(CakeLottieCache::default());
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Decodes and rasterizes a single frame of `bytes` (`.tgs` gzipped Lottie or plain Lottie JSON)
/// into premultiplied RGBA pixels of size `side_px × side_px`.
///
/// Returns `None` if `bytes` is empty or fails `tg-lottie` decoding.
#[must_use]
pub fn rasterize_cake_tgs_frame(
    bytes: &[u8],
    side_px: u32,
    frame_index: Option<u32>,
) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.is_empty() || side_px == 0 {
        return None;
    }
    let inst = Instance::from_bytes(bytes, None).ok()?;
    let total = inst.frames_count().max(1);
    let idx = frame_index
        .unwrap_or_else(|| total.saturating_sub(1))
        .min(total - 1);
    let mut fb = FrameBuffer::new(LottieSize::new(side_px, side_px));
    inst.render_to_prepared(&mut fb, idx);
    Some((fb.width, fb.height, fb.data))
}

/// Resolves (and caches) an `egui::TextureHandle` for the birthday `cake.tgs` Lottie emblem.
///
/// - When `override_tgs` is `Some(bytes)`, uses those bytes (falling back to `None` if invalid, so
///   the caller falls back to the vector birthday cake illustration).
/// - When `override_tgs` is `None`, loads [`BUNDLED_CAKE_TGS`].
/// - Follows tdesktop's `stopOnLastFrame = true` (`history_view_birthday_suggestion.cpp:63`):
///   plays once when motion is enabled, then rests permanently on the final frame; when
///   `reduced_motion` is `true`, renders the settled final frame immediately with zero animation.
#[must_use]
pub fn resolve_cake_lottie_texture(
    ctx: &Context,
    msg_id: i32,
    override_tgs: Option<&[u8]>,
    now_secs: f64,
    reduced_motion: bool,
) -> Option<TextureHandle> {
    let bytes = override_tgs.unwrap_or(BUNDLED_CAKE_TGS);
    if bytes.is_empty() {
        CAKE_LOTTIE_CACHE.with(|c| c.borrow_mut().last_rendered_lottie = false);
        return None;
    }
    let key_hash = hash_bytes(bytes);

    CAKE_LOTTIE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let (total_frames, native_fps) = {
            let entry = cache
                .instances
                .entry(key_hash)
                .or_insert_with(|| Instance::from_bytes(bytes, None).ok());
            let Some(inst) = entry.as_ref() else {
                cache.last_rendered_lottie = false;
                return None;
            };
            (inst.frames_count().max(1), inst.frame_rate().max(1.0))
        };

        let duration_secs = f64::from(total_frames) / native_fps;
        let last_frame = total_frames.saturating_sub(1);

        let frame_idx = if reduced_motion || total_frames <= 1 {
            // Settled final frame: deterministic in static snapshots and respectful of reduced motion.
            last_frame
        } else {
            let start = *cache.start_times.entry(msg_id).or_insert(now_secs);
            let elapsed = (now_secs - start).max(0.0);
            if elapsed >= duration_secs {
                // `stopOnLastFrame = true` (`history_view_birthday_suggestion.cpp:63`)
                last_frame
            } else {
                let quantized_t = (elapsed * CAKE_ANIM_FPS).floor() / CAKE_ANIM_FPS;
                let f = (quantized_t * native_fps).floor().max(0.0) as u32;
                ctx.request_repaint_after(std::time::Duration::from_millis(42));
                f.min(last_frame)
            }
        };

        let tex_id = egui::Id::unique((
            "tg_birthday_cake_tgs_tex",
            key_hash,
            CAKE_RASTER_PX,
            frame_idx,
        ));
        if let Some(tex) = ctx.data(|d| d.get_temp::<TextureHandle>(tex_id)) {
            cache.last_rendered_lottie = true;
            return Some(tex);
        }

        let mut fb = FrameBuffer::new(LottieSize::new(CAKE_RASTER_PX, CAKE_RASTER_PX));
        if let Some(Some(inst)) = cache.instances.get(&key_hash) {
            inst.render_to_prepared(&mut fb, frame_idx);
        } else {
            cache.last_rendered_lottie = false;
            return None;
        }

        // Guard against a blank transparent decode by verifying at least some non-zero alpha.
        let has_visible_pixels = fb.data.as_chunks::<4>().0.iter().any(|px| px[3] > 8);
        if !has_visible_pixels {
            cache.last_rendered_lottie = false;
            return None;
        }

        let image =
            ColorImage::from_rgba_premultiplied([fb.width as usize, fb.height as usize], &fb.data);
        let tex = ctx.load_texture(
            format!("tg_birthday_cake_tgs_{key_hash:x}_{frame_idx}"),
            image,
            TextureOptions::LINEAR,
        );
        ctx.data_mut(|d| d.insert_temp(tex_id, tex.clone()));
        cache.last_rendered_lottie = true;
        Some(tex)
    })
}

/// Returns `true` if the most recent birthday cake emblem paint used a decoded `cake.tgs`
/// Lottie texture (rather than the vector fallback).
#[must_use]
pub fn last_cake_emblem_used_lottie() -> bool {
    CAKE_LOTTIE_CACHE.with(|c| c.borrow().last_rendered_lottie)
}

/// Clears the thread-local birthday cake Lottie cache (useful for deterministic tests).
pub fn clear_cake_lottie_cache() {
    CAKE_LOTTIE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        cache.instances.clear();
        cache.start_times.clear();
        cache.last_rendered_lottie = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_cake_tgs_decodes_and_rasterizes_visible_pixels() {
        let (w, h, rgba) =
            rasterize_cake_tgs_frame(BUNDLED_CAKE_TGS, 96, None).expect("bundled cake.tgs decodes");
        assert_eq!((w, h), (96, 96));
        let non_transparent = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] > 16)
            .count();
        assert!(
            non_transparent > 200,
            "expected rich visible cake.tgs pixels, got {non_transparent}"
        );
    }

    #[test]
    fn invalid_or_empty_bytes_return_none_for_vector_fallback() {
        assert!(rasterize_cake_tgs_frame(&[], 96, None).is_none());
        assert!(rasterize_cake_tgs_frame(b"not valid tgs", 96, None).is_none());
    }
}
