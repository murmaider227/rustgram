//! Real (photo) userpics for any peer, painted in place of [`crate::userpic::EmptyUserpic`].
//!
//! Port of the image half of `PeerData::paintUserpic` (`data/data_peer.cpp`): when the peer has a
//! loaded small profile photo (`PeerData::userpicCloudImage` / `Data::PeerUserpicView`), tdesktop
//! paints it as a circle; until it loads it paints the empty userpic. The session installs a
//! [`UserpicImageSource`] on the `egui::Context`; widgets call [`paint_userpic_image`] with the
//! peer's full id and fall back to the empty userpic when it returns `false`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, Context, Id, Painter, Pos2, Rect, TextureHandle, TextureOptions};

/// A decoded small profile photo: a square RGBA8 image, not masked (unmultiplied alpha).
///
/// [`paint_userpic_image`] scales it to the size it is drawn at and masks it to a circle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserpicImage {
    /// Photo id the pixels belong to (a new id means a changed userpic).
    pub photo_id: i64,
    /// Pixel width.
    pub width: usize,
    /// Pixel height.
    pub height: usize,
    /// RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Supplies decoded userpics by full peer id (`PeerId.0`); `None` while unknown or loading.
///
/// Called from the UI thread on every paint, so implementations must not block: start the
/// download on the first call and ask the UI to repaint when it finishes.
pub trait UserpicImageSource: Send + Sync {
    /// Returns `peer_id`'s loaded userpic, starting its download if needed.
    fn userpic(&self, peer_id: i64) -> Option<Arc<UserpicImage>>;
}

#[derive(Clone)]
struct SourceSlot(Arc<dyn UserpicImageSource>);

/// The circles one peer's photo was prepared at: one texture per pixel size it is drawn at
/// (`Ui::PeerUserpicView` keeps the scaled, rounded image for its size).
struct PeerCircles {
    photo_id: i64,
    sizes: Vec<PreparedCircle>,
}

struct PreparedCircle {
    side_px: usize,
    /// Corner radius in pixels: `0` for a circle, else a rounded square (forums).
    corner_px: usize,
    texture: TextureHandle,
    /// The pass the circle was last painted in.
    used: u64,
}

/// Sizes kept per peer. A userpic is drawn at a handful of sizes (chat list, history, top bar,
/// profile); a size that animates replaces the one unused for the longest.
const MAX_SIZES_PER_PEER: usize = 6;
/// More peers than this drops the ones not painted for [`STALE_PASSES`] passes.
const MAX_PEERS: usize = 2048;
const STALE_PASSES: u64 = 600;

type TextureMap = Arc<Mutex<HashMap<i64, PeerCircles>>>;

fn source_id() -> Id {
    Id::unique("tg_ui_userpic_image_source")
}

fn textures_id() -> Id {
    Id::unique("tg_ui_userpic_image_textures")
}

/// Installs (or replaces) the userpic source used by [`paint_userpic_image`] on `ctx`.
pub fn install_userpic_source(ctx: &Context, source: Arc<dyn UserpicImageSource>) {
    ctx.data_mut(|d| {
        d.insert_temp(source_id(), SourceSlot(source));
        d.insert_temp(textures_id(), TextureMap::default());
    });
}

/// Removes the userpic source from `ctx` (logout): every userpic falls back to the empty one.
pub fn remove_userpic_source(ctx: &Context) {
    ctx.data_mut(|d| {
        d.remove::<SourceSlot>(source_id());
        d.remove::<TextureMap>(textures_id());
    });
}

/// Returns `peer_id`'s decoded userpic from the source installed on `ctx`, if loaded.
#[must_use]
pub fn userpic_image(ctx: &Context, peer_id: i64) -> Option<Arc<UserpicImage>> {
    if peer_id == 0 {
        return None;
    }
    let source = ctx.data(|d| d.get_temp::<SourceSlot>(source_id()))?;
    source.0.userpic(peer_id)
}

/// Paints `peer_id`'s photo userpic as a circle filling `rect`.
///
/// The photo is scaled to the exact pixel size of `rect` at the painter's pixels-per-point and
/// masked there, once per size, so the circle's edge is antialiased on the real pixel grid
/// (`Images::Circle` on the image scaled for `style::DevicePixelRatio()`).
///
/// Returns `false` (painting nothing) when no photo is loaded yet; the caller then paints the
/// empty userpic.
pub fn paint_userpic_image(painter: &Painter, rect: Rect, peer_id: i64) -> bool {
    paint_userpic_image_shaped(painter, rect, peer_id, None)
}

