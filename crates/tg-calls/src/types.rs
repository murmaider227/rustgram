//! Domain input/output types for 1:1 calls and group calls (API layer 233).
//!
//! Uses canonical IDs from `tg-data` (`UserId`, `PeerId`, `CallId`, `MsgId`, `TimeId`)
//! and converts to/from generated `tg-tl` types in [`crate::tl_conv`].

use serde::{Deserialize, Serialize};

pub use tg_data::{CallId, ChannelId, ChatId, MsgId, PeerId, TimeId, UserId};

/// Unix timestamp in seconds (`TimeId` in `tg-data`, `int` in TL).
pub type UnixTime = TimeId;

/// `inputPhoneCall#1e36fded id:long access_hash:long = InputPhoneCall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputPhoneCall {
    /// Call ID.
    pub id: CallId,
    /// Access hash.
    pub access_hash: i64,
}

/// `PhoneCallDiscardReason` constructors (`schema/api.tl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhoneCallDiscardReason {
    /// `phoneCallDiscardReasonMissed#85e42301`.
    Missed,
    /// `phoneCallDiscardReasonDisconnect#e095c1a0`.
    Disconnect,
    /// `phoneCallDiscardReasonHangup#57adc690`.
    Hangup,
    /// `phoneCallDiscardReasonBusy#faf7e8c9`.
    Busy,
    /// `phoneCallDiscardReasonMigrateConferenceCall#9fbbf1f7 slug:string`.
    MigrateConferenceCall {
        /// Conference invite slug to join.
        slug: String,
    },
}

/// `phoneCallProtocol#fc878fc8`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneCallProtocol {
    /// Whether UDP peer-to-peer connections are supported.
    pub udp_p2p: bool,
    /// Whether UDP reflector connections are supported.
    pub udp_reflector: bool,
    /// Minimum supported VoIP protocol layer.
    pub min_layer: i32,
    /// Maximum supported VoIP protocol layer.
    pub max_layer: i32,
    /// Ordered list of supported `tgcalls` library versions (newest first in tdesktop).
    pub library_versions: Vec<String>,
}

/// `PhoneConnection` constructors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhoneConnection {
    /// `phoneConnection#9cc123c7` (Telegram UDP/TCP reflector).
    Standard {
        /// Whether this reflector uses TCP instead of UDP.
        tcp: bool,
        /// Unique connection/endpoint identifier.
        id: i64,
        /// IPv4 address.
        ip: String,
        /// IPv6 address.
        ipv6: String,
        /// Port number.
        port: u16,
        /// 16-byte reflector peer tag.
        peer_tag: Vec<u8>,
    },
    /// `phoneConnectionWebrtc#635fe375` (WebRTC STUN/TURN server).
    Webrtc {
        /// Whether this server supports TURN.
        turn: bool,
        /// Whether this server supports STUN.
        stun: bool,
        /// Connection identifier.
        id: i64,
        /// IPv4 address.
        ip: String,
        /// IPv6 address.
        ipv6: String,
        /// Port number.
        port: u16,
        /// TURN username.
        username: String,
        /// TURN password.
        password: String,
    },
}

/// `phoneCallWaiting#c5226f17`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCallWaiting {
    /// Whether this is a video call.
    pub video: bool,
    /// Call ID.
    pub id: CallId,
    /// Call access hash.
    pub access_hash: i64,
    /// Call creation Unix timestamp.
    pub date: UnixTime,
    /// Initiator user ID.
    pub admin_id: UserId,
    /// Callee user ID.
    pub participant_id: UserId,
    /// Call protocol.
    pub protocol: PhoneCallProtocol,
    /// Set once the callee has acknowledged receipt via `phone.receivedCall`.
    pub receive_date: Option<UnixTime>,
}

/// `phoneCallRequested#14b0ed0c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCallRequested {
    /// Whether this is a video call.
    pub video: bool,
    /// Call ID.
    pub id: CallId,
    /// Call access hash.
    pub access_hash: i64,
    /// Call creation Unix timestamp.
    pub date: UnixTime,
    /// Initiator user ID.
    pub admin_id: UserId,
    /// Callee user ID.
    pub participant_id: UserId,
    /// 32-byte `SHA256(g_a)` commitment from the caller.
    pub g_a_hash: Vec<u8>,
    /// Caller's supported protocol.
    pub protocol: PhoneCallProtocol,
}

