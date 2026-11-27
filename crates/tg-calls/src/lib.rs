//! `tg-calls`: Telegram 1:1 voice/video call and group call protocol core.
//!
//! Ported from Telegram Desktop (`Telegram/SourceFiles/calls/` and `data/data_group_call.*`).
//! This crate implements everything except the audio/video media engine itself using a
//! deterministic, Sans-IO design: callers feed TL updates, RPC responses, controller callbacks,
//! and timer ticks, and execute the emitted [`CallAction`] / [`InstanceAction`] /
//! [`GroupCallAction`] outputs.
//!
//! # Architecture
//! - [`call`]: 1:1 voice/video call state machine (`calls/calls_call.cpp`).
//! - [`instance`]: Top-level 1:1 call coordinator, DH cache, and incoming call filtering (`calls/calls_instance.cpp`).
//! - [`dh`]: Diffie-Hellman key exchange, `g_a_hash` commitment, and 64-bit `key_fingerprint`.
//! - [`emoji_fingerprint`]: Exact 333-emoji table and 4-emoji verification fingerprint (`calls/calls_emoji_fingerprint.cpp`).
//! - [`signal_bars`]: Call signal strength indicator state (`calls/calls_signal_bars.cpp`).
//! - [`protocol`]: Protocol layers (`min_layer`/`max_layer`), library versions, timeout defaults, and endpoint/RTC server conversion.
//! - [`signaling`]: MTProto 2.0-based signaling packet encryption for `tgcalls`.
//! - [`group`]: Group call / voice chat / livestream participants list, JSON join payloads, mute/raise-hand/volume state, speaking indicators, and video endpoints (`group/calls_group_call.cpp`, `data/data_group_call.cpp`).
//! - [`types`]: Call state machine input/output types using canonical IDs from `tg-data`.
//! - [`tl_conv`]: `From`/`TryFrom` conversions between generated `tg-tl` types and [`types`].

pub mod call;
pub mod dh;
pub mod emoji_fingerprint;
pub mod error;
pub mod group;
pub mod instance;
pub mod protocol;
pub mod signal_bars;
pub mod signaling;
pub mod tl_conv;
pub mod types;