/// Paints `peer_id`'s photo userpic as a rounded square filling `rect` with corner `radius`
/// (`Ui::PeerUserpicShape::Forum`: forums and communities, radius
/// `size * Ui::ForumUserpicRadiusMultiplier()`).
///
/// Returns `false` (painting nothing) when no photo is loaded yet; the caller then paints the
/// empty userpic with the same shape.
pub fn paint_userpic_image_rounded(
    painter: &Painter,
    rect: Rect,
    peer_id: i64,
    radius: f32,
) -> bool {
    paint_userpic_image_shaped(painter, rect, peer_id, Some(radius))
}

fn paint_userpic_image_shaped(
    painter: &Painter,
    rect: Rect,
    peer_id: i64,
    corner_radius: Option<f32>,
) -> bool {
    let ctx = painter.ctx();
    let Some(image) = userpic_image(ctx, peer_id) else {
        return false;
    };
    let expected = image.width.saturating_mul(image.height).saturating_mul(4);
    if image.width == 0 || image.height == 0 || image.rgba.len() != expected {
        return false;
    }
    let pixels_per_point = painter.pixels_per_point();
    let side = rect.width().min(rect.height());
    let side_px = (side * pixels_per_point).round();
    if !(1.0..=4096.0).contains(&side_px) {
        return false;
    }
    let side_px = side_px as usize;
    let corner_px = corner_radius.map_or(0, |radius| {
        ((radius * pixels_per_point).round().max(1.0) as usize).min(side_px / 2)
    });
    let textures = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<TextureMap>(textures_id())
            .clone()
    });
    let Ok(mut map) = textures.lock() else {
        return false;
    };
    let pass = ctx.cumulative_pass_nr();
    if map.len() > MAX_PEERS {
        map.retain(|_, peer| {
            peer.sizes
                .iter()
                .any(|circle| circle.used + STALE_PASSES >= pass)
        });
    }
    let peer = map.entry(peer_id).or_insert_with(|| PeerCircles {
        photo_id: image.photo_id,
        sizes: Vec::new(),
    });
    if peer.photo_id != image.photo_id {
        // A changed photo: the old circles go.
        peer.photo_id = image.photo_id;
        peer.sizes.clear();
    }
    let texture = if let Some(circle) = peer
        .sizes
        .iter_mut()
        .find(|c| c.side_px == side_px && c.corner_px == corner_px)
    {
        circle.used = pass;
        circle.texture.clone()
    } else {
        if peer.sizes.len() >= MAX_SIZES_PER_PEER
            && let Some(oldest) = (0..peer.sizes.len()).min_by_key(|&i| peer.sizes[i].used)
        {
            peer.sizes.swap_remove(oldest);
        }
        let pixels = if corner_px == 0 {
            prepare_circle(image.width, image.height, &image.rgba, side_px)
        } else {
            prepare_rounded(image.width, image.height, &image.rgba, side_px, corner_px)
        };
        let texture = ctx.load_texture(
            format!("userpic_{peer_id}_{}_{side_px}_{corner_px}", image.photo_id),
            ColorImage::from_rgba_premultiplied([side_px, side_px], &pixels),
            TextureOptions::LINEAR,
        );
        peer.sizes.push(PreparedCircle {
            side_px,
            corner_px,
            texture: texture.clone(),
            used: pass,
        });
        texture
    };
    drop(map);
    // One texture pixel per screen pixel: the square starts on the pixel grid.
    let center = rect.center();
    let snap = |v: f32| (v * pixels_per_point).round() / pixels_per_point;
    let square = Rect::from_min_size(
        Pos2::new(snap(center.x - side * 0.5), snap(center.y - side * 0.5)),
        egui::Vec2::splat(side_px as f32 / pixels_per_point),
    );
    painter.image(
        texture.id(),
        square,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
    true
}

/// For each of `dst` output samples, the `src` samples it averages and their weights.
///
/// Scaling down averages every source sample the output pixel covers (an area filter, like
/// `Qt::SmoothTransformation`); scaling up blends the two nearest.
fn resample_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f32 / dst as f32;
    (0..dst)
        .map(|i| {
            if dst >= src {
                let pos = ((i as f32 + 0.5) * scale - 0.5).clamp(0.0, (src - 1) as f32);
                let first = pos.floor() as usize;
                let frac = pos - first as f32;
                if frac <= 0.0 || first + 1 >= src {
                    vec![(first, 1.0)]
                } else {
                    vec![(first, 1.0 - frac), (first + 1, frac)]
                }
            } else {
                let from = i as f32 * scale;
                let to = (i as f32 + 1.0) * scale;
                let first = from.floor() as usize;
                let last = (to.ceil() as usize).min(src);
                (first..last)
                    .filter_map(|s| {
                        let covered = (to.min(s as f32 + 1.0) - from.max(s as f32)) / scale;
                        (covered > 0.0).then_some((s, covered))
                    })
                    .collect()
            }
        })
        .collect()
}

