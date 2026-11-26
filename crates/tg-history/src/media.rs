//! Media element sizing and layout calculations.
//!
//! Port of:
//! - `Telegram/SourceFiles/history/view/media/history_view_media_common.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_photo.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_document.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_sticker.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_gif.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_web_page.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_poll.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_contact.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_location.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_call.cpp`
//! - `Telegram/SourceFiles/media/streaming/media_streaming_utility.cpp` (`FrameResizeMayExpand`)

use crate::consts::{
    CONTACTS_PHOTO_SIZE, FontMetrics, HISTORY_CALL_HEIGHT, HISTORY_CALL_ICON_POSITION,
    HISTORY_CALL_ICON_SKIP, HISTORY_CALL_LEFT, HISTORY_CALL_STATUS_SKIP, HISTORY_CALL_WIDTH,
    HISTORY_PAGE_BUTTON_HEIGHT, HISTORY_PAGE_BUTTON_PADDING, HISTORY_PAGE_PREVIEW_PADDING,
    HISTORY_PHOTO_BUBBLE_MIN_WIDTH, HISTORY_POLL_ANSWER_PADDING,
    HISTORY_POLL_ANSWER_PADDING_NO_MEDIA, HISTORY_POLL_ANSWERS_SKIP,
    HISTORY_POLL_BOTTOM_BUTTON_SKIP, HISTORY_POLL_QUESTION_TOP, HISTORY_POLL_SUBTITLE_SKIP,
    HISTORY_POLL_TOTAL_VOTES_SKIP, HISTORY_TRANSCRIBE_SIZE, HISTORY_TRANSCRIBE_SKIP, LOCATION_SIZE,
    MAX_ANIMATED_EMOJI_SIZE, MAX_GIF_SIZE, MAX_MEDIA_SIZE, MAX_STICKER_SIZE,
    MAX_VIDEO_MESSAGE_SIZE, MEDIA_IN_BUBBLE_SKIP, MEDIA_UNREAD_SIZE, MEDIA_UNREAD_SKIP,
    MIN_PHOTO_SIZE, MSG_DATE_DELTA, MSG_DATE_IMG_DELTA, MSG_DATE_IMG_PADDING, MSG_FILE_LAYOUT,
    MSG_FILE_MIN_WIDTH, MSG_FILE_THUMB_LAYOUT, MSG_FILE_TOP_MINUS, MSG_MAX_WIDTH, MSG_PADDING,
    MSG_WAVEFORM_BAR, MSG_WAVEFORM_SKIP, STICKER_MAX_EMOJI_SIZE_FIXED, STICKER_MAX_SIZE_FIXED,
    WAVEFORM_SAMPLES_COUNT, WEB_PAGE_PHOTO_DELTA,
};
use crate::geometry::{Margins, Size, downscaled_size, non_empty_size, safe_round};
use crate::grouped::{GroupedMediaInput, GroupedMediaLayout};

// ---------------------------------------------------------------------------
// Common media sizing helpers (history_view_media_common.cpp)
// ---------------------------------------------------------------------------

/// Computes the desired display size of media from its natural dimensions
/// (`HistoryView::CountDesiredMediaSize` in `history_view_media_common.cpp:320-324`).
#[must_use]
pub fn count_desired_media_size(original: Size) -> Size {
    downscaled_size(original, Size::new(MAX_MEDIA_SIZE, MAX_MEDIA_SIZE))
}

/// Scales `desired` down to fit within `new_width` while keeping aspect ratio
/// (`HistoryView::CountMediaSize` in `history_view_media_common.cpp:326-333`).
#[must_use]
pub fn count_media_size(desired: Size, new_width: i32) -> Size {
    if desired.is_empty() || new_width <= 0 {
        return Size::new(1, 1);
    }
    if desired.width <= new_width {
        desired
    } else {
        non_empty_size(desired.scaled_keep_aspect_ratio(Size::new(new_width, desired.height)))
    }
}

/// Fits a photo's `desired` size into `min(new_width, max_width)` and additionally clamps
/// height to `new_width` (`HistoryView::CountPhotoMediaSize` in `history_view_media_common.cpp:335-344`).
#[must_use]
pub fn count_photo_media_size(desired: Size, new_width: i32, max_width: i32) -> Size {
    let media = count_media_size(desired, new_width.min(max_width));
    if media.height <= new_width {
        media
    } else {
        non_empty_size(media.scaled_keep_aspect_ratio(Size::new(media.width, new_width)))
    }
}

/// Scales `original` to cover `bounds` (`QSize::scaled(bounds, Qt::KeepAspectRatioByExpanding)`).
#[must_use]
fn scaled_keep_aspect_ratio_by_expanding(original: Size, bounds: Size) -> Size {
    if original.is_empty() || bounds.is_empty() {
        return Size::ZERO;
    }
    let rw = (i64::from(bounds.height) * i64::from(original.width)) / i64::from(original.height);
    if rw >= i64::from(bounds.width) {
        Size::new(rw as i32, bounds.height)
    } else {
        let rh = (i64::from(bounds.width) * i64::from(original.height)) / i64::from(original.width);
        Size::new(bounds.width, rh as i32)
    }
}

/// How a picture of `original` size is fitted into an `outer` box
/// (`Media::Streaming::ExpandDecision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpandDecision {
    /// Size the picture is scaled to, keeping its aspect ratio.
    pub result: Size,
    /// `true`: `result` covers `outer` and is cropped to it (centered). `false`: `result` fits
    /// inside `outer`, centered over the blurred background.
    pub expanding: bool,
}

/// Port of `Media::Streaming::DecideFrameResize` (`media_streaming_utility.cpp:371-387`).
///
/// The picture covers the box when no more than `1 - nominator / denominator` of its height
/// is cut off (tdesktop's defaults are `(3, 4)`); otherwise it fits inside the box.
#[must_use]
pub fn decide_frame_resize(
    outer: Size,
    original: Size,
    min_visible_nominator: i32,
    min_visible_denominator: i32,
) -> ExpandDecision {
    decide_frame_resize_i64(
        outer,
        original,
        i64::from(min_visible_nominator),
        i64::from(min_visible_denominator),
    )
}

fn decide_frame_resize_i64(
    outer: Size,
    original: Size,
    min_visible_nominator: i64,
    min_visible_denominator: i64,
) -> ExpandDecision {
    if outer.is_empty() {
        return ExpandDecision {
            result: original,
            expanding: true,
        };
    }
    let big = scaled_keep_aspect_ratio_by_expanding(original, outer);
    if big.width <= outer.width
        && i64::from(big.height) * min_visible_nominator
            <= i64::from(outer.height) * min_visible_denominator
    {
        return ExpandDecision {
            result: big,
            expanding: true,
        };
    }
    ExpandDecision {
        result: original.scaled_keep_aspect_ratio(outer),
        expanding: false,
    }
}

