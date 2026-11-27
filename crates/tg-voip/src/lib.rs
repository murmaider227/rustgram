//! `tg-voip`: Telegram 1:1 audio/video and group call media engine in Rust.
//!
//! Implements 1:1 calls (audio + video) and group calls (interactive SFU DTLS-SRTP + broadcast
//! chunked playback) verified against `TelegramMessenger/tgcalls` (`development` branch):
//! - [`crypto`]: MTProto 2.0 AES-256-CTR encryption (`CryptoHelper.cpp`), sequence/ACK/replay
//!   framing (`EncryptedConnection.cpp`), and legacy typed messages (`Message.cpp`).
//! - [`signaling`]: `tgcalls` v2 JSON signaling (`InitialSetup`, `NegotiateChannels`,
//!   `Candidates`, `MediaState` from `v2/Signaling.cpp`) over [`crypto::EncryptedConnection`].
//! - [`reflector`]: Telegram UDP/TCP reflector relay framing (`v2/ReflectorPort.cpp`),
//!   RFC 5389 STUN / RFC 5766 TURN helpers (`TurnCustomizerImpl.cpp`), privacy-respecting
//!   candidate selection, and [`reflector::LoopbackReflector`] for local testing.
//! - [`rtp`]: RFC 3550 RTP audio/video packetization, RTCP PLI/FIR keyframe requests, keepalive
//!   Ping/Pong RTT probes, Receiver Reports, RFC 6464 audio-level parsing, and SCTP demux.
//! - [`dsp`]: Full-duplex 48 kHz voice DSP (`aec3` pure-Rust WebRTC HPF + AEC3 + NS + AGC2)
//!   and `opus-pure` VoIP encoder (reusing `tg-audio`'s pure-Rust Opus stack).
//! - [`jitter`]: Adaptive 48 kHz NetEq-lite jitter buffer with sequence unwrap, reordering,
//!   Opus in-band FEC recovery, and Opus Packet Loss Concealment (PLC).
//! - [`audio_io`]: Lock-free `cpal` WASAPI capture/playback (`ringbuf` SPSC) with `tg-audio`
//!   device resolution/hot-swap and a headless virtual audio backend for loopback tests.
//! - [`quality`]: Network-type aware adaptive audio + video bitrate/resolution control, 0–4
//!   signal bar estimation (`InstanceV2Impl.cpp`), traffic stats, and `phone.saveCallDebug` JSON.
//! - [`transport`]: Async Tokio UDP transport loop over Telegram reflectors and P2P candidates.
//! - [`engine`]: [`MediaEngine`] and [`CallMediaEngine`] API driven by `tg-calls::Call`.
//! - [`video`] & [`video_mf`]: Windows Media Foundation camera capture (`IMFSourceReader`),
//!   DXGI Desktop Duplication screen capture, Media Foundation H.264 MFT encode/decode (HW
//!   preferred with SW fallback), pixel conversion, rotation/aspect handling, and RGBA output.
//! - [`video_rtp`]: RFC 6184 H.264 RTP packetization (Single NAL, STAP-A, FU-A) & depacketization.
//! - [`group`]: Group calls (`GroupMediaEngine`, pure-Rust DTLS 1.2 + SRTP via `dimpl` +
//!   `webrtc-srtp`, ICE-lite SFU client, multi-SSRC Opus mixer & speaking levels, and
//!   [`group::BroadcastChunkLoader`] chunked stream playback).

pub mod audio_io;
pub mod crypto;
pub mod dsp;
pub mod engine;
pub mod error;
pub mod group;
pub mod jitter;
pub mod quality;
pub mod reflector;
pub mod rtp;
pub mod signaling;
pub mod transport;
pub mod turn;
pub mod video;
#[cfg(windows)]
pub mod video_mf;
pub mod video_rtp;