pub use call::{
    Call, CallAction, CallErrorType, CallSound, CallState, CallTimerKind, CallType,
    ControllerDescriptor, ControllerState, RemoteAudioState, RemoteBatteryState, VideoState,
};
pub use dh::{
    DhConfig, DhConfigCache, RANDOM_POWER_SIZE, compute_ga_hash, compute_key_fingerprint,
    compute_key_sha_for_fingerprint, fill_auth_key_data, finish_dh_key_exchange,
    generate_mod_exp_first, verify_ga_hash,
};
pub use emoji_fingerprint::{
    EMOJI_COUNT, EMOJI_IN_FINGERPRINT, compute_call_emoji_fingerprint, compute_emoji_fingerprint,
    compute_emoji_fingerprint_indices, compute_emoji_index, emoji_by_index,
};
pub use error::{DhError, GroupCallError, SignalingError};
pub use group::e2e::{
    CallFailure, ChainBlock, ChainChange, ChainGroupParticipant, ChainGroupState, ChainSharedKey,
    ChainStateProof, ConferenceBlockchain, ConferenceEncryptDecrypt, DEFAULT_CALL_CHANNEL_ID,
    DEFAULT_PARTICIPANT_PERMISSIONS, DEFAULT_PARTICIPANT_VERSION, E2eAction, E2eCall, E2eError,
    E2eKeyPair, E2eTimerKind, GroupBroadcast, PERMISSION_ADD_USERS, PERMISSION_REMOVE_USERS,
    PERMISSION_SET_VALUE, PublicKey, SHORT_POLL_CHAIN_BLOCKS_PER_REQUEST,
    SHORT_POLL_CHAIN_BLOCKS_TIMEOUT_MS, SHORT_POLL_CHAIN_BLOCKS_WAIT_FOR_MS, SUBCHAIN_BROADCAST,
    SUBCHAIN_STATE, SUBCHAINS_COUNT, TemporaryKeyPair, build_create_conference_call,
    build_decline_conference_invite, build_delete_conference_participants, build_get_chain_blocks,
    build_get_last_chain_block, build_invite_conference_participant, build_join_conference_call,
    build_send_conference_broadcast, compute_conference_emoji_hash, conference_emoji_fingerprint,
    is_chain_invalid_error, is_permanent_broadcast_error,
};
pub use group::messages::{
    DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS, GroupCallChatMessage, GroupCallMessages,
    GroupMessageAction, MAX_SHOWN_VIDEO_STREAM_MESSAGES, MessageDeleteRequest, PaidLocalState,
    PreparedEncryptedMessage, STARS_STATS_SHORT_POLL_DELAY_MS, StarsColoring, StarsDonor, StarsTop,
    build_delete_group_call_messages, build_delete_group_call_participant_messages,
    build_get_group_call_stars, build_save_default_send_as,
    build_send_group_call_encrypted_message, build_send_group_call_message,
    deserialize_encrypted_message, entities_to_tl, serialize_encrypted_message,
    stars_coloring_for_count, stars_required_for_message,
};
pub use group::participant::{
    ApplySliceSource, GroupCallParticipant, LastSpokeTimes, ParticipantUpdate, ParticipantsList,
    ParticipantsListEvents, VersionedGroupCallUpdate,
};
pub use group::payload::{
    DtlsFingerprint, GroupJoinPayload, IceCandidateParam, JoinResponseMediaSection,
    JoinResponseTransport, JoinSsrcGroup, ParsedJoinResponse, RtcJoinResponse, RtmpInfo,
    RtpHeaderExtensionParam, RtpPayloadTypeParam, parse_join_response,
};
pub use group::state::{
    AudioLevelSample, GroupCallAction, GroupCallSession, GroupCallSound, GroupCallState,
    GroupCallTimerKind, InstanceMode, InstanceState, LevelUpdate, MuteState, SelfUpdateKind,
};
pub use group::video::{
    MediaSsrcGroup, ParticipantVideoParams, RequestedVideoTrackInput, VideoChannelDescription,
    VideoEndpoint, VideoEndpointType, VideoParams, VideoQuality, compute_requested_video_channels,
};
pub use instance::{CallsInstance, InstanceAction};
pub use protocol::{
    CallProtocolConfig, DEFAULT_LIBRARY_VERSIONS, DEFAULT_VERSION, Endpoint, EndpointHost,
    EndpointType, HANGUP_TIMEOUT_MS, MAX_LAYER, MIN_LAYER, RtcServer,
    SERVER_CONFIG_UPDATE_TIMEOUT_MS, ServerCallConfig, collect_endpoints_and_servers,
};
pub use signal_bars::{
    SIGNAL_BAR_COUNT, SIGNAL_BAR_FINISHED, SIGNAL_BAR_STARTING, SignalBarsState,
};
pub use signaling::{
    AUTH_KEY_SIZE, ChannelType, DecryptedSignalingPacket, KEEP_INCOMING_COUNTERS_COUNT,
    MAX_ALLOWED_COUNTER, MAX_INCOMING_PACKET_SIZE, MAX_OUTER_PACKET_SIZE,
    MAX_SIGNALING_PACKET_SIZE, MESSAGE_REQUIRES_ACK_SEQ_BIT, MIN_ENCRYPTED_PACKET_SIZE,
    MSG_KEY_SIZE, SINGLE_MESSAGE_PACKET_SEQ_BIT, SignalingCipher, aes_256_ctr_apply,
    compute_msg_key, counter_from_seq, decrypt_ctr_envelope, decrypt_raw_mtproto2,
    derive_aes_key_iv, encrypt_ctr_envelope, encrypt_raw_mtproto2, prepare_aes_key_iv,
};

#[cfg(test)]
mod fuzz_untrusted;