/// Scales an RGBA8 image (`src_w` x `src_h`, unmultiplied alpha) to `side` x `side` pixels and
/// masks it to a circle. Returns premultiplied RGBA8.
///
/// A pixel the circle's edge crosses keeps the share of it the circle covers
/// (`Images::Circle`: an antialiased ellipse as the alpha mask), so the edge is a smooth ramp
/// about one pixel wide at any size.
#[must_use]
pub fn prepare_circle(src_w: usize, src_h: usize, rgba: &[u8], side: usize) -> Vec<u8> {
    prepare_shape(src_w, src_h, rgba, side, side as f32 * 0.5)
}

/// Like [`prepare_circle`], masked to a square with corners of `corner` pixels
/// (`Images::Round` with `ImageRoundRadius` for a forum userpic).
#[must_use]
pub fn prepare_rounded(
    src_w: usize,
    src_h: usize,
    rgba: &[u8],
    side: usize,
    corner: usize,
) -> Vec<u8> {
    prepare_shape(src_w, src_h, rgba, side, corner as f32)
}

/// Share of the pixel centred at (`x`, `y`) that a `side` square with `corner` radius covers;
/// `corner = side / 2` is a circle.
fn rounded_square_coverage(x: f32, y: f32, side: f32, corner: f32) -> f32 {
    let half = side * 0.5;
    let inner = half - corner;
    let qx = (x - half).abs() - inner;
    let qy = (y - half).abs() - inner;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let distance = outside + qx.max(qy).min(0.0) - corner;
    (0.5 - distance).clamp(0.0, 1.0)
}

