//! Reply header media preview: the 32x32 thumbnail of the replied-to message and the reply's
//! one-line text with its coloured media part.
//!
//! Port origin: the preview part of `Reply::paint`
//! (`Telegram/SourceFiles/history/view/history_view_reply.cpp`) and `Data::ReplyPreview`
//! (`Telegram/SourceFiles/data/data_reply_preview.cpp`): the blurred inline thumbnail is shown
//! at once and replaced by the real thumbnail when it is loaded; both are scaled to cover
//! `st::historyReplyPreview` and cropped to its centre.
//!
//! Internal design differs from tdesktop: `ReplyPreview::image` prepares the image on the UI
//! thread inside `paint`. Here painting only reads textures; the thumbnail is asked for by
//! [`ReplyPreviewTextures::poll`] (a few times a second while it loads, never per frame) and
//! decoded, cropped and (under a spoiler) blurred on a worker thread. A texture is cached per
//! message that holds the reply and per image identity ([`ReplyThumb::key`]), where tdesktop
//! keeps one `ReplyPreview` on the photo or document.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use egui::{Color32, ColorImage, CornerRadius, FontId, Pos2, Rect, TextureHandle, Ui, Vec2};
use tg_history::consts::{
    HISTORY_REPLY_PADDING, HISTORY_REPLY_PREVIEW, HISTORY_REPLY_PREVIEW_MARGIN,
};

use crate::history::backend::{HistoryBackend, ReplyHeader, ReplyThumb};
use crate::history::media::MediaAssetCache;
use tg_app_core::info::backend::SharedMediaThumb;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// `st::roundRadiusSmall` (`Images::Option::RoundSmall`).
const ROUND_RADIUS_SMALL: u8 = 3;
/// `st::messageQuoteStyle.outline`.
const QUOTE_OUTLINE: i32 = 3;
/// How often a thumbnail that is still downloading is asked for again.
const POLL_MS: i64 = 250;
/// Reply targets whose thumbnail state is kept before the cache starts over.
const ENTRIES_CAP: usize = 512;

/// `Reply::previewSkip`: how far the name and text move right when the header has a preview.
#[must_use]
pub const fn reply_preview_skip() -> i32 {
    QUOTE_OUTLINE
        + HISTORY_REPLY_PREVIEW_MARGIN.left
        + HISTORY_REPLY_PREVIEW
        + HISTORY_REPLY_PREVIEW_MARGIN.right
        - HISTORY_REPLY_PADDING.left
}

/// Where the preview is drawn inside a reply header at `reply_rect`
/// (`x + st::historyReplyPreviewMargin.left()`, `y + ...top()`, 32x32).
#[must_use]
pub fn reply_thumb_rect(reply_rect: Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            reply_rect.left() + HISTORY_REPLY_PREVIEW_MARGIN.left as f32,
            reply_rect.top() + HISTORY_REPLY_PREVIEW_MARGIN.top as f32,
        ),
        Vec2::splat(HISTORY_REPLY_PREVIEW as f32),
    )
}

/// UV rectangle of the centred square of a `width` x `height` image (the image is scaled to
/// cover the preview and its overflow is cut off evenly on both sides).
#[must_use]
pub fn center_square_uv(width: usize, height: usize) -> Rect {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let side = w.min(h);
    Rect::from_min_size(
        Pos2::new((w - side) * 0.5 / w, (h - side) * 0.5 / h),
        Vec2::new(side / w, side / h),
    )
}

/// Side the image is scaled down to before a spoiler blur (`ReplyPreview::prepare` scales the
/// original to 40x40 and blurs it).
const SPOILER_BLUR_SIDE: u32 = 40;

