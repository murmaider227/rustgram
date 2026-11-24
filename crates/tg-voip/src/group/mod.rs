//! Telegram group calls / voice chats / livestreams media engine.
//!
//! Port of `TelegramMessenger/tgcalls` (`GroupInstanceCustomImpl`) concepts:
//! - [`dtls_srtp`]: Pure-Rust DTLS 1.2 (`dimpl` `rust-crypto`) + SRTP/SRTCP (`webrtc-srtp`)
//!   with self-signed ECDSA P-256 X.509 identity and SHA-256 fingerprint verification.
//! - [`ice`]: `GroupJoinPayload` generator and ICE-lite STUN client for Telegram SFU UDP candidates.
//! - [`mixer`]: Multi-SSRC Opus receive, per-SSRC jitter buffer & volume, soft-clipped mixing,
//!   and speaking level updates (`tg_calls::AudioLevelSample`).
//! - [`broadcast`]: Large group call / RTMP chunked playback via [`BroadcastChunkLoader`]
//!   (`upload.getFile` `inputGroupCallStream`).
//! - [`engine`]: [`GroupMediaEngine`] integrating all layers for `tg_calls::GroupCallSession`.

pub mod broadcast;
pub mod dtls_srtp;
pub mod engine;
pub mod ice;
pub mod mixer;

pub use broadcast::{
    BROADCAST_STREAM_AUDIO_SSRC, BroadcastChunkLoader, BroadcastChunkRequest,
    BroadcastChunkResponse, BroadcastChunkScale, BroadcastChunkStatus, BroadcastStreamPlayer,
    encode_ogg_opus_chunk, extract_ogg_opus_packets,
};
pub use dtls_srtp::{
    DtlsIdentity, DtlsPollOutput, DtlsSrtpSession, WebRtcPacketKind, classify_webrtc_packet,
    is_rtcp_packet, sha256_fingerprint,
};
pub use engine::{
    GroupMediaConfig, GroupMediaEngine, GroupMediaEvent, active_group_media_task_count,
    transform_incoming_e2e_frame, transform_outgoing_e2e_frame,
};
pub use ice::{
    ICE_CHECK_RETRY_MS, ICE_CONSENT_INTERVAL_MS, IceLiteClient, IceStunOutcome,
    LocalIceCredentials, SfuUdpCandidate, build_group_join_payload, build_ice_binding_request,
    build_ice_binding_response, parse_sfu_udp_candidates, verify_stun_integrity_and_fingerprint,
};
pub use mixer::GroupAudioMixer;
