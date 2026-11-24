//! Common media and streaming data types and constants.
//!
//! Port of `media/media_common.h` and `media/streaming/media_streaming_common.h`.

/// Sentinel value representing an unknown timestamp in milliseconds (`kTimeUnknown`).
pub const TIME_UNKNOWN: i64 = i64::MIN;

/// Maximum supported media duration in milliseconds (`kDurationMax`).
pub const DURATION_MAX: i64 = i32::MAX as i64;

/// Sentinel value when a stream's duration cannot be determined upfront (`kDurationUnavailable`).
pub const DURATION_UNAVAILABLE: i64 = i64::MAX;

/// Maximum allowed decoded frame area for streaming playback (`3840 * 2160`, 4K).
pub const MAX_FRAME_AREA: u64 = 3840 * 2160;

/// Maximum allowed decoded frame area for inline GIF/clip playback (`1280 * 720`).
pub const MAX_INLINE_AREA: u64 = 1280 * 720;

/// Maximum allowed decoded frame area for WebM frame generator (`1920 * 1080 * 4`).
pub const MAX_FRAME_GENERATOR_AREA: u64 = 1920 * 1080 * 4;

/// Minimum supported playback speed multiplier (`kSpeedMin`).
pub const SPEED_MIN: f64 = 0.5;

/// Maximum supported playback speed multiplier (`kSpeedMax`).
pub const SPEED_MAX: f64 = 2.5;

/// Default sped-up playback multiplier (`kSpedUpDefault`).
pub const SPED_UP_DEFAULT: f64 = 1.7;

/// Checks whether frame dimensions are positive and within `max_area` pixels
/// (mirrors `Media::ValidFrameSize`).
#[must_use]
pub const fn valid_frame_size(width: u32, height: u32, max_area: u64) -> bool {
    width > 0 && height > 0 && (width as u64).saturating_mul(height as u64) <= max_area
}

/// Compares two playback speeds at 0.1x granularity (mirrors `Media::EqualSpeeds`).
#[must_use]
pub fn equal_speeds(a: f64, b: f64) -> bool {
    (a * 10.0).round() as i64 == (b * 10.0).round() as i64
}

/// Rational number (mirrors FFmpeg `AVRational`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    /// Numerator.
    pub num: i32,
    /// Denominator.
    pub den: i32,
}

impl Rational {
    /// Standard 1:1 aspect ratio (`kNormalAspect`).
    pub const NORMAL_ASPECT: Self = Self { num: 1, den: 1 };

    /// FFmpeg universal time base `1 / 1_000_000` (`kUniversalTimeBase`).
    pub const UNIVERSAL_TIME_BASE: Self = Self {
        num: 1,
        den: 1_000_000,
    };

    /// Creates a new rational value.
    #[must_use]
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Converts the rational to `f64` (returns `0.0` if denominator is zero).
    #[must_use]
    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            f64::from(self.num) / f64::from(self.den)
        }
    }
}

impl Default for Rational {
    fn default() -> Self {
        Self::NORMAL_ASPECT
    }
}

/// 2D integer dimensions in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// Creates a new pixel size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns `true` when either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Returns width and height swapped.
    #[must_use]
    pub const fn transposed(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    /// Scales `self` to fit inside `bounds` while preserving aspect ratio.
    #[must_use]
    pub fn scaled_keep_aspect(self, bounds: Self) -> Self {
        if self.is_empty() || bounds.is_empty() {
            return Self::default();
        }
        let w_by_h = (u64::from(self.width) * u64::from(bounds.height)) / u64::from(self.height);
        if w_by_h <= u64::from(bounds.width) {
            Self::new((w_by_h as u32).max(1), bounds.height)
        } else {
            let h_by_w = (u64::from(self.height) * u64::from(bounds.width)) / u64::from(self.width);
            Self::new(bounds.width, (h_by_w as u32).max(1))
        }
    }

    /// Scales `self` to cover `bounds` while preserving aspect ratio.
    #[must_use]
    pub fn scaled_keep_aspect_expanding(self, bounds: Self) -> Self {
        if self.is_empty() || bounds.is_empty() {
            return Self::default();
        }
        let w_by_h = (u64::from(self.width) * u64::from(bounds.height)) / u64::from(self.height);
        if w_by_h >= u64::from(bounds.width) {
            Self::new((w_by_h as u32).max(1), bounds.height)
        } else {
            let h_by_w = (u64::from(self.height) * u64::from(bounds.width)) / u64::from(self.width);
            Self::new(bounds.width, (h_by_w as u32).max(1))
        }
    }
}