/// Port of `Media::Streaming::FrameResizeMayExpand` (`media_streaming_utility.cpp:389-411`).
///
/// Default nominator/denominator in tdesktop are `(3, 4)`.
#[must_use]
pub fn frame_resize_may_expand(
    outer: Size,
    original: Size,
    min_visible_nominator: i32,
    min_visible_denominator: i32,
) -> bool {
    let min_dim = outer
        .width
        .min(outer.height)
        .min(original.width)
        .min(original.height);
    if min_dim <= 0 {
        return false;
    }
    if 2 * i64::from(min_visible_nominator) * i64::from(min_dim)
        < 2 * i64::from(min_visible_denominator)
            + i64::from(min_visible_denominator) * i64::from(min_dim)
    {
        return false;
    }
    let num =
        i64::from(min_visible_nominator) * i64::from(min_dim) - i64::from(min_visible_denominator);
    let den = i64::from(min_visible_denominator) * i64::from(min_dim);
    decide_frame_resize_i64(outer, original, num, den).expanding
}

// ---------------------------------------------------------------------------
// Photo (history_view_photo.cpp)
// ---------------------------------------------------------------------------

/// Input parameters for sizing a single photo (`HistoryView::Photo`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhotoInput {
    /// Original photo dimensions in pixels.
    pub dimensions: Size,
    /// Non-zero for circular service chat-photo updates (`msgServicePhotoWidth = 100`).
    pub service_width: i32,
    /// True if rendered inside a message bubble (e.g. has caption, reply, forward, sender).
    pub has_bubble: bool,
    /// Minimum width required by bottom info / comments bar (`Element::minWidthForMedia`).
    pub min_width_for_media: i32,
    /// Textual width of caption (`Element::textualMaxWidth`), `0` if no caption.
    pub textual_max_width: i32,
    /// True for extended-media video previews (uses `CountMediaSize` instead of `CountPhotoMediaSize`).
    pub is_extended_video: bool,
    /// True if hosted inside an Instant View article.
    pub hosted_instant_view: bool,
}

/// Port of `Photo::adjustHeightForLessCrop` (`history_view_photo.cpp:425-433`).
#[must_use]
pub fn photo_adjust_height_for_less_crop(dimensions: Size, current: Size) -> i32 {
    if dimensions.is_empty() || !frame_resize_may_expand(current, dimensions, 3, 4) {
        return current.height;
    }
    current.height.max(
        ((i64::from(current.width) * i64::from(dimensions.height)) / i64::from(dimensions.width))
            as i32,
    )
}

/// Computes a photo's optimal size (`Photo::countOptimalSize`, `history_view_photo.cpp:329-368`).
#[must_use]
pub fn count_photo_optimal_size(input: &PhotoInput) -> Size {
    if input.service_width > 0 {
        return Size::new(input.service_width, input.service_width);
    }
    let dims = non_empty_size(input.dimensions);
    let scaled = if input.hosted_instant_view {
        dims
    } else {
        count_desired_media_size(dims)
    };
    let max_media_width = if input.hosted_instant_view {
        scaled.width.max(MAX_MEDIA_SIZE)
    } else {
        MAX_MEDIA_SIZE
    };
    let lower = if input.has_bubble {
        HISTORY_PHOTO_BUBBLE_MIN_WIDTH
    } else {
        MIN_PHOTO_SIZE
    };
    let min_width = input.min_width_for_media.clamp(lower, max_media_width);
    let max_actual_width = scaled.width.max(min_width);
    let mut max_width = max_actual_width.max(scaled.height);
    let mut min_height = scaled.height.max(MIN_PHOTO_SIZE);
    if input.has_bubble {
        let max_with_caption = MSG_MAX_WIDTH.min(input.textual_max_width);
        max_width = max_width.max(max_with_caption).min(MSG_MAX_WIDTH);
        min_height = photo_adjust_height_for_less_crop(dims, Size::new(max_width, min_height));
    }
    Size::new(max_width, min_height)
}

/// Computes a photo's current size for `new_width` (`Photo::countCurrentSize`,
/// `history_view_photo.cpp:370-423`).
#[must_use]
pub fn count_photo_current_size(input: &PhotoInput, optimal: Size, new_width: i32) -> Size {
    if input.service_width > 0 {
        return Size::new(input.service_width, input.service_width);
    }
    let thumb_max_width = if input.hosted_instant_view {
        new_width.max(1)
    } else {
        new_width.min(MAX_MEDIA_SIZE)
    };
    let lower_base = if input.has_bubble {
        HISTORY_PHOTO_BUBBLE_MIN_WIDTH
    } else {
        MIN_PHOTO_SIZE
    };
    let min_width = input
        .min_width_for_media
        .clamp(thumb_max_width.min(lower_base), thumb_max_width);
    let dims = non_empty_size(input.dimensions);
    let desired = if input.hosted_instant_view {
        dims
    } else {
        count_desired_media_size(dims)
    };
    let pix = if input.is_extended_video {
        count_media_size(desired, new_width)
    } else {
        count_photo_media_size(
            desired,
            new_width,
            if input.hosted_instant_view {
                new_width
            } else {
                optimal.width
            },
        )
    };
    let mut cur_width = pix.width.max(min_width);
    let mut cur_height = pix.height.max(MIN_PHOTO_SIZE);
    if input.has_bubble {
        let max_with_caption = MSG_MAX_WIDTH.min(input.textual_max_width);
        cur_width = cur_width.max(max_with_caption).min(thumb_max_width);
        cur_height = photo_adjust_height_for_less_crop(dims, Size::new(cur_width, cur_height));
    }
    if cur_width >= optimal.width {
        cur_height = cur_height.min(optimal.height);
    }
    Size::new(cur_width, cur_height)
}

// ---------------------------------------------------------------------------
// Document / File / Voice / Audio (history_view_document.cpp)
// ---------------------------------------------------------------------------

/// Variant of a document message (`HistoryView::Document`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// Generic file without thumbnail (`msgFileLayout`).
    File,
    /// Document with square thumbnail preview (`msgFileThumbLayout`).
    Thumbed,
    /// Voice message with waveform (`msgFileLayout` + waveform).
    Voice,
    /// Audio/song file (`msgFileLayout`).
    Audio,
}

/// Input parameters for sizing a document / file / voice message (`HistoryView::Document`).
#[derive(Debug, Clone, Copy)]
pub struct DocumentInput {
    /// Document presentation kind.
    pub kind: DocumentKind,
    /// Whether this document is at the top of its bubble.
    pub is_bubble_top: bool,
    /// Whether this document is at the bottom of its bubble.
    pub is_bubble_bottom: bool,
    /// Measured filename / song title width (`0` for voice messages).
    pub name_width: i32,
    /// Maximum measured width of the status string (size / duration).
    pub max_status_width: i32,
    /// Current measured width of the status string.
    pub status_width: i32,
    /// Width of the thumbed file link text ("OPEN WITH" / "SAVE AS"), `0` if none.
    pub thumbed_link_width: i32,
    /// True if an unread dot is drawn next to the voice/round status.
    pub is_unread_media: bool,
    /// True if a voice-to-text transcribe button is shown.
    pub has_transcribe_button: bool,
    /// Height of expanded voice transcription text (`0` if collapsed/absent).
    pub transcribe_text_height: i32,
    /// Skip block width reserved for the bottom info timestamp.
    pub skip_block_width: i32,
    /// First-line width of the message's bottom info (`Element::bottomInfoFirstLineWidth`).
    pub bottom_info_first_line_width: i32,
}

