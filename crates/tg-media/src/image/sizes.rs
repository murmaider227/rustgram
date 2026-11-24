//! Port of photo/document thumbnail and size selection from
//! `Telegram/SourceFiles/data/data_session.cpp`, `data/data_photo.cpp`,
//! `data/data_types.cpp`, and `ui/image/image_location_factory.cpp`.

/// Maximum photo side length enforced by `PhotoData::SideLimit()` (`data_photo.cpp`).
pub const PHOTO_SIDE_LIMIT: u32 = 2560;

/// Standard sticker dimension (`kStickerSideSize` in `data_types.h`).
pub const STICKER_SIDE_SIZE: u32 = 512;

/// Maximum sticker side length (`kLargestStickerSide` in `data_types.cpp`).
pub const LARGEST_STICKER_SIDE: u32 = 2560;

/// Priority order for inline thumbnails (`data_session.cpp:116`).
pub const INLINE_LEVELS: &[u8] = b"i";
/// Priority order for `PhotoSize::Small` (`data_session.cpp:117`).
pub const SMALL_LEVELS: &[u8] = b"sa";
/// Priority order for `PhotoSize::Thumbnail` (`data_session.cpp:118`).
pub const THUMBNAIL_LEVELS: &[u8] = b"mbsa";
/// Priority order for `PhotoSize::Large` (`data_session.cpp:119`).
pub const LARGE_LEVELS: &[u8] = b"ydxcwmbsa";

/// Telegram photo size letter classification (`data_session.cpp:107-116`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhotoSizeType {
    /// `'s'`: box 100x100.
    Box100,
    /// `'m'`: box 320x320.
    Box320,
    /// `'x'`: box 800x800.
    Box800,
    /// `'y'`: box 1280x1280.
    Box1280,
    /// `'w'`: box 2560x2560.
    Box2560,
    /// `'a'`: crop 160x160.
    Crop160,
    /// `'b'`: crop 320x320.
    Crop320,
    /// `'c'`: crop 640x640.
    Crop640,
    /// `'d'`: crop 1280x1280.
    Crop1280,
    /// `'i'`: inline stripped JPEG thumbnail (`photoStrippedSize`).
    Stripped,
    /// `'j'`: inline vector outline path (`photoPathSize`).
    OutlinePath,
    /// Any other Telegram size letter (e.g., `'u'`, `'v'`, `'p'`, `'f'`).
    Other(u8),
}

impl PhotoSizeType {
    /// Parses a Telegram size type byte (`"s"`, `"m"`, `"x"`, `"y"`, `"w"`, `"a"`, `"b"`, `"c"`, `"d"`, `"i"`, `"j"`).
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            b's' => Self::Box100,
            b'm' => Self::Box320,
            b'x' => Self::Box800,
            b'y' => Self::Box1280,
            b'w' => Self::Box2560,
            b'a' => Self::Crop160,
            b'b' => Self::Crop320,
            b'c' => Self::Crop640,
            b'd' => Self::Crop1280,
            b'i' => Self::Stripped,
            b'j' => Self::OutlinePath,
            other => Self::Other(other),
        }
    }

    /// Parses the first byte of a Telegram `type` string.
    #[must_use]
    pub fn from_type_str(s: &str) -> Option<Self> {
        s.as_bytes().first().copied().map(Self::from_byte)
    }

    /// Returns the single-byte Telegram size letter.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Box100 => b's',
            Self::Box320 => b'm',
            Self::Box800 => b'x',
            Self::Box1280 => b'y',
            Self::Box2560 => b'w',
            Self::Crop160 => b'a',
            Self::Crop320 => b'b',
            Self::Crop640 => b'c',
            Self::Crop1280 => b'd',
            Self::Stripped => b'i',
            Self::OutlinePath => b'j',
            Self::Other(b) => b,
        }
    }

    /// Returns the bounding box `(max_w, max_h)` and whether this type is square-cropped, if known.
    #[must_use]
    pub const fn spec(self) -> Option<(u32, u32, bool)> {
        match self {
            Self::Box100 => Some((100, 100, false)),
            Self::Box320 => Some((320, 320, false)),
            Self::Box800 => Some((800, 800, false)),
            Self::Box1280 => Some((1280, 1280, false)),
            Self::Box2560 => Some((2560, 2560, false)),
            Self::Crop160 => Some((160, 160, true)),
            Self::Crop320 => Some((320, 320, true)),
            Self::Crop640 => Some((640, 640, true)),
            Self::Crop1280 => Some((1280, 1280, true)),
            Self::Stripped | Self::OutlinePath | Self::Other(_) => None,
        }
    }
}

