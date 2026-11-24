//! Userpic generation and temporary PNG caching for native toast notifications.
//!
//! Port of `Telegram/SourceFiles/window/notifications_utilities.h` and
//! `Telegram/SourceFiles/window/notifications_utilities.cpp`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tg_platform::system_info::RgbaColor;
use tg_platform::tray::{RgbaImage, default_app_icon};

use crate::notifications::backend::NotifyPeerInfo;

/// Temporary toast avatar lifetime in milliseconds (`kNotifyDeletePhotoAfterMs = 60000`).
pub const NOTIFY_DELETE_PHOTO_AFTER_MS: i64 = 60_000;

/// Default raster size for native toast avatars (`st::notifyMacPhotoSize = 64`).
pub const NOTIFY_USERPIC_SIZE: u32 = 64;

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(1);

/// Cache key matching tdesktop's `InMemoryKey` (`(first, second)`).
///
/// - `(0, 0)` represents the hidden/generic application icon placeholder (never auto-expired).
/// - `(peer_id, revision)` represents a peer-specific avatar that expires after 60 seconds of inactivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserpicKey {
    /// Primary key (`0` for persistent placeholder, non-zero peer ID for temporary peer photo).
    pub first: u64,
    /// Secondary revision key (e.g. photo hash or color hash).
    pub second: u64,
}

impl UserpicKey {
    /// Key for the generic hidden-sender app placeholder (`(0, 0)`).
    pub const HIDDEN_PLACEHOLDER: Self = Self {
        first: 0,
        second: 0,
    };

    /// Builds a cache key for a peer's avatar.
    #[must_use]
    pub const fn for_peer(peer_id: i64, revision: u64) -> Self {
        let raw = peer_id as u64;
        Self {
            first: if raw == 0 { 1 } else { raw },
            second: revision,
        }
    }
}

/// Generates a crisp circular RGBA avatar image for a peer (`GenerateUserpic`).
///
/// Mirrors `EmptyUserpic::GenerateSavedMessages` / `GenerateRepliesMessages` /
/// `PeerData::GenerateUserpicImage` without copying any trademarked assets.
#[must_use]
pub fn generate_userpic(peer: Option<&NotifyPeerInfo>, size: u32) -> RgbaImage {
    let dim = size.max(16);
    let Some(info) = peer else {
        return default_app_icon(dim, false, false);
    };

    let bg = if info.is_self {
        RgbaColor::rgb(0x40, 0xA7, 0xE3)
    } else if info.is_replies_chat {
        RgbaColor::rgb(0x52, 0x99, 0xE5)
    } else {
        RgbaColor::rgb(info.avatar_rgb[0], info.avatar_rgb[1], info.avatar_rgb[2])
    };
    let fg = RgbaColor::rgb(0xFF, 0xFF, 0xFF);

    let mut img = RgbaImage::transparent(dim, dim);
    draw_filled_circle(
        &mut img,
        dim as f32 * 0.5,
        dim as f32 * 0.5,
        dim as f32 * 0.48,
        bg,
    );

    if info.is_self {
        // Simple geometric bookmark silhouette for Saved Messages.
        draw_bookmark_glyph(&mut img, fg);
    } else if info.is_replies_chat {
        // Simple geometric reply arrow for Replies chat.
        draw_initials(&mut img, "RE", fg);
    } else {
        let letters = if info.initials.is_empty() {
            initials_from_title(&info.title)
        } else {
            info.initials.clone()
        };
        draw_initials(&mut img, &letters, fg);
    }
    img
}

/// Extracts up to two uppercase ASCII initials from a chat or sender title.
#[must_use]
pub fn initials_from_title(title: &str) -> String {
    let mut out = String::with_capacity(2);
    for word in title.split_whitespace().take(2) {
        if let Some(ch) = word.chars().next() {
            out.push(ch.to_ascii_uppercase());
        }
    }
    if out.is_empty() {
        out.push('T');
    }
    out
}

#[derive(Debug, Clone)]
struct CachedImageEntry {
    path: PathBuf,
    /// Expiration timestamp in ms (`0` means persistent until drop).
    until_ms: i64,
}

/// Temporary PNG cache for native toast notification avatars (`Window::Notifications::CachedUserpics`).
///
/// Writes circular 64x64 PNGs to a dedicated temporary directory, refreshes each peer's
/// 60-second TTL on access, removes expired files on [`Self::clear_expired`], and deletes
/// all created PNG files on [`Drop`].
#[derive(Debug)]
pub struct CachedUserpics {
    cache_dir: PathBuf,
    images: BTreeMap<UserpicKey, CachedImageEntry>,
    some_saved: bool,
}

impl Default for CachedUserpics {
    fn default() -> Self {
        Self::new(default_temp_dir())
    }
}

