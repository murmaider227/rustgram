//! Core audio types, constants, and state predicates.
//!
//! Port of `media/media_common.h`, `data/data_audio_msg_id.h`,
//! `data/data_audio_msg_id.cpp`, and `media/audio/media_audio.h`.

use std::sync::atomic::{AtomicU32, Ordering};

use tg_data::{DocumentId, FullMsgId};

/// Default internal mixing and voice recording sample rate (48 kHz).
pub const DEFAULT_FREQUENCY: u32 = 48_000;

/// Number of concurrent track slots per type (`Voice` and `Song`) in the mixer.
/// Matches `kTogetherLimit` in `media_audio.h`.
pub const TOGETHER_LIMIT: usize = 4;

/// Number of bars in a Telegram voice message waveform.
/// Matches `kWaveformSamplesCount` in `media_audio.h`.
pub const WAVEFORM_SAMPLES_COUNT: usize = 100;

/// Minimum supported playback speed multiplier (`kSpeedMin` in `media_common.h`).
pub const SPEED_MIN: f64 = 0.5;

/// Maximum supported playback speed multiplier (`kSpeedMax` in `media_common.h`).
pub const SPEED_MAX: f64 = 2.5;

/// Default "fast" voice playback speed (`kSpedUpDefault` in `media_common.h`).
pub const SPED_UP_DEFAULT: f64 = 1.7;

/// Standard fade-in / fade-out duration in milliseconds (`kFadeDuration`).
pub const FADE_DURATION_MS: i64 = 500;

/// Global suppression ramp-down duration in milliseconds (`kMediaPlayerSuppressDuration`).
pub const SUPPRESS_ATTACK_MS: i64 = 150;

/// Volume multiplier when suppressing all playback (`kSuppressRatioAll`).
pub const SUPPRESS_RATIO_ALL: f64 = 0.2;

/// Volume multiplier when ducking music for voice playback (`kSuppressRatioSong`).
pub const SUPPRESS_RATIO_SONG: f64 = 0.05;

/// Preload threshold in seconds (`kPreloadSeconds`).
pub const PRELOAD_SECONDS: i64 = 2;

/// Playback position polling interval in milliseconds (`kCheckPlaybackPositionTimeout`).
pub const CHECK_PLAYBACK_POSITION_TIMEOUT_MS: i64 = 100;

/// Minimum sample delta before firing a position update (`kCheckPlaybackPositionDelta`).
pub const CHECK_PLAYBACK_POSITION_DELTA: i64 = 2_400;

/// Fading tick interval in milliseconds (`kCheckFadingTimeout`).
pub const CHECK_FADING_TIMEOUT_MS: i64 = 7;

/// Delay before closing the audio output device after silence (`kDetachDeviceTimeout`).
pub const DETACH_DEVICE_TIMEOUT_MS: i64 = 500;

/// Minimum duration (seconds) for an audio file to allow speed changes
/// (`kMinLengthForChangeablePlaybackSpeed` = 20 minutes).
pub const MIN_LENGTH_FOR_CHANGEABLE_SPEED_SECS: i64 = 20 * 60;

/// Minimum duration (seconds) for saving resume position on video files (1 minute).
pub const MIN_LENGTH_FOR_SAVE_POSITION_VIDEO_SECS: i64 = 60;

/// Minimum duration (seconds) for saving resume position on music/audio files (20 minutes).
pub const MIN_LENGTH_FOR_SAVE_POSITION_MUSIC_SECS: i64 = 20 * 60;

/// Sentinel value indicating unknown time or duration (`kTimeUnknown`).
pub const TIME_UNKNOWN: i64 = i64::MIN;

/// Sentinel value indicating unavailable duration (`kDurationUnavailable`).
pub const DURATION_UNAVAILABLE: i64 = i64::MAX;

/// Compares two playback speeds at tdesktop's 0.1x precision (`EqualSpeeds`).
#[must_use]
pub fn equal_speeds(a: f64, b: f64) -> bool {
    (a * 10.0).round() as i32 == (b * 10.0).round() as i32
}

/// Clamps a playback speed into `[SPEED_MIN, SPEED_MAX]`.
#[must_use]
pub fn clamp_speed(speed: f64) -> f64 {
    if speed.is_nan() {
        1.0
    } else {
        speed.clamp(SPEED_MIN, SPEED_MAX)
    }
}

/// Converts a speed-dependent sample position back to normal-speed samples.
///
/// Matches `Mixer::Track::SpeedIndependentPosition` (`media_audio.cpp:440`).
#[must_use]
pub fn speed_independent_position(position: i64, speed: f64) -> i64 {
    let clamped = clamp_speed(speed);
    ((position as f64) * clamped).round() as i64
}