/// Clockwise video rotation normalized to 90-degree increments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Rotation {
    /// 0 degrees (upright).
    #[default]
    Deg0,
    /// 90 degrees clockwise.
    Deg90,
    /// 180 degrees.
    Deg180,
    /// 270 degrees clockwise (90 degrees counter-clockwise).
    Deg270,
}

impl Rotation {
    /// Normalizes arbitrary integer degrees to `0`, `90`, `180`, or `270`.
    #[must_use]
    pub const fn from_degrees(degrees: i32) -> Self {
        let mut r = degrees % 360;
        if r < 0 {
            r += 360;
        }
        match r {
            90 => Self::Deg90,
            180 => Self::Deg180,
            270 => Self::Deg270,
            _ => Self::Deg0,
        }
    }

    /// Returns the rotation angle in clockwise degrees.
    #[must_use]
    pub const fn degrees(self) -> i32 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }

    /// Returns `true` when 90 or 270 degrees swap width and height.
    #[must_use]
    pub const fn swaps_width_height(self) -> bool {
        matches!(self, Self::Deg90 | Self::Deg270)
    }

    /// Transposes `size` if rotation is 90 or 270 degrees.
    #[must_use]
    pub const fn transpose_size(self, size: Size) -> Size {
        if self.swaps_width_height() {
            size.transposed()
        } else {
            size
        }
    }
}

/// Requested output pixel format for decoded video frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutputPixelFormat {
    /// Tightly-packed 8-bit straight RGBA (`R, G, B, A`).
    #[default]
    Rgba,
    /// Tightly-packed 8-bit BGRA (`B, G, R, A`).
    Bgra,
    /// Prefer zero-conversion planar YUV420P or NV12 when decoded natively,
    /// falling back to RGBA for other pixel formats (such as YUVA420P with alpha).
    PreferYuv,
}

/// Format of a decoded video frame buffer (mirrors `Media::Streaming::FrameFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameFormat {
    /// 8-bit RGBA (`width * height * 4` bytes).
    Rgba,
    /// 8-bit BGRA (`width * height * 4` bytes).
    Bgra,
    /// Planar YUV 4:2:0 (Y, U, V planes).
    Yuv420,
    /// Bi-planar NV12 (Y plane + interleaved UV plane).
    Nv12,
}

/// Planar YUV 4:2:0 frame buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yuv420Data {
    /// Chroma plane width (`ceil(width / 2)`).
    pub chroma_width: u32,
    /// Chroma plane height (`ceil(height / 2)`).
    pub chroma_height: u32,
    /// Luma (Y) plane (`width * height` bytes).
    pub y: Vec<u8>,
    /// Blue-difference chroma (U/Cb) plane (`chroma_width * chroma_height` bytes).
    pub u: Vec<u8>,
    /// Red-difference chroma (V/Cr) plane (`chroma_width * chroma_height` bytes).
    pub v: Vec<u8>,
}

/// Bi-planar NV12 frame buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12Data {
    /// Chroma width (`ceil(width / 2)`).
    pub chroma_width: u32,
    /// Chroma height (`ceil(height / 2)`).
    pub chroma_height: u32,
    /// Luma (Y) plane (`width * height` bytes).
    pub y: Vec<u8>,
    /// Interleaved UV plane (`chroma_width * 2 * chroma_height` bytes).
    pub uv: Vec<u8>,
}

/// Decoded video frame payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameData {
    /// Tightly-packed RGBA8 pixels (`width * height * 4` bytes).
    Rgba(Vec<u8>),
    /// Tightly-packed BGRA8 pixels (`width * height * 4` bytes).
    Bgra(Vec<u8>),
    /// Planar YUV420P planes.
    Yuv420(Yuv420Data),
    /// Bi-planar NV12 planes.
    Nv12(Nv12Data),
}

impl FrameData {
    /// Returns the `FrameFormat` corresponding to this buffer.
    #[must_use]
    pub const fn format(&self) -> FrameFormat {
        match self {
            Self::Rgba(_) => FrameFormat::Rgba,
            Self::Bgra(_) => FrameFormat::Bgra,
            Self::Yuv420(_) => FrameFormat::Yuv420,
            Self::Nv12(_) => FrameFormat::Nv12,
        }
    }

    /// Returns the RGBA bytes if this frame is in `FrameData::Rgba`.
    #[must_use]
    pub fn as_rgba(&self) -> Option<&[u8]> {
        match self {
            Self::Rgba(buf) => Some(buf),
            _ => None,
        }
    }
}