impl Default for DocumentInput {
    fn default() -> Self {
        Self {
            kind: DocumentKind::File,
            is_bubble_top: true,
            is_bubble_bottom: true,
            name_width: 0,
            max_status_width: 0,
            status_width: 0,
            thumbed_link_width: 0,
            is_unread_media: false,
            has_transcribe_button: false,
            transcribe_text_height: 0,
            skip_block_width: 0,
            bottom_info_first_line_width: 0,
        }
    }
}

/// Computes the number of waveform bars fitting in `available_width`
/// (`PaintWaveform` in `history_view_document.cpp:270-272`).
#[must_use]
pub fn waveform_bar_count(available_width: i32, samples_count: i32) -> i32 {
    if available_width <= 0 || samples_count <= 0 {
        return 0;
    }
    let step = MSG_WAVEFORM_BAR + MSG_WAVEFORM_SKIP;
    (available_width / step).min(samples_count)
}

/// Computes a document's optimal size (`Document::countOptimalSize`,
/// `history_view_document.cpp:494-647`).
#[must_use]
pub fn count_document_optimal_size(input: &DocumentInput) -> Size {
    let thumbed = matches!(input.kind, DocumentKind::Thumbed);
    let voice = matches!(input.kind, DocumentKind::Voice);
    let has_transcribe = voice && input.transcribe_text_height > 0;
    let st = if thumbed {
        MSG_FILE_THUMB_LAYOUT
    } else {
        MSG_FILE_LAYOUT
    };

    let mut max_width = MSG_FILE_MIN_WIDTH;
    let tleft = st.padding.left + st.thumb_size + st.thumb_skip;
    let tright = st.padding.right;

    if thumbed {
        max_width = max_width.max(tleft + input.max_status_width + tright);
    } else {
        let unread = if voice {
            MEDIA_UNREAD_SKIP + MEDIA_UNREAD_SIZE
        } else {
            0
        };
        max_width = max_width.max(
            tleft + input.max_status_width + unread + input.skip_block_width + MSG_PADDING.right,
        );
    }

    if !voice && input.name_width > 0 {
        max_width = max_width.max(tleft + input.name_width + tright);
        max_width = max_width.min(MSG_MAX_WIDTH);
    }

    if voice {
        let max_waveform_width = WAVEFORM_SAMPLES_COUNT * (MSG_WAVEFORM_BAR + MSG_WAVEFORM_SKIP);
        let transcribe_width = if input.has_transcribe_button {
            HISTORY_TRANSCRIBE_SIZE.width + HISTORY_TRANSCRIBE_SKIP
        } else {
            0
        };
        max_width = max_width.max(
            max_waveform_width
                + st.padding.horizontal()
                + st.thumb_size
                + st.thumb_skip
                + transcribe_width,
        );
    }

    let mut min_height = st.padding.top + st.thumb_size + st.padding.bottom;
    if input.is_bubble_bottom && !has_transcribe && input.thumbed_link_width > 0 {
        max_width = max_width.max(
            tleft
                + input.thumbed_link_width
                + st.thumb_skip
                + input.bottom_info_first_line_width
                + tright,
        );
    }
    if !input.is_bubble_top {
        min_height -= MSG_FILE_TOP_MINUS;
    }

    if has_transcribe {
        min_height += input.transcribe_text_height;
        if input.is_bubble_bottom {
            min_height += MSG_PADDING.bottom;
        }
    }

    Size::new(max_width, min_height)
}

/// Computes a document's current size at `new_width` (`Document::countCurrentSize`,
/// `history_view_document.cpp:649-688`).
#[must_use]
pub fn count_document_current_size(
    input: &DocumentInput,
    optimal: Size,
    new_width: i32,
    fonts: &FontMetrics,
) -> Size {
    let thumbed = matches!(input.kind, DocumentKind::Thumbed);
    let voice = matches!(input.kind, DocumentKind::Voice);
    let has_transcribe = voice && input.transcribe_text_height > 0;
    let st = if thumbed {
        MSG_FILE_THUMB_LAYOUT
    } else {
        MSG_FILE_LAYOUT
    };

    let width = new_width.min(optimal.width);
    let mut height = optimal.height;

    if !has_transcribe && input.is_bubble_bottom {
        let thumbed_w = if thumbed { input.thumbed_link_width } else { 0 };
        let status_w = if thumbed_w > 0 { 0 } else { input.status_width };
        if thumbed_w > 0 || status_w > 0 {
            let needed = st.padding.left
                + if thumbed_w > 0 {
                    st.thumb_size + st.thumb_skip
                } else {
                    MSG_FILE_LAYOUT.thumb_size + MEDIA_UNREAD_SKIP
                }
                + (thumbed_w + status_w)
                + st.thumb_skip
                + if input.is_unread_media {
                    MEDIA_UNREAD_SKIP + MEDIA_UNREAD_SIZE
                } else {
                    0
                }
                + input.bottom_info_first_line_width
                + st.padding.right;
            if width < needed {
                height += fonts.date.height - MSG_DATE_DELTA.y;
            }
        }
    }

    Size::new(width, height)
}

// ---------------------------------------------------------------------------
// Sticker (history_view_sticker.cpp)
// ---------------------------------------------------------------------------

/// Kind of sticker / animated emoji (`HistoryView::Sticker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickerKind {
    /// Standard or video/Lottie sticker (`maxStickerSize = 224px`).
    Regular,
    /// Large animated emoji (`maxAnimatedEmojiSize = 112px`).
    AnimatedEmoji,
    /// Animated dice / slot machine (`EmojiSize = 112px`).
    Dice,
}

/// Input parameters for sizing a sticker (`HistoryView::Sticker`).
#[derive(Debug, Clone, Copy)]
pub struct StickerInput {
    /// Sticker presentation kind.
    pub kind: StickerKind,
    /// Original sticker dimensions (often 512x512).
    pub dimensions: Size,
    /// Optional explicit custom size override (e.g. sticker inside a gift box).
    pub custom_size: i32,
    /// Optional user setting override (`sticker-size` option, clamped to `50..=max`).
    pub option_sticker_size: Option<i32>,
}

/// Maximum bounding box for regular stickers (`Sticker::Size()`, `history_view_sticker.cpp:200-210`).
#[must_use]
pub fn sticker_max_size(option_sticker_size: Option<i32>) -> Size {
    let side = MAX_STICKER_SIZE.min(STICKER_MAX_SIZE_FIXED);
    if let Some(opt) = option_sticker_size
        && opt > 0
    {
        let clamped = opt.clamp(50, side);
        return Size::new(clamped, clamped);
    }
    Size::new(side, side)
}