pub use audio_io::{AudioBackendKind, VirtualAudioHandle, VoipAudioIo, is_headless_audio};
pub use crypto::{
    AUTH_KEY_SIZE, ChannelType, DecryptedRawMessage, DecryptedTransportMessage,
    EncryptedConnection, EncryptedPacket, MSG_KEY_SIZE, TransportMessage, aes_256_ctr_apply,
    compute_msg_key, prepare_aes_key_iv,
};
pub use dsp::{
    DEFAULT_VOICE_BITRATE_BPS, DSP_TEN_MS_SAMPLES, VoiceDsp, VoiceDspConfig, VoiceOpusEncoder,
    active_dsp_thread_count, compute_rms_level,
};
pub use engine::{
    CallMediaEngine, FinalCallState, MediaEngine, MediaEngineConfig, MediaEngineEvent,
    active_media_task_count,
};
pub use error::{VoipError, VoipResult};
pub use group::{
    BROADCAST_STREAM_AUDIO_SSRC, BroadcastChunkLoader, BroadcastChunkRequest,
    BroadcastChunkResponse, BroadcastChunkScale, BroadcastChunkStatus, BroadcastStreamPlayer,
    DtlsIdentity, DtlsPollOutput, DtlsSrtpSession, GroupAudioMixer, GroupMediaConfig,
    GroupMediaEngine, GroupMediaEvent, IceLiteClient, IceStunOutcome, LocalIceCredentials,
    SfuUdpCandidate, WebRtcPacketKind, active_group_media_task_count, build_group_join_payload,
    build_ice_binding_request, build_ice_binding_response, classify_webrtc_packet,
    encode_ogg_opus_chunk, extract_ogg_opus_packets, is_rtcp_packet, parse_sfu_udp_candidates,
    sha256_fingerprint, transform_incoming_e2e_frame, transform_outgoing_e2e_frame,
    verify_stun_integrity_and_fingerprint,
};
pub use jitter::{
    AdaptiveJitterBuffer, DEFAULT_MAX_DELAY_MS, DEFAULT_MIN_DELAY_MS, JitterBufferConfig,
    JitterBufferStats, PullOutcome, VOIP_FRAME_MS, VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE,
};
pub use quality::{
    BitrateLogRecord, ConnectionEndpointKind, DataSaving, MAX_AUDIO_BITRATE_BPS,
    MAX_VIDEO_BITRATE_BPS, MIN_AUDIO_BITRATE_BPS, MIN_VIDEO_BITRATE_BPS, NetworkLogRecord,
    NetworkType, QualityController, TrafficStats, VideoQualityProfile, VoipStatsSnapshot,
};
pub use reflector::{
    DEFAULT_REFLECTOR_PORT, LoopbackReflector, LoopbackReflectorStats, ParsedSdpCandidate,
    ReflectorImpairment, ReflectorIncoming, ReflectorTag, StunBindingResponse, TransportCandidate,
    build_stun_binding_request, build_stun_binding_response, build_turn_allocate_request,
    collect_transport_candidates, decode_reflector_udp_packet, encode_reflector_tcp_hello,
    encode_reflector_udp_data, encode_reflector_udp_hello, format_host_sdp_candidate,
    format_reflector_hostname, format_reflector_sdp_candidate, is_stun_packet,
    parse_reflector_hostname, parse_sdp_candidate, parse_stun_binding_response,
};
pub use rtp::{
    H264_CLOCK_RATE, H264_RTP_PAYLOAD_TYPE, OPUS_RTP_PAYLOAD_TYPE, RTCP_PSFB_FMT_FIR,
    RTCP_PSFB_FMT_PLI, RTCP_PT_PSFB, RtpHeader, SCTP_DEMUX_MAGIC, VoipPacket,
    parse_rtp_audio_level_ext,
};
pub use signaling::{
    SignalingChannel, SignalingDtlsFingerprint, SignalingFeedbackType, SignalingIceCandidate,
    SignalingMediaContent, SignalingMediaType, SignalingMessage, SignalingPayloadType,
    SignalingRtpExtension, SignalingSsrcGroup, SignalingVideoState,
};
pub use transport::{TransportRecvOutcome, TransportWireMode, VoipTransport};
pub use turn::{
    LoopbackTcpReflector, LoopbackTurnServer, ParsedTurnMessage, SCTP_PPID_BINARY, SCTP_PPID_DCEP,
    SCTP_PPID_STRING, SctpChunk, SctpDataChannelRouter, SctpDataMessage, SctpIncomingOutcome,
    TURN_ALLOCATE_ERROR, TURN_ALLOCATE_REQUEST, TURN_ALLOCATE_RESPONSE, TURN_CHANNEL_BIND_REQUEST,
    TURN_CHANNEL_BIND_RESPONSE, TURN_CREATE_PERMISSION_REQUEST, TURN_CREATE_PERMISSION_RESPONSE,
    TURN_DATA_INDICATION, TURN_DEFAULT_LIFETIME_SECS, TURN_MAX_CHANNEL_NUMBER,
    TURN_MIN_CHANNEL_NUMBER, TURN_REFRESH_REQUEST, TURN_REFRESH_RESPONSE, TURN_SEND_INDICATION,
    TcpReflectorRoute, TurnAllocationClient, TurnAllocationState, TurnIncomingOutcome,
    build_turn_allocate_auth_request, build_turn_channel_bind_request,
    build_turn_create_permission_request, build_turn_data_indication, build_turn_refresh_request,
    build_turn_send_indication, decode_turn_channel_data, drain_tcp_reflector_frames,
    encode_tcp_reflector_frame, encode_turn_channel_data, format_turn_relay_sdp_candidate,
    is_turn_channel_data, md5_digest, parse_turn_message, turn_attr, turn_long_term_key,
};
pub use video::{
    CameraCaptureConfig, CameraDeviceInfo, DisplayDeviceInfo, RgbaVideoFrame, ScreenCaptureConfig,
    SyntheticVideoSource, VideoCaptureSource, VideoCodecKind, VideoDecoder, VideoEncoder,
    VideoEncoderConfig, VideoFrame, VideoPixelFormat, bgra_to_nv12, bgra_to_rgba,
    create_h264_decoder, create_h264_encoder, i420_to_nv12, i420_to_rgba, list_cameras,
    list_displays, normalize_rotation, nv12_buffer_len, nv12_to_rgba, open_camera,
    open_screen_capture, resize_nv12, rgba_to_nv12, rotate_rgba,
};
#[cfg(windows)]
pub use video_mf::{
    DxgiScreenCapture, MfCameraCapture, MfH264Decoder, MfH264Encoder, dxgi_list_displays,
    mf_list_cameras,
};
pub use video_rtp::{
    ANNEX_B_START_CODE, AssembledH264Frame, DEFAULT_MAX_RTP_PAYLOAD, DepacketizeOutcome,
    H264RtpDepacketizer, H264RtpPacket, H264RtpPacketizer, NAL_TYPE_AUD, NAL_TYPE_FU_A,
    NAL_TYPE_IDR, NAL_TYPE_NON_IDR, NAL_TYPE_PPS, NAL_TYPE_SEI, NAL_TYPE_SPS, NAL_TYPE_STAP_A,
    join_annex_b_nalus, nal_unit_type, nalus_contain_keyframe, split_annex_b_nalus,
};

#[cfg(test)]
mod fuzz_untrusted;