/// The three display slots stored in `PhotoData` (`data_photo.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhotoSizeSlot {
    /// Small preview (`SmallLevels = "sa"`).
    Small = 0,
    /// Medium thumbnail (`ThumbnailLevels = "mbsa"`, or progressive scan index 1).
    Thumbnail = 1,
    /// Full display image (`LargeLevels = "ydxcwmbsa"`, or progressive max-area size).
    Large = 2,
}

/// TL-independent representation of Telegram's `PhotoSize` union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhotoSizeEntry {
    /// `photoSizeEmpty`.
    Empty {
        /// Size letter.
        size_type: String,
    },
    /// `photoSize`.
    Normal {
        /// Size letter (e.g., `"s"`, `"m"`, `"x"`, `"y"`, `"w"`).
        size_type: String,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Encoded file size in bytes.
        byte_size: u32,
    },
    /// `photoCachedSize`.
    Cached {
        /// Size letter.
        size_type: String,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Inline image bytes.
        bytes: Vec<u8>,
    },
    /// `photoStrippedSize` (`type = "i"`).
    Stripped {
        /// Size letter (typically `"i"`).
        size_type: String,
        /// Stripped thumbnail bytes (prefix `0x01`, height, width, scan data).
        bytes: Vec<u8>,
    },
    /// `photoSizeProgressive`.
    Progressive {
        /// Size letter.
        size_type: String,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Cumulative byte offsets of progressive JPEG scans.
        sizes: Vec<u32>,
    },
    /// `photoPathSize` (`type = "j"`).
    Path {
        /// Size letter (typically `"j"`).
        size_type: String,
        /// Compressed SVG outline path bytes.
        bytes: Vec<u8>,
    },
}

impl PhotoSizeEntry {
    /// Returns the first byte of `size_type`, or `0` for `Empty` or empty type strings.
    #[must_use]
    pub fn letter(&self) -> u8 {
        match self {
            Self::Empty { .. } => 0,
            Self::Normal { size_type, .. }
            | Self::Cached { size_type, .. }
            | Self::Stripped { size_type, .. }
            | Self::Progressive { size_type, .. }
            | Self::Path { size_type, .. } => size_type.as_bytes().first().copied().unwrap_or(0),
        }
    }

    /// Returns pixel area (`width * height`) for downloadable/cached sizes, or `0` for empty/inline.
    #[must_use]
    pub fn pixel_area(&self) -> u64 {
        match self {
            Self::Normal { width, height, .. }
            | Self::Cached { width, height, .. }
            | Self::Progressive { width, height, .. } => u64::from(*width) * u64::from(*height),
            Self::Empty { .. } | Self::Stripped { .. } | Self::Path { .. } => 0,
        }
    }

    /// Returns the total byte size of this entry (last progressive size for `Progressive`).
    #[must_use]
    pub fn byte_size(&self) -> Option<u32> {
        match self {
            Self::Normal { byte_size, .. } => Some(*byte_size),
            Self::Cached { bytes, .. } => u32::try_from(bytes.len()).ok(),
            Self::Progressive { sizes, .. } => sizes.last().copied(),
            Self::Empty { .. } | Self::Stripped { .. } | Self::Path { .. } => None,
        }
    }
}

/// Inline thumbnail found in a photo or document's size list (`InlineImageLocation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineThumbnail<'a> {
    /// `photoStrippedSize` payload (decode via [`crate::image::inline::from_inline_bytes`]).
    Stripped(&'a [u8]),
    /// `photoPathSize` payload (decode via [`crate::image::inline::path_from_inline_bytes`]).
    Path(&'a [u8]),
}

/// Selected sizes for a Telegram photo, mirroring `Session::photoApplyFields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPhotoSizes<'a> {
    /// Inline stripped thumbnail bytes (`FindPhotoInlineThumbnail` ignores vector paths).
    pub inline_stripped: Option<&'a [u8]>,
    /// Selected entry for `PhotoSizeSlot::Small` (`None` when using progressive).
    pub small: Option<&'a PhotoSizeEntry>,
    /// Selected entry for `PhotoSizeSlot::Thumbnail` (`None` when using progressive).
    pub thumbnail: Option<&'a PhotoSizeEntry>,
    /// Progressive byte prefix size for `PhotoSizeSlot::Thumbnail` (index 1 of `photoSizeProgressive.sizes`).
    pub progressive_thumbnail_bytes: Option<u32>,
    /// Selected entry for `PhotoSizeSlot::Large`.
    pub large: Option<&'a PhotoSizeEntry>,
}