/// Bounding box for animated emoji / dice (`Sticker::EmojiSize()`, `history_view_sticker.cpp:228-231`).
#[must_use]
pub const fn sticker_emoji_size() -> Size {
    let side = if MAX_ANIMATED_EMOJI_SIZE < STICKER_MAX_EMOJI_SIZE_FIXED {
        MAX_ANIMATED_EMOJI_SIZE
    } else {
        STICKER_MAX_EMOJI_SIZE_FIXED
    };
    Size::new(side, side)
}

/// Computes the display size for a sticker (`Sticker::initSize`, `history_view_sticker.cpp:155-171`).
#[must_use]
pub fn count_sticker_optimal_size(input: &StickerInput) -> Size {
    let max_box = sticker_max_size(input.option_sticker_size);
    let size = if input.custom_size > 0 {
        let original = if input.dimensions.is_empty() {
            Size::ZERO
        } else {
            downscaled_size(input.dimensions, max_box)
        };
        let proposed = Size::new(input.custom_size, input.custom_size);
        if original.is_empty() {
            proposed
        } else {
            downscaled_size(original, proposed)
        }
    } else if matches!(input.kind, StickerKind::AnimatedEmoji | StickerKind::Dice) {
        sticker_emoji_size()
    } else {
        downscaled_size(input.dimensions, max_box)
    };
    downscaled_size(size, max_box)
}

/// Computes the premium sticker effect overlay size (`Sticker::PremiumEffectSize`,
/// `history_view_sticker.cpp:46, 216-218`).
#[must_use]
pub fn sticker_premium_effect_size(dimensions: Size) -> Size {
    const PREMIUM_MULTIPLIER: f64 = 1.0 + 0.245 * 2.0;
    let base = downscaled_size(dimensions, sticker_max_size(None));
    Size::new(
        safe_round(f64::from(base.width) * PREMIUM_MULTIPLIER),
        safe_round(f64::from(base.height) * PREMIUM_MULTIPLIER),
    )
}

// ---------------------------------------------------------------------------
// GIF / Video file / Round video message (history_view_gif.cpp)
// ---------------------------------------------------------------------------

/// Variant of an animated video/GIF media item (`HistoryView::Gif`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GifKind {
    /// Silent looping GIF animation (`maxGifSize = 320px`).
    Gif,
    /// Full video file (`maxMediaSize = 430px`).
    VideoFile,
    /// Circular round video note (`maxVideoMessageSize = 240px`).
    VideoMessage,
}

/// Input parameters for sizing a GIF, video file, or round video message (`HistoryView::Gif`).
#[derive(Debug, Clone, Copy)]
pub struct GifInput {
    /// Media variant (GIF, video file, or round video note).
    pub kind: GifKind,
    /// Natural video dimensions.
    pub dimensions: Size,
    /// True if rendered inside a message bubble.
    pub has_bubble: bool,
    /// Minimum width required by bottom info / comments bar.
    pub min_width_for_media: i32,
    /// Width of the bottom info block (`Element::infoWidth`).
    pub info_width: i32,
    /// True if bottom info is hidden.
    pub hides_bottom_info: bool,
    /// Textual width of caption (`Element::textualMaxWidth`), `0` if none.
    pub textual_max_width: i32,
    /// Maximum status text width (e.g. `"GIF • 1.2 MB"`) when not yet streaming.
    pub max_status_width: i32,
    /// True if actively streaming inline (status overlay is hidden).
    pub is_streaming: bool,
    /// Additional side width for unwrapped round video reply/via/forwarded chip.
    pub unwrapped_additional_width: i32,
    /// Optimal reactions bar width when unwrapped.
    pub reactions_optimal_width: i32,
    /// True if hosted inside an Instant View article.
    pub hosted_instant_view: bool,
}

impl Default for GifInput {
    fn default() -> Self {
        Self {
            kind: GifKind::Gif,
            dimensions: Size::new(320, 240),
            has_bubble: false,
            min_width_for_media: 0,
            info_width: 0,
            hides_bottom_info: false,
            textual_max_width: 0,
            max_status_width: 0,
            is_streaming: false,
            unwrapped_additional_width: 0,
            reactions_optimal_width: 0,
            hosted_instant_view: false,
        }
    }
}

/// Computes the thumbnail box for a GIF / video / round video (`Gif::countThumbSize`,
/// `history_view_gif.cpp:357-376`).
#[must_use]
pub fn count_gif_thumb_size(
    kind: GifKind,
    dimensions: Size,
    width_max: &mut i32,
    hosted_instant_view: bool,
) -> Size {
    let max_size = if hosted_instant_view {
        (*width_max).max(1)
    } else {
        match kind {
            GifKind::VideoFile => MAX_MEDIA_SIZE,
            GifKind::VideoMessage => MAX_VIDEO_MESSAGE_SIZE,
            GifKind::Gif => MAX_GIF_SIZE,
        }
    };
    if hosted_instant_view {
        *width_max = (*width_max).max(1);
    } else {
        *width_max = (*width_max).min(max_size);
    }
    let size = non_empty_size(dimensions);
    downscaled_size(size, Size::new(*width_max, max_size))
}

/// Port of `Gif::adjustHeightForLessCrop` (`history_view_gif.cpp:531-542`).
#[must_use]
pub fn gif_adjust_height_for_less_crop(dimensions: Size, current: Size) -> i32 {
    if dimensions.is_empty() {
        return current.height;
    }
    let proportional = ((i64::from(current.width) * i64::from(dimensions.height))
        / i64::from(dimensions.width)) as i32;
    let max_expanded = (current.height * 4) / 3;
    current.height.max(proportional.min(max_expanded))
}

/// Computes optimal size for a GIF / video / round video (`Gif::countOptimalSize`,
/// `history_view_gif.cpp:378-430`).
#[must_use]
pub fn count_gif_optimal_size(input: &GifInput) -> Size {
    let max_media_width = if input.hosted_instant_view {
        MSG_MAX_WIDTH.max(MAX_MEDIA_SIZE)
    } else {
        MAX_MEDIA_SIZE
    };
    let lower = if input.has_bubble {
        HISTORY_PHOTO_BUBBLE_MIN_WIDTH
    } else {
        MIN_PHOTO_SIZE
    };
    let min_width = input.min_width_for_media.clamp(lower, max_media_width);
    let mut thumb_max_width = MSG_MAX_WIDTH;
    let scaled = count_gif_thumb_size(
        input.kind,
        input.dimensions,
        &mut thumb_max_width,
        input.hosted_instant_view,
    );
    let mut max_width = scaled.width.max(min_width).min(thumb_max_width);
    let mut min_height = scaled.height.max(MIN_PHOTO_SIZE);
    if !input.is_streaming && input.max_status_width > 0 {
        let status_w = input.max_status_width + 2 * (MSG_DATE_IMG_DELTA + MSG_DATE_IMG_PADDING.x);
        max_width = max_width.max(status_w);
    }
    if input.has_bubble {
        max_width = max_width.max(input.textual_max_width);
        min_height = gif_adjust_height_for_less_crop(scaled, Size::new(max_width, min_height));
    } else if matches!(input.kind, GifKind::VideoMessage) {
        max_width += input.unwrapped_additional_width;
        max_width = max_width.max(input.reactions_optimal_width);
    }
    Size::new(max_width, min_height)
}