/// Converts a normal-speed sample position into speed-dependent samples.
///
/// Matches `Mixer::Track::SpeedDependentPosition` (`media_audio.cpp:448`).
#[must_use]
pub fn speed_dependent_position(position: i64, speed: f64) -> i64 {
    let clamped = clamp_speed(speed);
    ((position as f64) / clamped).round() as i64
}

/// Playlist repeat mode (`Media::RepeatMode` in `media_common.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RepeatMode {
    /// Stop at the end of the playlist.
    #[default]
    None,
    /// Repeat the current track indefinitely.
    One,
    /// Loop through the entire playlist.
    All,
}

/// Playlist ordering mode (`Media::OrderMode` in `media_common.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OrderMode {
    /// Play items in ascending chronological / playlist index order.
    #[default]
    Default,
    /// Play items in reverse order.
    Reverse,
    /// Play items in pseudo-random non-repeating shuffle order.
    Shuffle,
}

/// Classification of an audio item (`AudioMsgId::Type` in `data_audio_msg_id.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum AudioType {
    /// Unclassified / empty track.
    #[default]
    Unknown,
    /// Voice note or round video message.
    Voice,
    /// Music / audio file.
    Song,
    /// Audio track belonging to a regular video player.
    Video,
}

/// Descriptor for a document's media kind used to derive [`AudioMsgId`] fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Voice note (`.ogg` Opus).
    VoiceMessage,
    /// Round video note (video + audio, played in Voice playlist).
    VideoMessage,
    /// Music / audio document.
    AudioFile,
    /// Regular video file.
    VideoFile,
    /// Other non-audio document.
    Other,
}

static NEXT_EXTERNAL_PLAY_ID: AtomicU32 = AtomicU32::new(0);

/// Identifier for a playable audio item (`AudioMsgId` in `data_audio_msg_id.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AudioMsgId {
    /// Underlying Telegram document ID (0 for synthetic video/external streams).
    pub document_id: DocumentId,
    /// Chat/message context ID where this audio lives.
    pub context_id: FullMsgId,
    /// Non-zero unique ID for externally-driven streaming playback.
    pub external_play_id: u32,
    /// Playback slot type (`Voice`, `Song`, `Video`, or `Unknown`).
    pub audio_type: AudioType,
    /// Whether playback speed controls apply to this item.
    pub changeable_playback_speed: bool,
    /// Whether this item is a round video message.
    pub is_video_message: bool,
    /// Known duration in milliseconds (0 if unknown).
    pub duration_ms: i64,
}

impl AudioMsgId {
    /// Allocates a monotonically increasing non-zero external play ID.
    ///
    /// Matches `AudioMsgId::CreateExternalPlayId` (`data_audio_msg_id.cpp:34`).
    #[must_use]
    pub fn create_external_play_id() -> u32 {
        loop {
            let next = NEXT_EXTERNAL_PLAY_ID
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            if next != 0 {
                return next;
            }
        }
    }

    /// Creates an [`AudioMsgId`] from document metadata, matching `AudioMsgId` constructor rules.
    #[must_use]
    pub fn new(
        document_id: DocumentId,
        context_id: FullMsgId,
        kind: MediaKind,
        duration_ms: i64,
        external_play_id: u32,
    ) -> Self {
        let audio_type = match kind {
            MediaKind::VoiceMessage | MediaKind::VideoMessage => AudioType::Voice,
            MediaKind::VideoFile => AudioType::Video,
            MediaKind::AudioFile => AudioType::Song,
            MediaKind::Other => AudioType::Unknown,
        };
        let duration_secs = duration_ms / 1000;
        let changeable_playback_speed =
            matches!(kind, MediaKind::VoiceMessage | MediaKind::VideoMessage)
                || duration_secs >= MIN_LENGTH_FOR_CHANGEABLE_SPEED_SECS;

        Self {
            document_id,
            context_id,
            external_play_id,
            audio_type,
            changeable_playback_speed,
            is_video_message: kind == MediaKind::VideoMessage,
            duration_ms,
        }
    }

    /// Creates an [`AudioMsgId`] for a standalone video audio stream (`AudioMsgId::ForVideo`).
    #[must_use]
    pub fn for_video() -> Self {
        Self {
            document_id: DocumentId(0),
            context_id: FullMsgId::default(),
            external_play_id: Self::create_external_play_id(),
            audio_type: AudioType::Video,
            changeable_playback_speed: true,
            is_video_message: false,
            duration_ms: 0,
        }
    }