impl<'a> SelectedPhotoSizes<'a> {
    /// Resolves the effective downloadable slot for a requested [`PhotoSizeSlot`],
    /// falling back upward `Small -> Thumbnail -> Large` as in `PhotoData::validSizeIndex`.
    #[must_use]
    pub fn resolve_slot(&self, slot: PhotoSizeSlot) -> Option<&'a PhotoSizeEntry> {
        match slot {
            PhotoSizeSlot::Small => self.small.or(self.thumbnail).or(self.large),
            PhotoSizeSlot::Thumbnail => self.thumbnail.or(self.large),
            PhotoSizeSlot::Large => self.large,
        }
    }

    /// Resolves the byte size to request for `slot`, accounting for `progressive_thumbnail_bytes`
    /// as in `PhotoData::imageByteSize`.
    #[must_use]
    pub fn byte_size_for_slot(&self, slot: PhotoSizeSlot) -> Option<u32> {
        match slot {
            PhotoSizeSlot::Small => {
                if let Some(entry) = self.small {
                    return entry.byte_size();
                }
                if let Some(prog) = self.progressive_thumbnail_bytes {
                    return Some(prog);
                }
                self.thumbnail
                    .and_then(PhotoSizeEntry::byte_size)
                    .or_else(|| self.large.and_then(PhotoSizeEntry::byte_size))
            }
            PhotoSizeSlot::Thumbnail => {
                if let Some(prog) = self.progressive_thumbnail_bytes {
                    return Some(prog);
                }
                self.thumbnail
                    .and_then(PhotoSizeEntry::byte_size)
                    .or_else(|| self.large.and_then(PhotoSizeEntry::byte_size))
            }
            PhotoSizeSlot::Large => self.large.and_then(PhotoSizeEntry::byte_size),
        }
    }
}

/// Finds an inline thumbnail in a list of photo sizes (`FindInlineThumbnail` in `data_session.cpp`).
///
/// Prefers `photoStrippedSize` over `photoPathSize`.
#[must_use]
pub fn find_inline_thumbnail(sizes: &[PhotoSizeEntry]) -> Option<InlineThumbnail<'_>> {
    if let Some(bytes) = sizes.iter().find_map(|s| match s {
        PhotoSizeEntry::Stripped { bytes, .. } => Some(bytes.as_slice()),
        _ => None,
    }) {
        return Some(InlineThumbnail::Stripped(bytes));
    }
    sizes.iter().find_map(|s| match s {
        PhotoSizeEntry::Path { bytes, .. } => Some(InlineThumbnail::Path(bytes.as_slice())),
        _ => None,
    })
}

/// Selects the highest-priority photo size matching `levels` (earlier byte in `levels` wins).
///
/// Exact port of the `find(levels)` lambda in `Session::photoApplyFields` (`data_session.cpp:3932`).
#[must_use]
pub fn find_photo_size_by_levels<'a>(
    sizes: &'a [PhotoSizeEntry],
    levels: &[u8],
) -> Option<&'a PhotoSizeEntry> {
    let invalid = levels.len();
    let mut best: Option<(usize, &'a PhotoSizeEntry)> = None;
    for entry in sizes {
        let letter = entry.letter();
        if letter == 0 {
            continue;
        }
        // Skip entries that cannot produce a valid location (Stripped / Path / Empty / empty Progressive).
        match entry {
            PhotoSizeEntry::Normal { .. } | PhotoSizeEntry::Cached { .. } => {}
            PhotoSizeEntry::Progressive { sizes, .. } if !sizes.is_empty() => {}
            _ => continue,
        }
        let rank = levels.iter().position(|&b| b == letter).unwrap_or(invalid);
        if rank == invalid {
            continue;
        }
        // Note: tdesktop uses `ranges::max_element(..., std::greater<>(), level)` which keeps
        // the first element with the smallest index in `levels`.
        if best.is_none_or(|(best_rank, _)| rank < best_rank) {
            best = Some((rank, entry));
        }
    }
    best.map(|(_, entry)| entry)
}

/// Selects `Small`, `Thumbnail`, `Large`, progressive thumbnail prefix, and inline stripped bytes
/// from a photo's `sizes` vector.
///
/// Exact port of `Session::photoApplyFields` (`data_session.cpp:3914-4025`).
#[must_use]
pub fn choose_photo_sizes(sizes: &[PhotoSizeEntry]) -> SelectedPhotoSizes<'_> {
    // Find progressive entry with largest pixel area.
    let progressive = sizes
        .iter()
        .filter(|s| matches!(s, PhotoSizeEntry::Progressive { sizes, .. } if !sizes.is_empty()))
        .max_by_key(|s| s.pixel_area());

    let inline_stripped = match find_inline_thumbnail(sizes) {
        Some(InlineThumbnail::Stripped(b)) => Some(b),
        _ => None,
    };

    if let Some(prog) = progressive {
        let progressive_thumbnail_bytes = match prog {
            PhotoSizeEntry::Progressive { sizes, .. } => sizes.get(1).copied(),
            _ => None,
        };
        SelectedPhotoSizes {
            inline_stripped,
            small: None,
            thumbnail: None,
            progressive_thumbnail_bytes,
            large: Some(prog),
        }
    } else {
        SelectedPhotoSizes {
            inline_stripped,
            small: find_photo_size_by_levels(sizes, SMALL_LEVELS),
            thumbnail: find_photo_size_by_levels(sizes, THUMBNAIL_LEVELS),
            progressive_thumbnail_bytes: None,
            large: find_photo_size_by_levels(sizes, LARGE_LEVELS),
        }
    }
}