/// Computes current size for a GIF / video / round video at `new_width`
/// (`Gif::countCurrentSize`, `history_view_gif.cpp:432-505`).
#[must_use]
pub fn count_gif_current_size(input: &GifInput, new_width: i32) -> Size {
    let available_width = new_width;
    let mut thumb_max_width = new_width;
    let scaled = count_gif_thumb_size(
        input.kind,
        input.dimensions,
        &mut thumb_max_width,
        input.hosted_instant_view,
    );
    let min_width_by_info = if input.hosted_instant_view {
        input.min_width_for_media
    } else if input.hides_bottom_info {
        0
    } else {
        input.info_width + 2 * (MSG_DATE_IMG_DELTA + MSG_DATE_IMG_PADDING.x)
    };
    let min_photo_width = MIN_PHOTO_SIZE.min(thumb_max_width);
    let mut cur_width = scaled
        .width
        .max(min_width_by_info)
        .clamp(min_photo_width, thumb_max_width);
    let mut cur_height = scaled.height.max(MIN_PHOTO_SIZE);
    if !input.is_streaming && input.max_status_width > 0 {
        let status_w = input.max_status_width + 2 * (MSG_DATE_IMG_DELTA + MSG_DATE_IMG_PADDING.x);
        cur_width = cur_width.max(status_w);
    }
    if input.has_bubble {
        cur_width = cur_width.max(input.min_width_for_media);
        let max_with_caption = MSG_MAX_WIDTH.min(input.textual_max_width);
        cur_width = cur_width.max(max_with_caption).min(thumb_max_width);
        cur_height = gif_adjust_height_for_less_crop(scaled, Size::new(cur_width, cur_height));
    } else if matches!(input.kind, GifKind::VideoMessage) {
        cur_width = cur_width.max(input.reactions_optimal_width);
        if input.unwrapped_additional_width > 0 {
            cur_width = (cur_width + input.unwrapped_additional_width).min(available_width);
        }
    }
    Size::new(cur_width, cur_height)
}

// ---------------------------------------------------------------------------
// WebPage preview (history_view_web_page.cpp)
// ---------------------------------------------------------------------------

/// Input parameters for sizing a web page preview card (`HistoryView::WebPage`).
#[derive(Debug, Clone, Copy, Default)]
pub struct WebPageInput {
    /// True when the webpage is still loading or failed (size is `0x0`).
    pub pending_or_failed: bool,
    /// True for compact article layout with square/narrow right thumbnail.
    pub as_article: bool,
    /// Site name line max width (`0` if empty).
    pub site_name_width: i32,
    /// Title text max width (`0` if empty).
    pub title_width: i32,
    /// Title wrapped line count (1 or 2) at current width.
    pub title_lines: i32,
    /// Description text max width (`0` if empty).
    pub description_width: i32,
    /// Description total wrapped height at current width.
    pub description_height: i32,
    /// Thumbnail dimensions when `as_article` is true.
    pub article_photo_size: Size,
    /// Embedded media attachment size (large photo, video, album, document) when `!as_article`.
    pub attach_size: Size,
    /// Width of optional action button ("INSTANT VIEW", "VIEW CHANNEL", etc.), `0` if none.
    pub open_button_width: i32,
    /// Skip block width for bottom timestamp.
    pub skip_block_width: i32,
    /// True if this webpage sits at the bottom of the bubble.
    pub is_bubble_bottom: bool,
}

fn web_page_padding(is_bubble_bottom: bool, has_open_button: bool) -> Margins {
    let in_bubble = Margins::new(
        MSG_PADDING.left,
        0,
        MSG_PADDING.right,
        if is_bubble_bottom {
            MSG_PADDING.bottom
        } else {
            0
        },
    );
    let inner = Margins::new(
        HISTORY_PAGE_PREVIEW_PADDING.left,
        HISTORY_PAGE_PREVIEW_PADDING.top,
        HISTORY_PAGE_PREVIEW_PADDING.right,
        HISTORY_PAGE_PREVIEW_PADDING.bottom
            + if has_open_button {
                HISTORY_PAGE_BUTTON_HEIGHT
            } else {
                0
            },
    );
    in_bubble + inner
}

fn article_thumb_width(photo_size: Size, height: i32) -> i32 {
    if photo_size.is_empty() || height <= 0 {
        return 0;
    }
    let w =
        ((i64::from(photo_size.width) * i64::from(height)) / i64::from(photo_size.height)) as i32;
    w.clamp(1, height)
}

/// Computes optimal size for a web page preview (`WebPage::countOptimalSize`,
/// `history_view_web_page.cpp:401-771`).
#[must_use]
pub fn count_web_page_optimal_size(input: &WebPageInput, fonts: &FontMetrics) -> Size {
    if input.pending_or_failed {
        return Size::ZERO;
    }
    let has_button = input.open_button_width > 0;
    let padding = web_page_padding(input.is_bubble_bottom, has_button);
    let line_h = fonts.united_line_height();

    let site_h = if input.site_name_width > 0 { line_h } else { 0 };
    let title_h = if input.title_width > 0 { line_h } else { 0 };
    let desc_max_lines = 3 + if site_h == 0 { 1 } else { 0 } + if title_h == 0 { 1 } else { 0 };
    let desc_min_h = if input.description_width > 0 {
        input
            .description_height
            .max(line_h)
            .min(desc_max_lines * line_h)
    } else {
        0
    };
    let article_min_h = site_h + title_h + desc_min_h;
    let article_photo_w = if input.as_article && !input.article_photo_size.is_empty() {
        WEB_PAGE_PHOTO_DELTA
            + article_thumb_width(input.article_photo_size, article_min_h).max(line_h)
    } else {
        0
    };

    let mut max_width = input.skip_block_width;
    let mut min_height = 0;

    if site_h > 0 {
        max_width = max_width.max(input.site_name_width + article_photo_w);
        min_height += site_h;
    }
    if title_h > 0 {
        max_width = max_width.max(input.title_width + article_photo_w);
        min_height += title_h;
    }
    if desc_min_h > 0 {
        max_width = max_width.max(input.description_width + article_photo_w);
        min_height += desc_min_h;
    }
    if !input.as_article && !input.attach_size.is_empty() {
        let attach_at_top = site_h == 0 && title_h == 0 && desc_min_h == 0;
        if !attach_at_top {
            min_height += MEDIA_IN_BUBBLE_SKIP;
        }
        max_width = max_width.max(input.attach_size.width);
        min_height += input.attach_size.height;
    }
    if has_button {
        max_width =
            max_width.max(HISTORY_PAGE_BUTTON_PADDING.horizontal() + input.open_button_width);
    }

    Size::new(
        max_width + padding.horizontal(),
        min_height + padding.vertical(),
    )
}