/// A decoded video frame with presentation timestamp and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Frame width in pixels (after any applied rotation/resize).
    pub width: u32,
    /// Frame height in pixels (after any applied rotation/resize).
    pub height: u32,
    /// Presentation timestamp in milliseconds from stream start (or looped timeline).
    pub pts_ms: i64,
    /// Frame duration in milliseconds (`0` if unknown).
    pub duration_ms: i64,
    /// Sequential 0-based frame index within the current playback cycle.
    pub index: usize,
    /// Whether the source pixel format contained an alpha channel (e.g. WebM VP9 `yuva420p`).
    pub has_alpha: bool,
    /// Source stream rotation metadata.
    pub rotation: Rotation,
    /// Pixel buffer in RGBA, BGRA, YUV420, or NV12 format.
    pub data: FrameData,
}

impl VideoFrame {
    /// Returns frame dimensions as a `Size`.
    #[must_use]
    pub const fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

/// A decoded and resampled audio frame in interleaved 32-bit float PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    /// Sample rate in Hz (e.g. 48000).
    pub sample_rate: u32,
    /// Channel count (e.g. 2 for stereo).
    pub channels: u16,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: i64,
    /// Frame duration in milliseconds.
    pub duration_ms: i64,
    /// Interleaved `f32` PCM samples in `[-1.0, 1.0]` (`frames * channels` elements).
    pub samples: Vec<f32>,
}

/// Streaming playback mode (mirrors `Media::Streaming::Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlaybackMode {
    /// Decode both video and audio tracks.
    #[default]
    Both,
    /// Decode audio track only.
    Audio,
    /// Decode video track only (GIFs, silent videos, stickers).
    Video,
    /// Inspect stream metadata and cover frame without continuous playback.
    Inspection,
}

/// Playback configuration passed to `StreamingPlayer::play`
/// (mirrors `Media::Streaming::PlaybackOptions`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackOptions {
    /// Active track mode.
    pub mode: PlaybackMode,
    /// Initial seek position in milliseconds.
    pub position_ms: i64,
    /// Optional duration override in milliseconds (0 = use container metadata).
    pub duration_override_ms: i64,
    /// Playback speed multiplier (`SPEED_MIN..=SPEED_MAX`).
    pub speed: f64,
    /// Audio volume (`0.0..=1.0`).
    pub volume: f64,
    /// Synchronize video clock to external audio clock when audio is active.
    pub sync_video_by_audio: bool,
    /// Wait for caller `mark_frame_shown()` before advancing/dropping frames.
    pub wait_for_mark_as_shown: bool,
    /// Allow hardware decoding (D3D11VA / DXVA2 / CUDA on Windows).
    pub hw_allowed: bool,
    /// Whether the underlying stream supports seeking.
    pub seekable: bool,
    /// Loop back to start upon reaching EOF.
    pub loop_playback: bool,
    /// Desired output pixel format for decoded video frames.
    pub output_format: OutputPixelFormat,
    /// Apply stream rotation metadata directly to RGBA/BGRA output pixels.
    pub apply_rotation: bool,
    /// Optional target resize dimensions (None = native stream size).
    pub resize: Option<Size>,
    /// Target audio output sample rate in Hz (default 48000).
    pub audio_sample_rate: u32,
    /// Target audio output channel count (default 2).
    pub audio_channels: u16,
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            mode: PlaybackMode::Both,
            position_ms: 0,
            duration_override_ms: 0,
            speed: 1.0,
            volume: 1.0,
            sync_video_by_audio: true,
            wait_for_mark_as_shown: false,
            hw_allowed: false,
            seekable: true,
            loop_playback: false,
            output_format: OutputPixelFormat::Rgba,
            apply_rotation: true,
            resize: None,
            audio_sample_rate: 48_000,
            audio_channels: 2,
        }
    }
}

/// Position/buffering state of a single track (mirrors `Media::Streaming::TrackState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackState {
    /// Current playback position in milliseconds (`TIME_UNKNOWN` if not started).
    pub position_ms: i64,
    /// Preloaded/buffered position in milliseconds.
    pub received_till_ms: i64,
    /// Total track duration in milliseconds (`DURATION_UNAVAILABLE` if unknown).
    pub duration_ms: i64,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            position_ms: TIME_UNKNOWN,
            received_till_ms: TIME_UNKNOWN,
            duration_ms: TIME_UNKNOWN,
        }
    }
}

impl TrackState {
    /// Returns `true` when the entire track has been received.
    #[must_use]
    pub const fn is_fully_received(self) -> bool {
        self.duration_ms != TIME_UNKNOWN
            && self.duration_ms != DURATION_UNAVAILABLE
            && self.received_till_ms >= self.duration_ms
    }
}