/// Chooses the best downloadable thumbnail for a document (`FindDocumentThumbnail` in `data_session.cpp:171`).
///
/// Picks the entry with the maximum pixel area (`width * height > 0`), ignoring `Empty`, `Stripped`, and `Path`.
#[must_use]
pub fn choose_document_thumbnail(thumbs: &[PhotoSizeEntry]) -> Option<&PhotoSizeEntry> {
    thumbs
        .iter()
        .filter(|s| s.pixel_area() > 0)
        .max_by_key(|s| s.pixel_area())
}

/// TL-independent representation of a Telegram `VideoSize` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoSizeEntry {
    /// Size type letter (e.g., `"u"`, `"v"`, `"p"`).
    pub size_type: String,
    /// Video width in pixels.
    pub width: u32,
    /// Video height in pixels.
    pub height: u32,
    /// Encoded video byte size.
    pub byte_size: u32,
    /// Optional start timestamp in seconds for animated avatars (`video_start_ts`).
    pub video_start_ts: Option<f64>,
}

impl VideoSizeEntry {
    /// Pixel area when `byte_size > 0`, otherwise `0`.
    #[must_use]
    pub fn valid_area(&self) -> u64 {
        if self.byte_size > 0 {
            u64::from(self.width) * u64::from(self.height)
        } else {
            0
        }
    }

    /// Converts `video_start_ts` (seconds) to clamped milliseconds (`VideoStartTime` in `data_session.cpp:222`).
    #[must_use]
    pub fn start_time_ms(&self) -> u32 {
        let secs = self.video_start_ts.unwrap_or(0.0);
        if !secs.is_finite() || secs <= 0.0 {
            return 0;
        }
        let ms = (secs * 1000.0).floor();
        if ms >= f64::from(i32::MAX) {
            i32::MAX as u32
        } else {
            ms as u32
        }
    }
}

/// Chooses small (`'p'`) and large (maximum area) video sizes for a photo (`data_session.cpp:3957`).
#[must_use]
pub fn choose_photo_video_sizes(
    video_sizes: &[VideoSizeEntry],
) -> (Option<&VideoSizeEntry>, Option<&VideoSizeEntry>) {
    let small = video_sizes
        .iter()
        .find(|v| v.size_type.as_bytes().first() == Some(&b'p') && v.valid_area() > 0);
    let large = video_sizes
        .iter()
        .filter(|v| v.valid_area() > 0)
        .max_by_key(|v| v.valid_area());
    (small, large)
}

/// Chooses the largest document video thumbnail (`FindDocumentVideoThumbnail` in `data_session.cpp:195`).
#[must_use]
pub fn choose_document_video_thumbnail(video_thumbs: &[VideoSizeEntry]) -> Option<&VideoSizeEntry> {
    video_thumbs
        .iter()
        .filter(|v| u64::from(v.width) * u64::from(v.height) > 0)
        .max_by_key(|v| u64::from(v.width) * u64::from(v.height))
}

/// Clamps photo dimensions to [`PHOTO_SIDE_LIMIT`] while preserving aspect ratio,
/// matching `PhotoData::size` (`data_photo.cpp:172-183`).
#[must_use]
pub fn clamp_photo_dimensions(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    if width <= PHOTO_SIDE_LIMIT && height <= PHOTO_SIDE_LIMIT {
        return Some((width, height));
    }
    let (w, h) =
        super::prepare::shrink_to_keep_aspect(width, height, PHOTO_SIDE_LIMIT, PHOTO_SIDE_LIMIT);
    Some((w.max(1), h.max(1)))
}

/// Validates sticker dimensions (`GoodStickerDimensions` in `data_types.cpp:150`).
#[must_use]
pub const fn good_sticker_dimensions(width: u32, height: u32) -> bool {
    let max_area = (LARGEST_STICKER_SIDE as u64) * (LARGEST_STICKER_SIDE as u64);
    width > 0 && height > 0 && (width as u64) * (height as u64) <= max_area
}