/// Computes current size for a web page preview at `new_width` (`WebPage::countCurrentSize`,
/// `history_view_web_page.cpp:773-900`).
#[must_use]
pub fn count_web_page_current_size(
    input: &WebPageInput,
    new_width: i32,
    fonts: &FontMetrics,
) -> Size {
    if input.pending_or_failed {
        return Size::new(new_width, 0);
    }
    let has_button = input.open_button_width > 0;
    let padding = web_page_padding(input.is_bubble_bottom, has_button);
    let line_h = fonts.united_line_height();
    let lines_max = 5;
    let site_lines = if input.site_name_width > 0 { 1 } else { 0 };
    let title_lines = if input.title_width > 0 {
        input.title_lines.clamp(1, 2)
    } else {
        0
    };
    let rest_lines = (lines_max - site_lines - title_lines).max(0);
    let desc_h = if input.description_width > 0 {
        input
            .description_height
            .max(fonts.web_page_description.height)
            .min(rest_lines * fonts.web_page_description.height)
    } else {
        0
    };
    let mut inner_h = site_lines * line_h + title_lines * line_h + desc_h;
    if input.as_article && !input.article_photo_size.is_empty() {
        let pix_h = inner_h.max(line_h);
        inner_h = inner_h.max(pix_h);
    } else if !input.attach_size.is_empty() {
        if inner_h > 0 {
            inner_h += MEDIA_IN_BUBBLE_SKIP;
        }
        inner_h += input.attach_size.height;
    }
    Size::new(new_width, inner_h + padding.vertical())
}

// ---------------------------------------------------------------------------
// Poll (history_view_poll.cpp)
// ---------------------------------------------------------------------------

/// Single poll answer option metrics (`HistoryView::Poll`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PollAnswerInput {
    /// Measured single-line width of the option text.
    pub text_width: i32,
    /// Wrapped height of the option text at the target width.
    pub text_height: i32,
}

/// Input parameters for sizing a poll (`HistoryView::Poll`, `history_view_poll.cpp:2209-2236`).
#[derive(Debug, Clone, Default)]
pub struct PollInput {
    /// Question single-line max width.
    pub question_width: i32,
    /// Question wrapped height.
    pub question_height: i32,
    /// Description single-line max width (`0` if none).
    pub description_width: i32,
    /// Description wrapped height (`0` if none).
    pub description_height: i32,
    /// Answer options.
    pub answers: Vec<PollAnswerInput>,
    /// True if any answer carries an inline media thumbnail.
    pub any_answer_has_media: bool,
    /// Extra media thumbnail width + skip when `any_answer_has_media` is true.
    pub answer_media_width: i32,
    /// True if the poll shows a bottom action button ("VIEW RESULTS" / "VOTE").
    pub has_bottom_button: bool,
}

/// Computes optimal size for a poll (`Poll::countOptimalSize`, `history_view_poll.cpp:2209-2236`).
#[must_use]
pub fn count_poll_optimal_size(input: &PollInput, fonts: &FontMetrics) -> Size {
    let paddings = MSG_PADDING.horizontal();
    let mut max_width = MSG_FILE_MIN_WIDTH;
    max_width = max_width.max(paddings + input.description_width);
    max_width = max_width.max(paddings + input.question_width);

    let answer_pad = if input.any_answer_has_media {
        HISTORY_POLL_ANSWER_PADDING
    } else {
        HISTORY_POLL_ANSWER_PADDING_NO_MEDIA
    };
    let any_media = if input.any_answer_has_media {
        input.answer_media_width
    } else {
        0
    };
    for answer in &input.answers {
        max_width = max_width
            .max(paddings + answer_pad.left + answer.text_width + any_media + answer_pad.right);
    }

    let min_height = count_poll_height(input, fonts);
    Size::new(max_width, min_height)
}

/// Computes current size for a poll at `new_width`.
#[must_use]
pub fn count_poll_current_size(
    input: &PollInput,
    optimal: Size,
    new_width: i32,
    fonts: &FontMetrics,
) -> Size {
    let width = new_width
        .min(optimal.width)
        .max(MSG_FILE_MIN_WIDTH.min(new_width));
    Size::new(width, count_poll_height(input, fonts))
}

fn count_poll_height(input: &PollInput, fonts: &FontMetrics) -> i32 {
    let answer_pad = if input.any_answer_has_media {
        HISTORY_POLL_ANSWER_PADDING
    } else {
        HISTORY_POLL_ANSWER_PADDING_NO_MEDIA
    };
    let mut height = HISTORY_POLL_QUESTION_TOP
        + input.question_height.max(fonts.semibold.height)
        + HISTORY_POLL_SUBTITLE_SKIP
        + fonts.normal.height;
    if input.description_height > 0 {
        height += input.description_height + HISTORY_POLL_SUBTITLE_SKIP;
    }
    height += HISTORY_POLL_ANSWERS_SKIP;
    for answer in &input.answers {
        height += answer_pad.vertical() + answer.text_height.max(fonts.normal.height);
    }
    height += HISTORY_POLL_TOTAL_VOTES_SKIP + fonts.normal.height;
    if input.has_bottom_button {
        height += HISTORY_POLL_BOTTOM_BUTTON_SKIP + HISTORY_PAGE_BUTTON_HEIGHT;
    }
    height
}

// ---------------------------------------------------------------------------
// Contact (history_view_contact.cpp)
// ---------------------------------------------------------------------------

/// Input parameters for sizing a shared contact card (`HistoryView::Contact`).
#[derive(Debug, Clone, Default)]
pub struct ContactInput {
    /// Contact display name width.
    pub name_width: i32,
    /// Phone number line width.
    pub phone_width: i32,
    /// Action button label widths (e.g. `"SEND MESSAGE"`, `"ADD CONTACT"`).
    pub button_widths: Vec<i32>,
    /// Skip block width for bottom info.
    pub skip_block_width: i32,
    /// True if at the bottom of its bubble.
    pub is_bubble_bottom: bool,
}

/// Computes optimal size for a contact card (`Contact::countOptimalSize`,
/// `history_view_contact.cpp:296-328`).
#[must_use]
pub fn count_contact_optimal_size(input: &ContactInput, fonts: &FontMetrics) -> Size {
    let has_buttons = !input.button_widths.is_empty();
    let in_bubble = Margins::new(
        MSG_PADDING.left,
        0,
        MSG_PADDING.right,
        if input.is_bubble_bottom {
            MSG_PADDING.bottom
        } else {
            0
        },
    );
    let inner_margin = Margins::new(
        HISTORY_PAGE_PREVIEW_PADDING.left,
        HISTORY_PAGE_PREVIEW_PADDING.top,
        HISTORY_PAGE_PREVIEW_PADDING.right,
        HISTORY_PAGE_PREVIEW_PADDING.bottom
            + if has_buttons {
                HISTORY_PAGE_BUTTON_HEIGHT
            } else {
                0
            },
    );
    let padding = in_bubble + inner_margin;
    let line_left = CONTACTS_PHOTO_SIZE + inner_margin.left;
    let line_h = fonts.united_line_height();

    let mut max_width = input.skip_block_width;
    let mut text_min_h = 0;
    if input.name_width > 0 {
        max_width = max_width.max(line_left + input.name_width);
        text_min_h += line_h;
    }
    if input.phone_width > 0 {
        max_width = max_width.max(line_left + input.phone_width);
        text_min_h += line_h;
    }
    let min_height = text_min_h.max(CONTACTS_PHOTO_SIZE);

    if has_buttons {
        let buttons_w: i32 = HISTORY_PAGE_BUTTON_PADDING.horizontal()
            + input.button_widths.iter().copied().sum::<i32>();
        max_width = max_width.max(buttons_w);
    }

    Size::new(
        max_width + padding.horizontal(),
        min_height + padding.vertical(),
    )
}