fn prepare_shape(src_w: usize, src_h: usize, rgba: &[u8], side: usize, corner: f32) -> Vec<u8> {
    let mut out = vec![0u8; side * side * 4];
    if side == 0 || src_w == 0 || src_h == 0 || rgba.len() != src_w * src_h * 4 {
        return out;
    }
    // Horizontal pass into premultiplied floats, `side` x `src_h`.
    let columns = resample_weights(src_w, side);
    let mut rows = vec![[0.0f32; 4]; side * src_h];
    for y in 0..src_h {
        let line = &rgba[y * src_w * 4..(y + 1) * src_w * 4];
        for (x, weights) in columns.iter().enumerate() {
            let mut sum = [0.0f32; 4];
            for &(sx, weight) in weights {
                let px = &line[sx * 4..sx * 4 + 4];
                let alpha = f32::from(px[3]) / 255.0 * weight;
                sum[0] += f32::from(px[0]) * alpha;
                sum[1] += f32::from(px[1]) * alpha;
                sum[2] += f32::from(px[2]) * alpha;
                sum[3] += 255.0 * alpha;
            }
            rows[y * side + x] = sum;
        }
    }
    // Vertical pass, then the shape's coverage.
    let lines = resample_weights(src_h, side);
    for (y, weights) in lines.iter().enumerate() {
        for x in 0..side {
            let coverage =
                rounded_square_coverage(x as f32 + 0.5, y as f32 + 0.5, side as f32, corner);
            if coverage <= 0.0 {
                continue;
            }
            let mut sum = [0.0f32; 4];
            for &(sy, weight) in weights {
                let px = rows[sy * side + x];
                for (total, value) in sum.iter_mut().zip(px) {
                    *total += value * weight;
                }
            }
            let at = (y * side + x) * 4;
            for (byte, total) in out[at..at + 4].iter_mut().zip(sum) {
                *byte = (total * coverage).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Option<Arc<UserpicImage>>);

    impl UserpicImageSource for Fixed {
        fn userpic(&self, peer_id: i64) -> Option<Arc<UserpicImage>> {
            (peer_id == 42).then(|| self.0.clone()).flatten()
        }
    }

    fn image() -> Arc<UserpicImage> {
        Arc::new(UserpicImage {
            photo_id: 7,
            width: 4,
            height: 4,
            rgba: vec![200; 64],
        })
    }

    fn white(side: usize) -> UserpicImage {
        UserpicImage {
            photo_id: 1,
            width: side,
            height: side,
            rgba: vec![255; side * side * 4],
        }
    }

    #[test]
    fn paints_only_loaded_userpics() {
        let ctx = Context::default();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(42.0, 42.0));
        assert!(
            !paint_userpic_image(&painter, rect, 42),
            "no source installed"
        );
        install_userpic_source(&ctx, Arc::new(Fixed(Some(image()))));
        assert!(paint_userpic_image(&painter, rect, 42));
        assert!(!paint_userpic_image(&painter, rect, 43), "unknown peer");
        remove_userpic_source(&ctx);
        assert!(
            !paint_userpic_image(&painter, rect, 42),
            "removed on logout"
        );
    }

    /// The alpha of a white photo's circle is the share of each pixel the circle covers:
    /// a ramp about one pixel wide at the chat-list size, at 100% (46 px) and 150% (69 px).
    #[test]
    fn prepared_circle_edge_is_a_one_pixel_ramp() {
        for side in [46usize, 69] {
            let pixels = prepare_circle(160, 160, &white(160).rgba, side);
            let radius = side as f32 * 0.5;
            let (mut partial, mut ring) = (0usize, 0usize);
            for y in 0..side {
                for x in 0..side {
                    let dx = x as f32 + 0.5 - radius;
                    let dy = y as f32 + 0.5 - radius;
                    let distance = (dx * dx + dy * dy).sqrt();
                    let alpha = pixels[(y * side + x) * 4 + 3];
                    let covered = (radius - distance + 0.5).clamp(0.0, 1.0);
                    assert!(
                        (f32::from(alpha) / 255.0 - covered).abs() <= 0.01,
                        "{side} px: pixel ({x}, {y}) has alpha {alpha}, the circle covers {covered}"
                    );
                    if (distance - radius).abs() < 0.45 {
                        ring += 1;
                        if alpha > 0 && alpha < 255 {
                            partial += 1;
                        }
                    }
                }
            }
            assert!(ring > side, "{side} px: {ring} edge pixels");
            assert_eq!(
                partial, ring,
                "{side} px: every pixel the edge crosses is partly covered"
            );
            assert_eq!(pixels[3], 0, "{side} px: the corner is clear");
            let middle = (side / 2 * side + side / 2) * 4;
            assert_eq!(
                pixels[middle..middle + 4],
                [255; 4],
                "{side} px: the middle"
            );
        }
    }

    /// Scaling down averages the photo (no skipped pixels); scaling up blends neighbours.
    #[test]
    fn prepared_circle_scales_the_photo_by_area() {
        // Black and white columns, two pixels each: a 4:1 reduction is mid grey.
        let side = 160usize;
        let mut rgba = Vec::with_capacity(side * side * 4);
        for _y in 0..side {
            for x in 0..side {
                let v = if x % 4 < 2 { 0 } else { 255 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let striped = UserpicImage {
            photo_id: 2,
            width: side,
            height: side,
            rgba,
        };
        let small = prepare_circle(side, side, &striped.rgba, 40);
        let middle = (20 * 40 + 20) * 4;
        assert!(
            (i32::from(small[middle]) - 128).abs() <= 1,
            "a 4:1 reduction of the stripes is grey, got {}",
            small[middle]
        );
        let large = prepare_circle(8, 8, &white(8).rgba, 64);
        let middle = (32 * 64 + 32) * 4;
        assert_eq!(large[middle..middle + 4], [255; 4]);
    }

    #[test]
    fn each_size_gets_one_circle_and_an_animated_size_is_capped() {
        let ctx = Context::default();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        install_userpic_source(&ctx, Arc::new(Fixed(Some(image()))));
        let sizes = |ctx: &Context| -> Vec<usize> {
            let map = ctx.data_mut(|d| {
                d.get_temp_mut_or_default::<TextureMap>(textures_id())
                    .clone()
            });
            let map = map.lock().expect("texture map");
            let mut sizes: Vec<usize> = map
                .get(&42)
                .map(|peer| peer.sizes.iter().map(|c| c.side_px).collect())
                .unwrap_or_default();
            sizes.sort_unstable();
            sizes
        };
        for side in [46.0, 33.0, 46.0, 33.0] {
            let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(side, side));
            assert!(paint_userpic_image(&painter, rect, 42));
        }
        assert_eq!(sizes(&ctx), [33, 46], "one circle per size, reused");
        for side in 10..30u8 {
            let rect = Rect::from_min_size(Pos2::ZERO, egui::Vec2::splat(f32::from(side)));
            assert!(paint_userpic_image(&painter, rect, 42));
        }
        assert_eq!(
            sizes(&ctx).len(),
            MAX_SIZES_PER_PEER,
            "a size that animates does not pile up textures"
        );
    }
}