/// Metadata describing an opened video track (mirrors `Media::Streaming::VideoInformation`).
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInformation {
    /// Track playback and buffering state.
    pub state: TrackState,
    /// Display dimensions after aspect-ratio correction and rotation.
    pub size: Size,
    /// Encoded dimensions after rotation (without aspect-ratio scaling).
    pub real_size: Size,
    /// Sample aspect ratio.
    pub aspect: Rational,
    /// Clockwise rotation from container metadata.
    pub rotation: Rotation,
    /// Guessed frames per second (`0.0` if unavailable).
    pub fps: f64,
    /// Whether the video stream carries an alpha channel.
    pub has_alpha: bool,
    /// Codec name (e.g. `"h264"`, `"vp9"`).
    pub codec_name: String,
    /// Optional cover frame captured on stream open.
    pub cover: Option<VideoFrame>,
}

impl Default for VideoInformation {
    fn default() -> Self {
        Self {
            state: TrackState::default(),
            size: Size::default(),
            real_size: Size::default(),
            aspect: Rational::NORMAL_ASPECT,
            rotation: Rotation::Deg0,
            fps: 0.0,
            has_alpha: false,
            codec_name: String::new(),
            cover: None,
        }
    }
}

/// Metadata describing an opened audio track (mirrors `Media::Streaming::AudioInformation`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioInformation {
    /// Track playback and buffering state.
    pub state: TrackState,
    /// Source sample rate in Hz.
    pub sample_rate: u32,
    /// Source channel count.
    pub channels: u16,
    /// Codec name (e.g. `"aac"`, `"opus"`).
    pub codec_name: String,
}

/// Combined stream metadata (mirrors `Media::Streaming::Information`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamInformation {
    /// Video stream metadata, if present.
    pub video: Option<VideoInformation>,
    /// Audio stream metadata, if present.
    pub audio: Option<AudioInformation>,
    /// Bytes read for container header initialization.
    pub header_size: usize,
    /// Demuxer format name (e.g. `"mov,mp4,m4a,3gp,3g2,mj2"`, `"matroska,webm"`).
    pub format_name: String,
}

/// Network download speed estimate (mirrors `Media::Streaming::SpeedEstimate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpeedEstimate {
    /// Estimated throughput in bytes/second (clamped to 64 MiB/s).
    pub bytes_per_second: u32,
    /// `true` when fewer than 3 parts (384 KB) have completed in the window.
    pub unreliable: bool,
}

/// Alternative video quality variant descriptor (mirrors `QualityDescriptor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QualityDescriptor {
    /// Video height in pixels (e.g. 360, 720, 1080).
    pub height: u32,
    /// Total file size in bytes for this quality variant.
    pub size_in_bytes: u64,
}

/// Per-instance rendering request parameters (mirrors `Media::Streaming::FrameRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrameRequest {
    /// Requested inner video size.
    pub resize: Option<Size>,
    /// Requested outer canvas size.
    pub outer: Option<Size>,
    /// Whether circular masking should be applied (round video notes).
    pub round_circle: bool,
    /// Optional corner radius in pixels for rounded-rectangle masking.
    pub corner_radius: u32,
    /// Preserve alpha channel rather than compositing over an opaque background.
    pub keep_alpha: bool,
    /// Request RGBA output (`false` permits YUV420/NV12 output).
    pub require_rgba: bool,
    /// Whether blurred background fill is requested when aspect ratios differ.
    pub blurred_background: bool,
    /// Strict match requirement.
    pub strict: bool,
}

/// Events emitted by `StreamingPlayer` (mirrors `Media::Streaming::Update`).
#[derive(Debug, Clone, PartialEq)]
pub enum StreamingUpdate {
    /// Stream opened and first frame/metadata is ready.
    Information(Box<StreamInformation>),
    /// Video preloaded position updated (milliseconds).
    PreloadedVideo(i64),
    /// Video playback position updated (milliseconds).
    UpdateVideo(i64),
    /// Audio preloaded position updated (milliseconds).
    PreloadedAudio(i64),
    /// Audio playback position updated (milliseconds).
    UpdateAudio(i64),
    /// Buffering / waiting-for-data state changed.
    WaitingForData(bool),
    /// Download speed estimate updated.
    SpeedEstimate(SpeedEstimate),
    /// Playback muted by another active audio source.
    MutedByOther,
    /// Playback reached the end of all active tracks.
    Finished,
}