impl CachedUserpics {
    /// Creates a [`CachedUserpics`] store writing PNGs inside `cache_dir`.
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            images: BTreeMap::new(),
            some_saved: false,
        }
    }

    /// Returns the directory where temporary avatar PNGs are stored.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Number of currently cached avatar entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Returns `true` if no avatar entries are currently cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Returns the filesystem path to a cached PNG avatar for `key`, generating and writing it
    /// on cache miss (`CachedUserpics::get`).
    pub fn get(
        &mut self,
        key: UserpicKey,
        peer: Option<&NotifyPeerInfo>,
        now_ms: i64,
    ) -> std::io::Result<PathBuf> {
        if let Some(existing) = self.images.get_mut(&key) {
            if existing.until_ms != 0 {
                existing.until_ms = now_ms.saturating_add(NOTIFY_DELETE_PHOTO_AFTER_MS);
            }
            if existing.path.exists() {
                return Ok(existing.path.clone());
            }
        }

        fs::create_dir_all(&self.cache_dir)?;
        let seq = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let filename = format!("notify_{}_{:x}.png", std::process::id(), seq);
        let path = self.cache_dir.join(filename);

        let image = if key.first != 0 || key.second != 0 {
            generate_userpic(peer, NOTIFY_USERPIC_SIZE)
        } else {
            default_app_icon(NOTIFY_USERPIC_SIZE, false, false)
        };
        let png_bytes = image
            .encode_png()
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        fs::write(&path, png_bytes)?;

        let until_ms = if key.first != 0 {
            now_ms.saturating_add(NOTIFY_DELETE_PHOTO_AFTER_MS)
        } else {
            0
        };
        self.images.insert(
            key,
            CachedImageEntry {
                path: path.clone(),
                until_ms,
            },
        );
        self.some_saved = true;
        Ok(path)
    }

    /// Deletes all cached PNGs whose TTL expired at or before `now_ms`.
    ///
    /// Returns the earliest remaining expiration timestamp in ms, if any (`CachedUserpics::clear`).
    pub fn clear_expired(&mut self, now_ms: i64) -> Option<i64> {
        let mut next_expiry: Option<i64> = None;
        let mut to_remove = Vec::new();
        for (&key, entry) in &self.images {
            if entry.until_ms == 0 {
                continue;
            }
            if entry.until_ms <= now_ms {
                let _ = fs::remove_file(&entry.path);
                to_remove.push(key);
            } else {
                next_expiry = Some(next_expiry.map_or(entry.until_ms, |m| m.min(entry.until_ms)));
            }
        }
        for key in to_remove {
            self.images.remove(&key);
        }
        next_expiry
    }

    /// Deletes all cached temporary PNG files immediately.
    pub fn clear_all(&mut self) {
        if self.some_saved {
            for entry in self.images.values() {
                let _ = fs::remove_file(&entry.path);
            }
        }
        self.images.clear();
        self.some_saved = false;
    }
}

impl Drop for CachedUserpics {
    fn drop(&mut self) {
        self.clear_all();
    }
}

fn default_temp_dir() -> PathBuf {
    std::env::temp_dir().join("rustgram-notify-userpics")
}

fn draw_filled_circle(img: &mut RgbaImage, cx: f32, cy: f32, radius: f32, color: RgbaColor) {
    let x0 = ((cx - radius - 1.0).floor() as i32).max(0);
    let y0 = ((cy - radius - 1.0).floor() as i32).max(0);
    let x1 = ((cx + radius + 1.0).ceil() as i32).min(img.width as i32);
    let y1 = ((cy + radius + 1.0).ceil() as i32).min(img.height as i32);
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = dx.hypot(dy) - radius;
            let coverage = (0.5 - dist).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let a = (f32::from(color.a) * coverage).round().clamp(0.0, 255.0) as u8;
                img.blend_pixel(x, y, RgbaColor::rgba(color.r, color.g, color.b, a));
            }
        }
    }
}

fn draw_bookmark_glyph(img: &mut RgbaImage, color: RgbaColor) {
    let w = img.width as i32;
    let h = img.height as i32;
    let left = w * 35 / 100;
    let right = w * 65 / 100;
    let top = h * 28 / 100;
    let bottom = h * 74 / 100;
    let notch_top = h * 58 / 100;
    let mid_x = (left + right) / 2;

    for y in top..bottom {
        for x in left..right {
            let in_notch = if y > notch_top {
                let progress = (y - notch_top) as f32 / (bottom - notch_top).max(1) as f32;
                let half_span = ((right - left) as f32 * 0.5) * progress;
                (x - mid_x).abs() as f32 <= half_span
            } else {
                false
            };
            if !in_notch {
                img.blend_pixel(x, y, color);
            }
        }
    }
}

fn draw_initials(img: &mut RgbaImage, text: &str, color: RgbaColor) {
    let chars: Vec<char> = text
        .chars()
        .take(2)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if chars.is_empty() {
        return;
    }
    let scale = ((img.width as i32) / 18).max(1);
    let glyph_w = 5 * scale;
    let glyph_h = 7 * scale;
    let gap = scale * 2;
    let total_w = (chars.len() as i32) * glyph_w + ((chars.len() as i32) - 1) * gap;
    let start_x = (img.width as i32 - total_w) / 2;
    let start_y = (img.height as i32 - glyph_h) / 2;

    for (idx, ch) in chars.iter().enumerate() {
        let rows = glyph_5x7(*ch);
        let gx = start_x + (idx as i32) * (glyph_w + gap);
        for (ry, &bits) in rows.iter().enumerate() {
            for rx in 0..5 {
                if (bits & (1 << (4 - rx))) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            img.blend_pixel(
                                gx + rx * scale + sx,
                                start_y + (ry as i32) * scale + sy,
                                color,
                            );
                        }
                    }
                }
            }
        }
    }
}

fn glyph_5x7(ch: char) -> &'static [u8; 7] {
    match ch {
        'A' => &[
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => &[
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => &[
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => &[
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => &[
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => &[
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => &[
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => &[
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => &[
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => &[
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => &[
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => &[
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => &[
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => &[
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => &[
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => &[
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => &[
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => &[
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => &[
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => &[
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => &[
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => &[
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => &[
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => &[
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => &[
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => &[
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0'..='9' => &[
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        _ => &[
            0b01110, 0b10001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
    }
}