/// `phoneCallAccepted#3660c311`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCallAccepted {
    /// Whether this is a video call.
    pub video: bool,
    /// Call ID.
    pub id: CallId,
    /// Call access hash.
    pub access_hash: i64,
    /// Call creation Unix timestamp.
    pub date: UnixTime,
    /// Initiator user ID.
    pub admin_id: UserId,
    /// Callee user ID.
    pub participant_id: UserId,
    /// Callee's Diffie-Hellman public value `g_b`.
    pub g_b: Vec<u8>,
    /// Callee's supported protocol.
    pub protocol: PhoneCallProtocol,
}

/// `phoneCall#30535af5`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCallActive {
    /// Whether P2P connections are allowed by privacy settings.
    pub p2p_allowed: bool,
    /// Whether this is a video call.
    pub video: bool,
    /// Whether migrating this 1:1 call to a conference call is supported.
    pub conference_supported: bool,
    /// Call ID.
    pub id: CallId,
    /// Call access hash.
    pub access_hash: i64,
    /// Call creation Unix timestamp.
    pub date: UnixTime,
    /// Initiator user ID.
    pub admin_id: UserId,
    /// Callee user ID.
    pub participant_id: UserId,
    /// `g_a` (for callee) or `g_b` (for caller).
    pub g_a_or_b: Vec<u8>,
    /// Lower 64 bits of `SHA1(auth_key)`.
    pub key_fingerprint: u64,
    /// Negotiated protocol (selected version is `protocol.library_versions[0]`).
    pub protocol: PhoneCallProtocol,
    /// Reflector and WebRTC STUN/TURN connection endpoints.
    pub connections: Vec<PhoneConnection>,
    /// Call start Unix timestamp.
    pub start_date: UnixTime,
    /// Optional server-provided JSON custom parameters.
    pub custom_parameters: Option<String>,
}

/// `phoneCallDiscarded#50ca4de1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCallDiscarded {
    /// Whether the server requests a user call quality rating.
    pub need_rating: bool,
    /// Whether the server requests uploading call debug logs.
    pub need_debug: bool,
    /// Whether the call involved video.
    pub video: bool,
    /// Call ID.
    pub id: CallId,
    /// Discard reason if provided.
    pub reason: Option<PhoneCallDiscardReason>,
    /// Call duration in seconds if provided.
    pub duration: Option<i32>,
}

/// `PhoneCall` boxed union (`updatePhoneCall.phone_call` and `phone.PhoneCall.phone_call`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneCallUpdate {
    /// `phoneCallEmpty#5366c915`.
    Empty {
        /// Call ID.
        id: CallId,
    },
    /// `phoneCallWaiting#c5226f17`.
    Waiting(PhoneCallWaiting),
    /// `phoneCallRequested#14b0ed0c`.
    Requested(PhoneCallRequested),
    /// `phoneCallAccepted#3660c311`.
    Accepted(PhoneCallAccepted),
    /// `phoneCall#30535af5`.
    Active(PhoneCallActive),
    /// `phoneCallDiscarded#50ca4de1`.
    Discarded(PhoneCallDiscarded),
}

/// `updatePhoneCallSignalingData#2661bf09`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePhoneCallSignalingData {
    /// Call ID this signaling payload belongs to.
    pub phone_call_id: CallId,
    /// Encrypted signaling payload bytes.
    pub data: Vec<u8>,
}

/// `messages.DhConfig` union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DhConfigResponse {
    /// `messages.dhConfig#2c221edd`.
    Config {
        /// Generator `g`.
        g: i32,
        /// 2048-bit safe prime `p` (big-endian bytes).
        p: Vec<u8>,
        /// Config version.
        version: i32,
        /// 256 bytes of server-provided random entropy.
        random: Vec<u8>,
    },
    /// `messages.dhConfigNotModified#c0e24635`.
    NotModified {
        /// 256 bytes of server-provided random entropy.
        random: Vec<u8>,
    },
}

/// `InputGroupCall` union.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InputGroupCall {
    /// `inputGroupCall#d8aa840f`.
    Id {
        /// Group call ID.
        id: CallId,
        /// Access hash.
        access_hash: i64,
    },
    /// `inputGroupCallSlug#fe06823f`.
    Slug {
        /// Conference link slug.
        slug: String,
    },
    /// `inputGroupCallInviteMessage#8c10603f`.
    InviteMessage {
        /// Invite message ID.
        msg_id: MsgId,
    },
}