    /// Returns `true` if this ID references an actual track or external stream.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.document_id.0 != 0 || self.external_play_id != 0
    }

    /// Returns `true` if `self` and `other` point to the same document and message context.
    #[must_use]
    pub const fn same_item(&self, other: &Self) -> bool {
        self.document_id.0 == other.document_id.0
            && self.context_id.peer.0 == other.context_id.peer.0
            && self.context_id.msg.0 == other.context_id.msg.0
    }
}

/// Playback lifecycle state of a mixer track (`Media::Player::State` in `media_audio.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum PlaybackState {
    /// Stopped cleanly.
    #[default]
    Stopped = 0x01,
    /// Reached end of stream naturally.
    StoppedAtEnd = 0x02,
    /// Stopped due to a playback or decoding error.
    StoppedAtError = 0x03,
    /// Failed before playback could begin.
    StoppedAtStart = 0x04,

    /// Fading in at start of playback.
    Starting = 0x08,
    /// Actively playing at full target gain.
    Playing = 0x10,
    /// Fading out toward [`PlaybackState::Stopped`].
    Stopping = 0x18,
    /// Fading out toward [`PlaybackState::Paused`].
    Pausing = 0x20,
    /// Paused mid-stream.
    Paused = 0x28,
    /// Paused after reaching the end of buffered audio.
    PausedAtEnd = 0x30,
    /// Fading in after being paused.
    Resuming = 0x38,
}

impl PlaybackState {
    /// `IsStopped` from `media_audio.h:88`.
    #[must_use]
    pub const fn is_stopped(self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::StoppedAtEnd | Self::StoppedAtError | Self::StoppedAtStart
        )
    }

    /// `IsStoppedOrStopping` from `media_audio.h:95`.
    #[must_use]
    pub const fn is_stopped_or_stopping(self) -> bool {
        self.is_stopped() || matches!(self, Self::Stopping)
    }

    /// `IsStoppedAtEnd` from `media_audio.h:99`.
    #[must_use]
    pub const fn is_stopped_at_end(self) -> bool {
        matches!(self, Self::StoppedAtEnd)
    }

    /// `IsPaused` from `media_audio.h:103`.
    #[must_use]
    pub const fn is_paused(self) -> bool {
        matches!(self, Self::Paused | Self::PausedAtEnd)
    }

    /// `IsPausedOrPausing` from `media_audio.h:108`.
    #[must_use]
    pub const fn is_paused_or_pausing(self) -> bool {
        self.is_paused() || matches!(self, Self::Pausing)
    }

    /// `IsFading` from `media_audio.h:112`.
    #[must_use]
    pub const fn is_fading(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Stopping | Self::Pausing | Self::Resuming
        )
    }

    /// `IsActive` from `media_audio.h:119`.
    #[must_use]
    pub const fn is_active(self) -> bool {
        !self.is_stopped() && !self.is_paused()
    }

    /// `ShowPauseIcon` from `media_audio.h:123`.
    #[must_use]
    pub const fn show_pause_icon(self) -> bool {
        !self.is_stopped_or_stopping() && !self.is_paused_or_pausing()
    }
}

/// Snapshot of a track's playback progress and state (`Media::Player::TrackState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackState {
    /// Track identifier.
    pub id: AudioMsgId,
    /// Current playback state.
    pub state: PlaybackState,
    /// Current speed-independent position in sample frames at `frequency`.
    pub position: i64,
    /// Samples received/buffered so far (for progressive downloads).
    pub received_till: i64,
    /// Total length in sample frames at `frequency`.
    pub length: i64,
    /// Sample rate in Hz (default 48,000).
    pub frequency: u32,
    /// Container header byte size if known.
    pub file_header_size: usize,
    /// Whether playback is currently stalled waiting for more data.
    pub waiting_for_data: bool,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            id: AudioMsgId::default(),
            state: PlaybackState::Stopped,
            position: 0,
            received_till: 0,
            length: 0,
            frequency: DEFAULT_FREQUENCY,
            file_header_size: 0,
            waiting_for_data: false,
        }
    }
}

impl TrackState {
    /// Current playback position in milliseconds.
    #[must_use]
    pub fn position_ms(&self) -> i64 {
        if self.frequency == 0 || self.position == TIME_UNKNOWN {
            return 0;
        }
        (self.position * 1000) / i64::from(self.frequency)
    }

    /// Total track duration in milliseconds.
    #[must_use]
    pub fn length_ms(&self) -> i64 {
        if self.frequency == 0 || self.length <= 0 || self.length == TIME_UNKNOWN {
            return 0;
        }
        (self.length * 1000) / i64::from(self.frequency)
    }

    /// Normalized progress in `[0.0, 1.0]`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.length <= 0 || self.position <= 0 {
            return 0.0;
        }
        (self.position as f64 / self.length as f64).clamp(0.0, 1.0)
    }
}