/// Decodes thumbnail `bytes` and returns their centred square scaled to `side_px`; with `blur`
/// the square is blurred like the inline thumbnail (media under a spoiler).
#[must_use]
pub fn decode_reply_thumb(bytes: &[u8], side_px: u32, blur: bool) -> Option<ColorImage> {
    let decoded = tg_media::image::read_image(tg_media::image::ReadArgs {
        content: bytes.to_vec(),
        ..Default::default()
    })
    .ok()?;
    let (w, h) = decoded.image.dimensions();
    if w == 0 || h == 0 || side_px == 0 {
        return None;
    }
    let side = w.min(h);
    let mut square =
        image::imageops::crop_imm(&decoded.image, (w - side) / 2, (h - side) / 2, side, side)
            .to_image();
    let mut side = side;
    if blur {
        if side > SPOILER_BLUR_SIDE {
            square = image::imageops::resize(
                &square,
                SPOILER_BLUR_SIDE,
                SPOILER_BLUR_SIDE,
                image::imageops::FilterType::Triangle,
            );
            side = SPOILER_BLUR_SIDE;
        }
        square = crate::history::media::blur_small_rgba(&square);
    }
    let scaled = if side == side_px {
        square
    } else {
        image::imageops::resize(
            &square,
            side_px,
            side_px,
            image::imageops::FilterType::Triangle,
        )
    };
    Some(ColorImage::from_rgba_unmultiplied(
        [side_px as usize, side_px as usize],
        scaled.as_raw(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadState {
    /// The backend was not asked yet.
    NotAsked,
    /// The backend is downloading the thumbnail; asked again at `next_ask_ms`.
    Loading { next_ask_ms: i64 },
    /// The bytes are with the decode worker.
    Decoding,
    /// Loaded, or there is nothing to load.
    Done,
}

struct Entry {
    state: LoadState,
    texture: Option<TextureHandle>,
}

/// The chat, the message that holds the reply, and [`ReplyThumb::key`].
type Key = (PeerId, MsgId, u64);
type Decoded = (Key, Option<ColorImage>);

struct DecodeJob {
    key: Key,
    bytes: Vec<u8>,
    side_px: u32,
    blur: bool,
    ctx: egui::Context,
}

/// Real (downloaded) reply preview thumbnails per replied-to message.
pub struct ReplyPreviewTextures {
    entries: HashMap<Key, Entry>,
    /// Thumbnails painted without their file since the last poll, and whether they are
    /// blurred.
    wanted: Vec<(Key, bool)>,
    jobs: Option<Sender<DecodeJob>>,
    results_tx: Sender<Decoded>,
    results_rx: Receiver<Decoded>,
    backend_asks: u64,
}

impl Default for ReplyPreviewTextures {
    fn default() -> Self {
        let (results_tx, results_rx) = mpsc::channel();
        Self {
            entries: HashMap::new(),
            wanted: Vec::new(),
            jobs: None,
            results_tx,
            results_rx,
            backend_asks: 0,
        }
    }
}

impl ReplyPreviewTextures {
    /// The downloaded thumbnail of `key`, if loaded (blurred when asked with `blur`). One that
    /// is not loaded is remembered and asked for at the next [`Self::poll`].
    pub fn texture(&mut self, key: Key, blur: bool) -> Option<TextureHandle> {
        match self.entries.get(&key) {
            Some(Entry {
                texture: Some(texture),
                ..
            }) => Some(texture.clone()),
            Some(Entry {
                state: LoadState::Done | LoadState::Decoding,
                ..
            }) => None,
            _ => {
                if !self.wanted.iter().any(|(wanted, _)| *wanted == key) {
                    self.wanted.push((key, blur));
                }
                None
            }
        }
    }

    /// Uploads decoded thumbnails and asks `backend` for the thumbnails painted without one:
    /// once when first seen, then every [`POLL_MS`] while the download runs.
    pub fn poll(&mut self, ctx: &egui::Context, backend: &dyn HistoryBackend, now_ms: i64) {
        while let Ok((key, image)) = self.results_rx.try_recv() {
            let texture = image.map(|image| {
                ctx.load_texture(
                    format!("tg_reply_preview_{}_{}", key.0.0, key.1.0),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.entries.insert(
                key,
                Entry {
                    state: LoadState::Done,
                    texture,
                },
            );
        }
        // An edited target has a new image key: the entries of the old image are dropped.
        if self.wanted.iter().any(|(key, _)| {
            !self.entries.contains_key(key)
                && self.entries.keys().any(|k| k.0 == key.0 && k.1 == key.1)
        }) {
            let stale: Vec<Key> = self
                .wanted
                .iter()
                .flat_map(|(key, _)| {
                    self.entries
                        .keys()
                        .filter(move |k| k.0 == key.0 && k.1 == key.1 && k.2 != key.2)
                        .copied()
                })
                .collect();
            for key in stale {
                self.entries.remove(&key);
            }
        }
        if self.wanted.is_empty() {
            return;
        }
        if self.entries.len() >= ENTRIES_CAP {
            self.entries.retain(|_, e| e.state == LoadState::Decoding);
        }
        let side_px = (HISTORY_REPLY_PREVIEW as f32 * ctx.pixels_per_point()).ceil() as u32;
        for (key, blur) in std::mem::take(&mut self.wanted) {
            let state = self
                .entries
                .get(&key)
                .map_or(LoadState::NotAsked, |e| e.state);
            let due = match state {
                LoadState::NotAsked => true,
                LoadState::Loading { next_ask_ms } => {
                    now_ms >= next_ask_ms || now_ms < next_ask_ms - POLL_MS
                }
                LoadState::Decoding | LoadState::Done => false,
            };
            if !due {
                ctx.request_repaint_after(std::time::Duration::from_millis(POLL_MS as u64));
                continue;
            }
            self.backend_asks += 1;
            let next = match backend.reply_preview_thumb(key.0, key.1) {
                SharedMediaThumb::Ready { bytes } => {
                    self.decode(ctx, key, bytes, side_px, blur);
                    continue;
                }
                SharedMediaThumb::Loading { .. } => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(POLL_MS as u64));
                    LoadState::Loading {
                        next_ask_ms: now_ms + POLL_MS,
                    }
                }
                SharedMediaThumb::Unavailable => LoadState::Done,
            };
            self.entries.insert(
                key,
                Entry {
                    state: next,
                    texture: None,
                },
            );
        }
    }

    /// Decodes on the worker thread, or at once when background decoding is off (tests and
    /// screenshot harnesses, see [`tg_emoji::background`]).
    fn decode(&mut self, ctx: &egui::Context, key: Key, bytes: Vec<u8>, side_px: u32, blur: bool) {
        if !tg_emoji::background::background_decode_enabled() {
            let texture = decode_reply_thumb(&bytes, side_px, blur).map(|image| {
                ctx.load_texture(
                    format!("tg_reply_preview_{}_{}", key.0.0, key.1.0),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.entries.insert(
                key,
                Entry {
                    state: LoadState::Done,
                    texture,
                },
            );
            return;
        }
        self.entries.insert(
            key,
            Entry {
                state: LoadState::Decoding,
                texture: None,
            },
        );
        let job = DecodeJob {
            key,
            bytes,
            side_px,
            blur,
            ctx: ctx.clone(),
        };
        let jobs = self.jobs.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<DecodeJob>();
            let results = self.results_tx.clone();
            // The thread ends when this cache (the only sender) is dropped.
            let spawned = std::thread::Builder::new()
                .name("reply-preview-decode".to_owned())
                .spawn(move || {
                    for job in rx {
                        let image = decode_reply_thumb(&job.bytes, job.side_px, job.blur);
                        if results.send((job.key, image)).is_err() {
                            break;
                        }
                        job.ctx.request_repaint();
                    }
                });
            if spawned.is_err() {
                tracing::warn!("reply preview decode thread did not start");
            }
            tx
        });
        if jobs.send(job).is_err() {
            // No worker: leave the blurred thumbnail.
            self.entries.insert(
                key,
                Entry {
                    state: LoadState::Done,
                    texture: None,
                },
            );
        }
    }

    /// How many times the backend was asked for a thumbnail.
    #[must_use]
    pub const fn backend_asks(&self) -> u64 {
        self.backend_asks
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// What [`paint_reply_thumb`] drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyThumbPaint {
    /// The downloaded thumbnail.
    Real,
    /// The downloaded thumbnail, blurred: media under a spoiler without an inline thumbnail.
    RealBlurred,
    /// The blurred inline thumbnail.
    Blurred,
    /// A plain tint: no image is available.
    Tint,
}

/// `Ui::DefaultImageSpoiler` over the preview (`FillPreviewSpoiler`): white particles.
const SPOILER_PARTICLE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 255);

/// Whether the header of `reply` draws the animated spoiler particles over its preview
/// (`Reply::_spoiler`).
#[must_use]
pub fn reply_thumb_has_spoiler_particles(reply: &ReplyHeader) -> bool {
    reply.preview_thumb_rgb.is_some()
        && reply
            .preview_thumb
            .as_ref()
            .is_some_and(|t| t.spoiler && t.spoiler_animation)
}

/// Paints the reply header's media preview of the message `holder_id` and returns what was
/// drawn, or `None` when the header has no preview.
pub fn paint_reply_thumb(
    ui: &Ui,
    assets: &mut MediaAssetCache,
    peer_id: PeerId,
    holder_id: MsgId,
    reply: &ReplyHeader,
    reply_rect: Rect,
) -> Option<ReplyThumbPaint> {
    let painted = paint_reply_thumb_image(ui, assets, peer_id, holder_id, reply, reply_rect)?;
    if painted != ReplyThumbPaint::Tint && reply_thumb_has_spoiler_particles(reply) {
        tg_ui::effects::paint_spoiler_particles(
            ui.ctx(),
            ui.painter(),
            reply_thumb_rect(reply_rect),
            SPOILER_PARTICLE,
            Color32::TRANSPARENT,
            reply.preview_thumb.as_ref().map_or(0, |t| t.key) ^ 0x7E9B,
        );
    }
    Some(painted)
}

fn paint_reply_thumb_image(
    ui: &Ui,
    assets: &mut MediaAssetCache,
    peer_id: PeerId,
    holder_id: MsgId,
    reply: &ReplyHeader,
    reply_rect: Rect,
) -> Option<ReplyThumbPaint> {
    let tint = reply.preview_thumb_rgb?;
    let rect = reply_thumb_rect(reply_rect);
    let thumb = reply.preview_thumb.as_ref();
    let radius = if thumb.is_some_and(|t| t.round) {
        CornerRadius::same((HISTORY_REPLY_PREVIEW / 2) as u8)
    } else {
        CornerRadius::same(ROUND_RADIUS_SMALL)
    };
    let image = |texture: &TextureHandle, uv: Rect| {
        ui.painter().add(
            egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
                .with_texture(texture.id(), uv),
        );
    };
    // The file is used unless the media is under a spoiler and has an inline thumbnail
    // (`ReplyPreview::image`); under a spoiler it is blurred.
    let spoiler = thumb.is_some_and(|t| t.spoiler);
    let real = thumb
        .filter(|t| !t.spoiler || t.stripped.is_none())
        .and_then(|t| {
            assets
                .reply_previews
                .texture((peer_id, holder_id, t.key), t.spoiler)
        });
    if let Some(texture) = real {
        image(
            &texture,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        );
        return Some(if spoiler {
            ReplyThumbPaint::RealBlurred
        } else {
            ReplyThumbPaint::Real
        });
    }
    let blurred = thumb
        .and_then(|t: &ReplyThumb| t.stripped.as_deref())
        .and_then(|bytes| assets.get_stripped_texture(ui.ctx(), hash_bytes(bytes), bytes));
    if let Some(texture) = blurred {
        let [w, h] = texture.size();
        image(&texture, center_square_uv(w, h));
        return Some(ReplyThumbPaint::Blurred);
    }
    let [r, g, b] = tint;
    ui.painter()
        .rect_filled(rect, radius, Color32::from_rgb(r, g, b));
    Some(ReplyThumbPaint::Tint)
}

/// One laid-out line of a reply header's text.
pub type ReplyTextLine = tg_ui::spoiler_line::SpoilerLine;

/// One elided line of the reply text: its first `colored_len` bytes in `colored`
/// (`Ui::Text::Colorized`, drawn with the reply palette's link colour), the rest in `plain`,
/// and the byte ranges in `spoilers` hidden. The header never reveals them (`Reply::paint`
/// draws its text with `.spoiler` and no click handler: a click on the header jumps to the
/// message).
#[allow(clippy::too_many_arguments)]
pub fn reply_text_line(
    ui: &Ui,
    text: &str,
    colored_len: usize,
    spoilers: &[std::ops::Range<usize>],
    font: FontId,
    colored: Color32,
    plain: Color32,
    max_w: f32,
) -> ReplyTextLine {
    let text = text.replace('\n', " ");
    tg_ui::spoiler_line::layout_spoiler_line(
        ui.painter(),
        &text,
        colored_len,
        spoilers,
        font,
        colored,
        plain,
        max_w,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use tg_ui::spoiler_line::{hidden_glyph_rects, spoiler_line_job as reply_text_job};

    fn layout(
        text: &str,
        colored_len: usize,
        spoiler: Option<std::ops::Range<usize>>,
        max_w: f32,
    ) -> (Arc<egui::Galley>, Vec<Rect>) {
        let ctx = egui::Context::default();
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
        let mut out = None;
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (job, hidden) = reply_text_job(
                text,
                colored_len,
                spoiler.as_slice(),
                FontId::proportional(13.0),
                Color32::BLUE,
                Color32::BLACK,
                max_w,
            );
            let galley = ui.painter().layout_job(job);
            let rects = hidden_glyph_rects(&galley, &hidden);
            out = Some((galley, rects));
        });
        frame.textures_delta.clear();
        out.unwrap_or_else(|| panic!("the frame ran"))
    }

    /// Where the glyph of byte `at` starts in a reply line of `text`.
    fn x_of(galley: &egui::Galley, text: &str, at: usize) -> f32 {
        let index = text[..at].chars().count();
        galley.rows[0].row.glyphs[index].pos.x
    }

    #[test]
    fn a_reply_spoiler_is_transparent_and_covered_where_its_glyphs_are() {
        // TGB-3027: "Photo, " coloured, "secret" hidden under the particle cover.
        let text = "Photo, look secret here";
        let at = text.find("secret").unwrap_or(0);
        let (galley, rects) = layout(text, "Photo,".len(), Some(at..at + 6), 1000.0);
        let colors: Vec<(String, Color32)> = galley
            .job
            .sections
            .iter()
            .map(|s| {
                let range = s.byte_range.start.0..s.byte_range.end.0;
                (text[range].to_owned(), s.format.color)
            })
            .collect();
        assert_eq!(
            colors,
            [
                ("Photo,".to_owned(), Color32::BLUE),
                (" look ".to_owned(), Color32::BLACK),
                ("secret".to_owned(), Color32::TRANSPARENT),
                (" here".to_owned(), Color32::BLACK),
            ]
        );
        assert_eq!(rects.len(), 1, "{rects:?}");
        let left = x_of(&galley, text, at);
        let right = x_of(&galley, text, at + 6);
        assert!((rects[0].min.x - left).abs() < 0.5, "{rects:?} vs {left}");
        assert!((rects[0].max.x - right).abs() < 1.0, "{rects:?} vs {right}");
        assert!(rects[0].height() > 10.0);
    }

    #[test]
    fn the_elision_mark_of_a_cut_spoiler_is_not_covered() {
        let text = "a secret that is much longer than the reply header";
        let (galley, rects) = layout(text, 0, Some(2..text.len()), 80.0);
        assert!(galley.elided);
        assert_eq!(rects.len(), 1);
        let last = galley.rows[0].row.glyphs.last().map(|g| g.pos.x);
        assert!(last.is_some_and(|x| rects[0].max.x <= x + 0.5), "{rects:?}");
        // A spoiler that starts after the cut has nothing on screen to cover.
        let (_, rects) = layout(text, 0, Some(text.len() - 6..text.len()), 80.0);
        assert!(rects.is_empty(), "{rects:?}");
        // Ranges off char boundaries or past the end are clamped, never panic.
        let (_, rects) = layout("ü secret", 0, Some(1..500), 1000.0);
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn preview_geometry_matches_tdesktop() {
        // outline 3 + margin 7 + preview 32 + margin 4 - padding 11.
        assert_eq!(reply_preview_skip(), 35);
        let reply = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(300.0, 40.0));
        let thumb = reply_thumb_rect(reply);
        assert_eq!(thumb.min, Pos2::new(107.0, 54.0));
        assert_eq!(thumb.size(), Vec2::splat(32.0));
    }

    #[test]
    fn a_wide_image_is_cropped_to_its_centre() {
        let uv = center_square_uv(80, 40);
        assert_eq!(uv.min, Pos2::new(0.25, 0.0));
        assert_eq!(uv.size(), Vec2::new(0.5, 1.0));
        let uv = center_square_uv(30, 40);
        assert_eq!(uv.min, Pos2::new(0.0, 0.125));
        assert_eq!(uv.size(), Vec2::new(1.0, 0.75));
    }

    #[test]
    fn a_thumbnail_decodes_to_a_centred_square() {
        // 60x20: left third red, middle green, right third blue.
        let mut img = image::RgbaImage::new(60, 20);
        for (x, _, px) in img.enumerate_pixels_mut() {
            *px = match x / 20 {
                0 => image::Rgba([255, 0, 0, 255]),
                1 => image::Rgba([0, 255, 0, 255]),
                _ => image::Rgba([0, 0, 255, 255]),
            };
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode png");
        let out = decode_reply_thumb(&png, 32, false).expect("decoded");
        assert_eq!(out.size, [32, 32]);
        assert!(
            out.pixels
                .iter()
                .all(|p| *p == Color32::from_rgb(0, 255, 0))
        );
        assert!(decode_reply_thumb(b"not an image", 32, false).is_none());
    }
}
