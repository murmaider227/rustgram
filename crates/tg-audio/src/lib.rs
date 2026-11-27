//! `tg-audio`: Audio playback, mixing, Opus voice recording, waveform computation,
//! playlists, notification sound cache, floating mini-player state, and device management.
//!
//! Ported from Telegram Desktop's `Telegram/SourceFiles/media/audio/`,
//! `Telegram/SourceFiles/media/player/`, and `Telegram/lib_webrtc/webrtc/webrtc_device_*`.

pub mod background_stream;
pub mod capture;
pub mod decoder;
pub mod device;
pub mod dsp;
pub mod error;
pub mod external;
pub mod float_controller;
pub mod local_cache;
pub mod mixer;
pub mod opus_codec;
pub mod output;
pub mod player;
pub mod sound_track;
pub mod source;
pub mod types;
pub mod waveform;

pub use background_stream::{AudioStreamOpener, BackgroundAudioStream};
pub use capture::{
    AudioRecorder, CAPTURE_FADE_IN_DURATION_MS, CAPTURE_FREQUENCY, CAPTURE_MAX_DURATION_SECS,
    CAPTURE_SKIP_DURATION_MS, CAPTURE_UPDATE_DELTA_MS, CaptureConfig, CaptureResult, CaptureUpdate,
    VoiceCapturePipeline,
};
pub use decoder::{
    AudioAttributes, AudioDecoder, count_waveform_from_bytes, decode_sound_track,
    prepare_for_sending,
};
pub use device::{
    DEFAULT_DEVICE_ID, DeviceChangeReason, DeviceInfo, DeviceManager, DeviceResolvedId,
    DeviceResolver, DeviceType,
};
pub use dsp::{Resampler, TimeStretcher};
pub use error::{AudioError, AudioResult};
pub use external::{EXTERNAL_FEED_MAX_MS, ExternalPcmFeed, ExternalPcmStream};
pub use float_controller::{
    DragOutcome, FloatColumn, FloatController, FloatCorner, FloatItemState, FloatSide, Point, Rect,
    SectionArea, Size, compute_out_ratio,
};
pub use local_cache::{
    LOCAL_SOUND_MAX_DURATION_MS, LOCAL_SOUND_SAMPLE_RATE, LocalCache, LocalDiskCache, LocalSound,
    convert_and_cut_to_wav, encode_wav_pcm_s16le,
};
pub use mixer::{
    AudioStream, ExternalSyncPoint, MemoryPcmStream, MixerCore, MixerEvent, PcmChunk,
    compute_volume,
};
pub use opus_codec::{
    AudioEditResult, OPUS_VOICE_BITRATE_BPS, OPUS_VOICE_FRAME_SAMPLES, OggOpusStreamDecoder,
    concat_ogg_opus, decode_ogg_opus_to_f32, decode_ogg_opus_to_i16, encode_ogg_opus_i16,
    is_ogg_opus_header, trim_ogg_opus,
};
pub use output::AudioOutputEngine;
pub use player::{
    PlayerEvent, PlayerInstance, PlaylistNavigator, SeekingPhase, ShuffleState,
    compute_seek_position_ms, save_last_playback_position,
};
pub use sound_track::{MAX_SOUND_TRACK_FILE_SIZE, SoundTrack, SoundTrackId};
pub use source::{
    AudioDataSource, AudioInput, FileAudioSource, InMemoryPlaylistProvider, InMemoryPositionStore,
    MemoryAudioSource, PlaylistContext, PlaylistEntry, PlaylistProvider, ReadSeek,
    ResumePositionStore, StreamedInputOpener,
};
pub use tg_data::{DocumentId, FullMsgId, MsgId, PeerId};
pub use types::{
    AudioMsgId, AudioType, DEFAULT_FREQUENCY, FADE_DURATION_MS, MediaKind, OrderMode,
    PlaybackState, RepeatMode, SPED_UP_DEFAULT, SPEED_MAX, SPEED_MIN, TOGETHER_LIMIT, TrackState,
    WAVEFORM_SAMPLES_COUNT, clamp_speed, equal_speeds, speed_dependent_position,
    speed_independent_position,
};
pub use waveform::{
    CaptureWaveformCollector, VoiceWaveform, collect_waveform_from_bins, compute_waveform_from_f32,
    compute_waveform_from_i16, decode_5bit, encode_5bit, normalize_peaks,
};