/// `groupCallParticipantVideoSourceGroup#dcb118b7`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallParticipantVideoSourceGroup {
    /// SSRC group semantics (e.g. `"SIM"`, `"FID"`).
    pub semantics: String,
    /// List of SSRCs belonging to this group.
    pub sources: Vec<u32>,
}

/// `groupCallParticipantVideo#67753ac8`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallParticipantVideo {
    /// Whether the video stream is paused.
    pub paused: bool,
    /// Video endpoint ID string.
    pub endpoint: String,
    /// SSRC groups for simulcast/RTX.
    pub source_groups: Vec<GroupCallParticipantVideoSourceGroup>,
    /// Optional additional audio SSRC (used for screen-sharing with system audio).
    pub audio_source: Option<u32>,
}

/// `groupCallParticipant#2a3dc7ac`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallParticipantData {
    /// Whether the participant is muted.
    pub muted: bool,
    /// Whether the participant has left the call.
    pub left: bool,
    /// Whether the participant is allowed to unmute themselves.
    pub can_self_unmute: bool,
    /// Whether the participant just joined in this update.
    pub just_joined: bool,
    /// Whether this participant update increments the group call version.
    pub versioned: bool,
    /// Whether this is a minimal update (lacks per-viewer fields like `muted_by_you`).
    pub min: bool,
    /// Whether this participant is muted locally by the current user.
    pub muted_by_you: bool,
    /// Whether volume was set by an admin for everyone.
    pub volume_by_admin: bool,
    /// Whether this participant represents the current user.
    pub is_self: bool,
    /// Whether video is active/joined for this participant.
    pub video_joined: bool,
    /// Participant peer ID.
    pub peer: PeerId,
    /// Join Unix timestamp.
    pub date: UnixTime,
    /// Last active Unix timestamp.
    pub active_date: Option<UnixTime>,
    /// Primary audio SSRC (`source` in TL).
    pub source: u32,
    /// Volume in `1..=20000` (10000 = 100%).
    pub volume: Option<i32>,
    /// Optional bio/about text.
    pub about: Option<String>,
    /// Non-zero monotonically increasing rating when hand is raised.
    pub raise_hand_rating: Option<u64>,
    /// Camera video stream info.
    pub video: Option<GroupCallParticipantVideo>,
    /// Screen-share presentation video stream info.
    pub presentation: Option<GroupCallParticipantVideo>,
}

/// `groupCall#efb2b617`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallInfo {
    /// Call ID.
    pub id: CallId,
    /// Access hash.
    pub access_hash: i64,
    /// Server participant count.
    pub participants_count: i32,
    /// Optional title.
    pub title: Option<String>,
    /// Stream DC ID for broadcast/RTMP mode.
    pub stream_dc_id: Option<i32>,
    /// Recording start date (0 if not recording).
    pub record_start_date: Option<UnixTime>,
    /// Scheduled start date (0 if active).
    pub schedule_date: Option<UnixTime>,
    /// Maximum concurrent video senders allowed.
    pub unmuted_video_limit: i32,
    /// Monotonically increasing state version.
    pub version: i32,
    /// Whether new joiners start muted.
    pub join_muted: bool,
    /// Whether admins can toggle `join_muted`.
    pub can_change_join_muted: bool,
    /// Whether participants are ordered ascending by join date.
    pub join_date_asc: bool,
    /// Whether current user subscribed to scheduled start notification.
    pub schedule_start_subscribed: bool,
    /// Whether video recording is active.
    pub record_video_active: bool,
    /// Whether this is an RTMP livestream.
    pub rtmp_stream: bool,
    /// Whether listeners are hidden from the participant list.
    pub listeners_hidden: bool,
    /// Whether this is an E2E conference call.
    pub conference: bool,
    /// Whether current user is the creator.
    pub creator: bool,
    /// Whether in-call messages are enabled.
    pub messages_enabled: bool,
    /// Whether admins can toggle in-call messages.
    pub can_change_messages_enabled: bool,
    /// Conference invite link, if any.
    pub invite_link: Option<String>,
    /// Paid messages minimum stars.
    pub send_paid_messages_stars: Option<i64>,
}