// ---------------------------------------------------------------------------
// Location / Venue (history_view_location.cpp)
// ---------------------------------------------------------------------------

/// Input parameters for sizing a map location / venue (`HistoryView::Location`).
#[derive(Debug, Clone, Copy, Default)]
pub struct LocationInput {
    /// Explicit size override; if empty, defaults to `LOCATION_SIZE` (`320x240`).
    pub size_override: Size,
    /// True if rendered inside a message bubble (e.g. venue with title/address).
    pub has_bubble: bool,
    /// True if at the bottom of the bubble.
    pub is_bubble_bottom: bool,
    /// Minimum width required by bottom info.
    pub min_width_for_media: i32,
    /// Venue title wrapped height (`0` if plain geo point without title).
    pub title_height: i32,
    /// Venue address/description wrapped height (`0` if empty).
    pub description_height: i32,
    /// True if hosted inside an Instant View article.
    pub hosted_instant_view: bool,
}

fn location_full_size(input: &LocationInput) -> Size {
    Size::new(
        if input.size_override.width > 0 {
            input.size_override.width
        } else {
            LOCATION_SIZE.width
        },
        if input.size_override.height > 0 {
            input.size_override.height
        } else {
            LOCATION_SIZE.height
        },
    )
}

/// Computes optimal size for a location / venue (`Location::countOptimalSize`,
/// `history_view_location.cpp:296-336`).
#[must_use]
pub fn count_location_optimal_size(input: &LocationInput, fonts: &FontMetrics) -> Size {
    let full = location_full_size(input);
    let mut tw = full.width;
    let mut th = full.height;
    if !input.hosted_instant_view && tw > MAX_MEDIA_SIZE {
        th = ((i64::from(MAX_MEDIA_SIZE) * i64::from(th)) / i64::from(tw)) as i32;
        tw = MAX_MEDIA_SIZE;
    }
    let min_width = if input.hosted_instant_view {
        input.min_width_for_media.max(1)
    } else {
        input
            .min_width_for_media
            .clamp(MIN_PHOTO_SIZE, MAX_MEDIA_SIZE)
    };
    let max_width = tw.max(min_width);
    let mut min_height = th.max(MIN_PHOTO_SIZE);

    if input.has_bubble {
        if input.title_height > 0 {
            min_height += input.title_height.min(2 * fonts.web_page_title.height);
        }
        if input.description_height > 0 {
            min_height += input
                .description_height
                .min(3 * fonts.web_page_description.height);
        }
        if input.title_height > 0 || input.description_height > 0 {
            min_height += MEDIA_IN_BUBBLE_SKIP;
            if input.is_bubble_bottom {
                min_height += MSG_PADDING.bottom;
            }
        }
    }

    Size::new(max_width, min_height)
}

/// Computes current size for a location / venue at `new_width` (`Location::countCurrentSize`,
/// `history_view_location.cpp:338-386`).
#[must_use]
pub fn count_location_current_size(
    input: &LocationInput,
    optimal: Size,
    new_width: i32,
    fonts: &FontMetrics,
) -> Size {
    let mut cur_width = new_width.min(optimal.width);
    let full = location_full_size(input);
    let mut tw = full.width;
    let mut th = full.height;
    if !input.hosted_instant_view && tw > MAX_MEDIA_SIZE {
        th = ((i64::from(MAX_MEDIA_SIZE) * i64::from(th)) / i64::from(tw)) as i32;
        tw = MAX_MEDIA_SIZE;
    }
    let mut cur_height = th;
    if tw > cur_width {
        cur_height = ((i64::from(cur_width) * i64::from(cur_height)) / i64::from(tw)) as i32;
    } else {
        cur_width = tw;
    }
    let min_width = if input.hosted_instant_view {
        input.min_width_for_media.max(1)
    } else {
        let upper = cur_width.clamp(MIN_PHOTO_SIZE, MAX_MEDIA_SIZE);
        input.min_width_for_media.clamp(MIN_PHOTO_SIZE, upper)
    };
    cur_width = cur_width.max(min_width);
    cur_height = cur_height.max(MIN_PHOTO_SIZE);

    if input.has_bubble {
        if input.title_height > 0 {
            cur_height += input.title_height.min(2 * fonts.web_page_title.height);
        }
        if input.description_height > 0 {
            cur_height += input
                .description_height
                .min(3 * fonts.web_page_description.height);
        }
        if input.title_height > 0 || input.description_height > 0 {
            cur_height += MEDIA_IN_BUBBLE_SKIP;
            if input.is_bubble_bottom {
                cur_height += MSG_PADDING.bottom;
            }
        }
    }

    Size::new(cur_width, cur_height)
}

// ---------------------------------------------------------------------------
// Call (history_view_call.cpp)
// ---------------------------------------------------------------------------

/// Input parameters for sizing a call bubble (`HistoryView::Call`).
#[derive(Debug, Clone, Copy)]
pub struct CallInput {
    /// Width of the call title (e.g. `"Outgoing Call"`).
    pub text_width: i32,
    /// Width of the status line (time + duration).
    pub status_width: i32,
    /// Width of the call-direction arrow icon (default ~12px).
    pub arrow_icon_width: i32,
    /// Width of the phone / camera / group call icon (default ~24px).
    pub call_icon_width: i32,
    /// Whether this media is at the top of the bubble.
    pub is_bubble_top: bool,
}

impl Default for CallInput {
    fn default() -> Self {
        Self {
            text_width: 0,
            status_width: 0,
            arrow_icon_width: 12,
            call_icon_width: 24,
            is_bubble_top: true,
        }
    }
}

/// Computes optimal size for a call media block (`Call::countOptimalSize`,
/// `history_view_call.cpp:65-103`).
#[must_use]
pub fn count_call_optimal_size(input: &CallInput) -> Size {
    let status_w = input.arrow_icon_width + HISTORY_CALL_STATUS_SKIP + input.status_width;
    let icon_skip = HISTORY_CALL_ICON_SKIP + input.call_icon_width + HISTORY_CALL_ICON_POSITION.x;
    let max_width =
        HISTORY_CALL_WIDTH.max(HISTORY_CALL_LEFT + input.text_width.max(status_w) + icon_skip);
    let mut min_height = HISTORY_CALL_HEIGHT;
    if !input.is_bubble_top {
        min_height -= MSG_FILE_TOP_MINUS;
    }
    Size::new(max_width, min_height)
}