/// `groupCallDiscarded#7780bcb4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupCallDiscarded {
    /// Call ID.
    pub id: CallId,
    /// Access hash.
    pub access_hash: i64,
    /// Call duration in seconds.
    pub duration: i32,
}

/// `GroupCall` boxed union (`groupCall` or `groupCallDiscarded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupCallObject {
    /// Active group call metadata.
    Active(GroupCallInfo),
    /// Discarded/ended group call.
    Discarded(GroupCallDiscarded),
}

/// `updateGroupCallParticipants#f2ebdb4e`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateGroupCallParticipants {
    /// Target group call ID.
    pub call_id: CallId,
    /// Participant delta list.
    pub participants: Vec<GroupCallParticipantData>,
    /// Group call version associated with this update.
    pub version: i32,
}

/// `updateGroupCallConnection#b783982`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateGroupCallConnection {
    /// True if these params are for the screen-share presentation connection.
    pub presentation: bool,
    /// Raw JSON parameters string (`DataJSON.data`).
    pub params_json: String,
}

/// `updateGroupCallChainBlocks#a477288f`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateGroupCallChainBlocks {
    /// Target conference call ID.
    pub call_id: CallId,
    /// Subchain ID (`0` = state chain, `1` = verification broadcasts).
    pub sub_chain_id: i32,
    /// Raw TL-serialized E2E blocks or broadcasts.
    pub blocks: Vec<Vec<u8>>,
    /// Index immediately after the last block in `blocks`.
    pub next_offset: i32,
}

/// Supported inline formatting entities in group call / conference messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupCallMessageEntity {
    /// Bold text range (UTF-16 offset & length).
    Bold {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
    },
    /// Italic text range.
    Italic {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
    },
    /// Underlined text range.
    Underline {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
    },
    /// Strikethrough text range.
    Strike {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
    },
    /// Spoiler text range.
    Spoiler {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
    },
    /// Inline code text range.
    Code {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
    },
    /// Custom emoji entity.
    CustomEmoji {
        /// UTF-16 code-unit offset.
        offset: i32,
        /// UTF-16 code-unit length.
        length: i32,
        /// Custom emoji Telegram document ID.
        document_id: i64,
    },
}

/// `groupCallMessage#1a8afc7e`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallMessageData {
    /// Whether sent by a call admin.
    pub from_admin: bool,
    /// Server message ID.
    pub id: i32,
    /// Author peer ID.
    pub from_id: PeerId,
    /// Unix send timestamp.
    pub date: UnixTime,
    /// Message text.
    pub text: String,
    /// Filtered inline entities.
    pub entities: Vec<GroupCallMessageEntity>,
    /// Paid Stars attached to the message, if any.
    pub paid_message_stars: Option<i64>,
}

/// `updateGroupCallMessage#d8326f0d`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateGroupCallMessage {
    /// Target group call ID.
    pub call_id: CallId,
    /// Message payload.
    pub message: GroupCallMessageData,
}

/// `updateGroupCallEncryptedMessage#c957a766`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateGroupCallEncryptedMessage {
    /// Target conference call ID.
    pub call_id: CallId,
    /// Sender peer ID.
    pub from_id: PeerId,
    /// E2E encrypted message payload bytes.
    pub encrypted_message: Vec<u8>,
}

/// `updateDeleteGroupCallMessages#3e85e92c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateDeleteGroupCallMessages {
    /// Target group call ID.
    pub call_id: CallId,
    /// Deleted message IDs.
    pub messages: Vec<i32>,
}

/// `groupCallDonor#ee430c85`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallDonorData {
    /// Whether this donor is in the top list.
    pub top: bool,
    /// Whether this donor is the current user.
    pub my: bool,
    /// Donor peer ID (None if anonymous).
    pub peer_id: Option<PeerId>,
    /// Total Stars donated.
    pub stars: i64,
}

/// `phone.groupCallStars#9d1dbd26`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhoneGroupCallStarsResponse {
    /// Total Stars sent to the call/stream.
    pub total_stars: i64,
    /// Top donors list.
    pub top_donors: Vec<GroupCallDonorData>,
}

/// `groupCallStreamChannel#80eb48af`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupCallStreamChannel {
    /// Channel identifier.
    pub channel: i32,
    /// Broadcast timestamp scale (`period_ms = 1000 >> scale`).
    pub scale: i32,
    /// Latest available broadcast chunk timestamp in milliseconds.
    pub last_timestamp_ms: i64,
}