// ---------------------------------------------------------------------------
// Unified media dispatch for Message layout
// ---------------------------------------------------------------------------

/// Unified media payload attached to a message element.
#[derive(Debug, Clone)]
pub enum MediaInput {
    /// Single photo.
    Photo(PhotoInput),
    /// Document / file / voice / audio.
    Document(DocumentInput),
    /// Sticker / animated emoji / dice.
    Sticker(StickerInput),
    /// GIF / video file / round video note.
    Gif(GifInput),
    /// Web page link preview.
    WebPage(WebPageInput),
    /// Poll or quiz.
    Poll(PollInput),
    /// Shared contact card.
    Contact(ContactInput),
    /// Geo location / venue map.
    Location(LocationInput),
    /// Phone / video / group call item.
    Call(CallInput),
    /// Grouped media album (mosaic grid or playlist column).
    Grouped(GroupedMediaInput),
}

/// Computed media sizing properties consumed by [`crate::element::layout_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaLayout {
    /// Optimal size (`countOptimalSize`).
    pub optimal_size: Size,
    /// Current size at the allocated bubble width (`countCurrentSize`).
    pub current_size: Size,
    /// Whether this media requires a surrounding message bubble even without text/headers.
    pub needs_bubble: bool,
    /// Whether this media is rendered unwrapped (stickers, round videos without caption).
    pub unwrapped: bool,
    /// Whether the bubble width is strictly enforced to match the media width.
    pub enforce_bubble_width: bool,
    /// Whether this media suppresses the bubble's bottom tail corner.
    pub skip_bubble_tail: bool,
    /// Whether the media touches the top edge of the bubble (no top `msgPadding`).
    pub is_bubble_top: bool,
    /// Whether the media touches the bottom edge of the bubble (no bottom `msgPadding`).
    pub is_bubble_bottom: bool,
    /// Optional detailed grouped layout when this is [`MediaInput::Grouped`].
    pub grouped: Option<GroupedMediaLayout>,
}

impl MediaInput {
    /// Computes the optimal and current layout of this media at `available_width`.
    #[must_use]
    pub fn layout(
        &self,
        available_width: i32,
        has_header: bool,
        has_caption: bool,
        fonts: &FontMetrics,
    ) -> MediaLayout {
        match self {
            Self::Photo(p) => {
                let optimal = count_photo_optimal_size(p);
                let current = count_photo_current_size(p, optimal, available_width);
                let needs_bubble = p.has_bubble || has_header || has_caption;
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble,
                    unwrapped: false,
                    enforce_bubble_width: true,
                    skip_bubble_tail: !has_caption,
                    is_bubble_top: !has_header,
                    is_bubble_bottom: !has_caption,
                    grouped: None,
                }
            }
            Self::Document(d) => {
                let optimal = count_document_optimal_size(d);
                let current = count_document_current_size(d, optimal, available_width, fonts);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble: true,
                    unwrapped: false,
                    enforce_bubble_width: false,
                    skip_bubble_tail: false,
                    is_bubble_top: d.is_bubble_top,
                    is_bubble_bottom: d.is_bubble_bottom,
                    grouped: None,
                }
            }
            Self::Sticker(s) => {
                let optimal = count_sticker_optimal_size(s);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: Size::new(optimal.width.min(available_width), optimal.height),
                    needs_bubble: false,
                    unwrapped: true,
                    enforce_bubble_width: false,
                    skip_bubble_tail: true,
                    is_bubble_top: true,
                    is_bubble_bottom: true,
                    grouped: None,
                }
            }
            Self::Gif(g) => {
                let optimal = count_gif_optimal_size(g);
                let current = count_gif_current_size(g, available_width);
                let unwrapped =
                    matches!(g.kind, GifKind::VideoMessage) && !g.has_bubble && !has_caption;
                let needs_bubble = !unwrapped && (g.has_bubble || has_header || has_caption);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble,
                    unwrapped,
                    enforce_bubble_width: !unwrapped,
                    skip_bubble_tail: !has_caption,
                    is_bubble_top: !has_header,
                    is_bubble_bottom: !has_caption,
                    grouped: None,
                }
            }
            Self::WebPage(w) => {
                let optimal = count_web_page_optimal_size(w, fonts);
                let current =
                    count_web_page_current_size(w, available_width.min(optimal.width), fonts);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble: true,
                    unwrapped: false,
                    enforce_bubble_width: false,
                    skip_bubble_tail: false,
                    is_bubble_top: false,
                    is_bubble_bottom: w.is_bubble_bottom,
                    grouped: None,
                }
            }
            Self::Poll(poll) => {
                let optimal = count_poll_optimal_size(poll, fonts);
                let current = count_poll_current_size(poll, optimal, available_width, fonts);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble: true,
                    unwrapped: false,
                    enforce_bubble_width: false,
                    skip_bubble_tail: false,
                    is_bubble_top: !has_header,
                    is_bubble_bottom: true,
                    grouped: None,
                }
            }
            Self::Contact(c) => {
                let optimal = count_contact_optimal_size(c, fonts);
                let current = Size::new(available_width.min(optimal.width), optimal.height);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble: true,
                    unwrapped: false,
                    enforce_bubble_width: false,
                    skip_bubble_tail: false,
                    is_bubble_top: !has_header,
                    is_bubble_bottom: c.is_bubble_bottom,
                    grouped: None,
                }
            }
            Self::Location(loc) => {
                let optimal = count_location_optimal_size(loc, fonts);
                let current = count_location_current_size(loc, optimal, available_width, fonts);
                let needs_bubble = loc.has_bubble
                    || loc.title_height > 0
                    || loc.description_height > 0
                    || has_header;
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble,
                    unwrapped: false,
                    enforce_bubble_width: true,
                    skip_bubble_tail: loc.title_height == 0 && loc.description_height == 0,
                    is_bubble_top: !has_header,
                    is_bubble_bottom: loc.is_bubble_bottom,
                    grouped: None,
                }
            }
            Self::Call(c) => {
                let optimal = count_call_optimal_size(c);
                let current = Size::new(available_width.min(optimal.width), optimal.height);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble: true,
                    unwrapped: false,
                    enforce_bubble_width: false,
                    skip_bubble_tail: false,
                    is_bubble_top: c.is_bubble_top,
                    is_bubble_bottom: true,
                    grouped: None,
                }
            }
            Self::Grouped(g) => {
                let mut layout = GroupedMediaLayout::compute_optimal(g, fonts);
                let optimal = layout.optimal_size;
                let current = layout.resize_to_width(available_width, g, None, fonts);
                let is_grid = matches!(g.mode, crate::grouped::GroupedMode::Grid);
                MediaLayout {
                    optimal_size: optimal,
                    current_size: current,
                    needs_bubble: !is_grid || has_header || has_caption,
                    unwrapped: false,
                    enforce_bubble_width: is_grid,
                    skip_bubble_tail: is_grid && !has_caption,
                    is_bubble_top: g.is_bubble_top,
                    is_bubble_bottom: g.is_bubble_bottom,
                    grouped: Some(layout),
                }
            }
        }
    }
}
