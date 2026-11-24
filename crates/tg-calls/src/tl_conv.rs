//! Conversions between generated `tg-tl` types and `tg-calls` domain input/output types.
//!
//! Bridges `tg_tl::enums::*` / `tg_tl::types::*` (`PhoneCall`, `PhoneConnection`,
//! `PhoneCallProtocol`, `PhoneCallDiscardReason`, `InputPhoneCall`, `GroupCall`,
//! `InputGroupCall`, `GroupCallParticipant*`, `messages::DhConfig`, `phone::PhoneCall`,
//! `phone::GroupCall`, `UpdatePhoneCallSignalingData`, `UpdateGroupCallParticipants`,
//! `UpdateGroupCallConnection`) with [`crate::types`].

use thiserror::Error;

use crate::types::{
    CallId, ChannelId, ChatId, DhConfigResponse, GroupCallDiscarded, GroupCallDonorData,
    GroupCallInfo, GroupCallMessageData, GroupCallMessageEntity, GroupCallObject,
    GroupCallParticipantData, GroupCallParticipantVideo, GroupCallParticipantVideoSourceGroup,
    GroupCallStreamChannel, InputGroupCall, InputPhoneCall, MsgId, PeerId, PhoneCallAccepted,
    PhoneCallActive, PhoneCallDiscardReason, PhoneCallDiscarded, PhoneCallProtocol,
    PhoneCallRequested, PhoneCallUpdate, PhoneCallWaiting, PhoneConnection,
    PhoneGroupCallStarsResponse, UpdateDeleteGroupCallMessages, UpdateGroupCallChainBlocks,
    UpdateGroupCallConnection, UpdateGroupCallEncryptedMessage, UpdateGroupCallMessage,
    UpdateGroupCallParticipants, UpdatePhoneCallSignalingData, UserId,
};

/// Error returned when a `tg-tl` value cannot be losslessly converted into a `tg-calls` type.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TlConversionError {
    /// A TL `port: int` was outside the valid `u16` range (`0..=65535`).
    #[error("invalid TCP/UDP port in PhoneConnection: {0}")]
    InvalidPort(i32),
    /// A `PeerId` could not be mapped to a TL `Peer` (e.g. zero or fake peer ID).
    #[error("cannot convert PeerId({0}) to TL Peer")]
    UnsupportedPeerId(i64),
    /// A `MsgId` was outside the 32-bit TL `int` range.
    #[error("MsgId({0}) out of i32 range for TL InputGroupCallInviteMessage")]
    MsgIdOutOfRange(i64),
    /// `updateGroupCallParticipants.call` was not an `inputGroupCall` with a numeric `id`.
    #[error("updateGroupCallParticipants requires an inputGroupCall with a numeric call ID")]
    MissingGroupCallId,
}

/// Convert a TL `Peer` into a `tg-data` [`PeerId`].
#[must_use]
pub const fn peer_from_tl(peer: &tg_tl::enums::Peer) -> PeerId {
    match peer {
        tg_tl::enums::Peer::User(u) => PeerId::from_user(UserId::from_tl(u.user_id)),
        tg_tl::enums::Peer::Chat(c) => PeerId::from_chat(ChatId::from_tl(c.chat_id)),
        tg_tl::enums::Peer::Channel(ch) => PeerId::from_channel(ChannelId::from_tl(ch.channel_id)),
    }
}

/// Convert a `tg-data` [`PeerId`] into a TL `Peer`.
pub fn peer_to_tl(peer: PeerId) -> Result<tg_tl::enums::Peer, TlConversionError> {
    if peer.is_zero() {
        return Err(TlConversionError::UnsupportedPeerId(peer.0));
    }
    if let Some(u) = peer.to_user() {
        Ok(tg_tl::enums::Peer::User(tg_tl::types::PeerUser {
            user_id: u.to_tl(),
        }))
    } else if let Some(c) = peer.to_chat() {
        Ok(tg_tl::enums::Peer::Chat(tg_tl::types::PeerChat {
            chat_id: c.to_tl(),
        }))
    } else if let Some(ch) = peer.to_channel() {
        Ok(tg_tl::enums::Peer::Channel(tg_tl::types::PeerChannel {
            channel_id: ch.to_tl(),
        }))
    } else {
        Err(TlConversionError::UnsupportedPeerId(peer.0))
    }
}

// ---------------------------------------------------------------------------
// InputPhoneCall
// ---------------------------------------------------------------------------

impl From<tg_tl::types::InputPhoneCall> for InputPhoneCall {
    fn from(tl: tg_tl::types::InputPhoneCall) -> Self {
        Self {
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
        }
    }
}

impl From<tg_tl::enums::InputPhoneCall> for InputPhoneCall {
    fn from(tl: tg_tl::enums::InputPhoneCall) -> Self {
        match tl {
            tg_tl::enums::InputPhoneCall::InputPhoneCall(inner) => inner.into(),
        }
    }
}

impl From<InputPhoneCall> for tg_tl::types::InputPhoneCall {
    fn from(val: InputPhoneCall) -> Self {
        Self {
            id: val.id.to_tl(),
            access_hash: val.access_hash,
        }
    }
}

impl From<InputPhoneCall> for tg_tl::enums::InputPhoneCall {
    fn from(val: InputPhoneCall) -> Self {
        Self::InputPhoneCall(val.into())
    }
}

// ---------------------------------------------------------------------------
// PhoneCallDiscardReason
// ---------------------------------------------------------------------------

impl From<tg_tl::enums::PhoneCallDiscardReason> for PhoneCallDiscardReason {
    fn from(tl: tg_tl::enums::PhoneCallDiscardReason) -> Self {
        match tl {
            tg_tl::enums::PhoneCallDiscardReason::Missed => Self::Missed,
            tg_tl::enums::PhoneCallDiscardReason::Disconnect => Self::Disconnect,
            tg_tl::enums::PhoneCallDiscardReason::Hangup => Self::Hangup,
            tg_tl::enums::PhoneCallDiscardReason::Busy => Self::Busy,
            tg_tl::enums::PhoneCallDiscardReason::MigrateConferenceCall(m) => {
                Self::MigrateConferenceCall { slug: m.slug }
            }
        }
    }
}

impl From<PhoneCallDiscardReason> for tg_tl::enums::PhoneCallDiscardReason {
    fn from(val: PhoneCallDiscardReason) -> Self {
        match val {
            PhoneCallDiscardReason::Missed => Self::Missed,
            PhoneCallDiscardReason::Disconnect => Self::Disconnect,
            PhoneCallDiscardReason::Hangup => Self::Hangup,
            PhoneCallDiscardReason::Busy => Self::Busy,
            PhoneCallDiscardReason::MigrateConferenceCall { slug } => Self::MigrateConferenceCall(
                tg_tl::types::PhoneCallDiscardReasonMigrateConferenceCall { slug },
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// PhoneCallProtocol
// ---------------------------------------------------------------------------

impl From<tg_tl::types::PhoneCallProtocol> for PhoneCallProtocol {
    fn from(tl: tg_tl::types::PhoneCallProtocol) -> Self {
        Self {
            udp_p2p: tl.udp_p2p,
            udp_reflector: tl.udp_reflector,
            min_layer: tl.min_layer,
            max_layer: tl.max_layer,
            library_versions: tl.library_versions,
        }
    }
}

impl From<tg_tl::enums::PhoneCallProtocol> for PhoneCallProtocol {
    fn from(tl: tg_tl::enums::PhoneCallProtocol) -> Self {
        match tl {
            tg_tl::enums::PhoneCallProtocol::PhoneCallProtocol(inner) => inner.into(),
        }
    }
}

impl From<PhoneCallProtocol> for tg_tl::types::PhoneCallProtocol {
    fn from(val: PhoneCallProtocol) -> Self {
        Self {
            udp_p2p: val.udp_p2p,
            udp_reflector: val.udp_reflector,
            min_layer: val.min_layer,
            max_layer: val.max_layer,
            library_versions: val.library_versions,
        }
    }
}

impl From<PhoneCallProtocol> for tg_tl::enums::PhoneCallProtocol {
    fn from(val: PhoneCallProtocol) -> Self {
        Self::PhoneCallProtocol(val.into())
    }
}

// ---------------------------------------------------------------------------
// PhoneConnection
// ---------------------------------------------------------------------------

impl TryFrom<tg_tl::enums::PhoneConnection> for PhoneConnection {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::enums::PhoneConnection) -> Result<Self, Self::Error> {
        match tl {
            tg_tl::enums::PhoneConnection::PhoneConnection(c) => {
                let port =
                    u16::try_from(c.port).map_err(|_| TlConversionError::InvalidPort(c.port))?;
                Ok(Self::Standard {
                    tcp: c.tcp,
                    id: c.id,
                    ip: c.ip,
                    ipv6: c.ipv6,
                    port,
                    peer_tag: c.peer_tag,
                })
            }
            tg_tl::enums::PhoneConnection::Webrtc(c) => {
                let port =
                    u16::try_from(c.port).map_err(|_| TlConversionError::InvalidPort(c.port))?;
                Ok(Self::Webrtc {
                    turn: c.turn,
                    stun: c.stun,
                    id: c.id,
                    ip: c.ip,
                    ipv6: c.ipv6,
                    port,
                    username: c.username,
                    password: c.password,
                })
            }
        }
    }
}

impl From<PhoneConnection> for tg_tl::enums::PhoneConnection {
    fn from(val: PhoneConnection) -> Self {
        match val {
            PhoneConnection::Standard {
                tcp,
                id,
                ip,
                ipv6,
                port,
                peer_tag,
            } => Self::PhoneConnection(tg_tl::types::PhoneConnection {
                tcp,
                id,
                ip,
                ipv6,
                port: i32::from(port),
                peer_tag,
            }),
            PhoneConnection::Webrtc {
                turn,
                stun,
                id,
                ip,
                ipv6,
                port,
                username,
                password,
            } => Self::Webrtc(tg_tl::types::PhoneConnectionWebrtc {
                turn,
                stun,
                id,
                ip,
                ipv6,
                port: i32::from(port),
                username,
                password,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// PhoneCall variants & phone.PhoneCall
// ---------------------------------------------------------------------------

impl From<tg_tl::types::PhoneCallWaiting> for PhoneCallWaiting {
    fn from(tl: tg_tl::types::PhoneCallWaiting) -> Self {
        Self {
            video: tl.video,
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
            date: tl.date,
            admin_id: UserId::from_tl(tl.admin_id),
            participant_id: UserId::from_tl(tl.participant_id),
            protocol: tl.protocol.into(),
            receive_date: tl.receive_date,
        }
    }
}

impl From<tg_tl::types::PhoneCallRequested> for PhoneCallRequested {
    fn from(tl: tg_tl::types::PhoneCallRequested) -> Self {
        Self {
            video: tl.video,
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
            date: tl.date,
            admin_id: UserId::from_tl(tl.admin_id),
            participant_id: UserId::from_tl(tl.participant_id),
            g_a_hash: tl.g_a_hash,
            protocol: tl.protocol.into(),
        }
    }
}

impl From<tg_tl::types::PhoneCallAccepted> for PhoneCallAccepted {
    fn from(tl: tg_tl::types::PhoneCallAccepted) -> Self {
        Self {
            video: tl.video,
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
            date: tl.date,
            admin_id: UserId::from_tl(tl.admin_id),
            participant_id: UserId::from_tl(tl.participant_id),
            g_b: tl.g_b,
            protocol: tl.protocol.into(),
        }
    }
}

impl TryFrom<tg_tl::types::PhoneCall> for PhoneCallActive {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::PhoneCall) -> Result<Self, Self::Error> {
        let connections = tl
            .connections
            .into_iter()
            .map(PhoneConnection::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let custom_parameters = tl.custom_parameters.map(|dj| match dj {
            tg_tl::enums::DataJSON::DataJSON(d) => d.data,
        });
        Ok(Self {
            p2p_allowed: tl.p2p_allowed,
            video: tl.video,
            conference_supported: tl.conference_supported,
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
            date: tl.date,
            admin_id: UserId::from_tl(tl.admin_id),
            participant_id: UserId::from_tl(tl.participant_id),
            g_a_or_b: tl.g_a_or_b,
            key_fingerprint: tl.key_fingerprint as u64,
            protocol: tl.protocol.into(),
            connections,
            start_date: tl.start_date,
            custom_parameters,
        })
    }
}

impl From<tg_tl::types::PhoneCallDiscarded> for PhoneCallDiscarded {
    fn from(tl: tg_tl::types::PhoneCallDiscarded) -> Self {
        Self {
            need_rating: tl.need_rating,
            need_debug: tl.need_debug,
            video: tl.video,
            id: CallId::from_tl(tl.id),
            reason: tl.reason.map(Into::into),
            duration: tl.duration,
        }
    }
}

impl TryFrom<tg_tl::enums::PhoneCall> for PhoneCallUpdate {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::enums::PhoneCall) -> Result<Self, Self::Error> {
        Ok(match tl {
            tg_tl::enums::PhoneCall::Empty(e) => Self::Empty {
                id: CallId::from_tl(e.id),
            },
            tg_tl::enums::PhoneCall::Waiting(w) => Self::Waiting(w.into()),
            tg_tl::enums::PhoneCall::Requested(r) => Self::Requested(r.into()),
            tg_tl::enums::PhoneCall::Accepted(a) => Self::Accepted(a.into()),
            tg_tl::enums::PhoneCall::PhoneCall(c) => Self::Active(c.try_into()?),
            tg_tl::enums::PhoneCall::Discarded(d) => Self::Discarded(d.into()),
        })
    }
}

impl TryFrom<tg_tl::types::phone::PhoneCall> for PhoneCallUpdate {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::phone::PhoneCall) -> Result<Self, Self::Error> {
        tl.phone_call.try_into()
    }
}

impl TryFrom<tg_tl::enums::phone::PhoneCall> for PhoneCallUpdate {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::enums::phone::PhoneCall) -> Result<Self, Self::Error> {
        match tl {
            tg_tl::enums::phone::PhoneCall::PhoneCall(inner) => inner.try_into(),
        }
    }
}

impl From<tg_tl::types::UpdatePhoneCallSignalingData> for UpdatePhoneCallSignalingData {
    fn from(tl: tg_tl::types::UpdatePhoneCallSignalingData) -> Self {
        Self {
            phone_call_id: CallId::from_tl(tl.phone_call_id),
            data: tl.data,
        }
    }
}

impl From<UpdatePhoneCallSignalingData> for tg_tl::types::UpdatePhoneCallSignalingData {
    fn from(val: UpdatePhoneCallSignalingData) -> Self {
        Self {
            phone_call_id: val.phone_call_id.to_tl(),
            data: val.data,
        }
    }
}

// ---------------------------------------------------------------------------
// messages.DhConfig
// ---------------------------------------------------------------------------

impl From<tg_tl::enums::messages::DhConfig> for DhConfigResponse {
    fn from(tl: tg_tl::enums::messages::DhConfig) -> Self {
        match tl {
            tg_tl::enums::messages::DhConfig::DhConfig(cfg) => Self::Config {
                g: cfg.g,
                p: cfg.p,
                version: cfg.version,
                random: cfg.random,
            },
            tg_tl::enums::messages::DhConfig::NotModified(nm) => {
                Self::NotModified { random: nm.random }
            }
        }
    }
}

impl From<DhConfigResponse> for tg_tl::enums::messages::DhConfig {
    fn from(val: DhConfigResponse) -> Self {
        match val {
            DhConfigResponse::Config {
                g,
                p,
                version,
                random,
            } => Self::DhConfig(tg_tl::types::messages::DhConfig {
                g,
                p,
                version,
                random,
            }),
            DhConfigResponse::NotModified { random } => {
                Self::NotModified(tg_tl::types::messages::DhConfigNotModified { random })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// GroupCall / InputGroupCall / GroupCallParticipant* / phone.GroupCall
// ---------------------------------------------------------------------------

impl From<tg_tl::enums::InputGroupCall> for InputGroupCall {
    fn from(tl: tg_tl::enums::InputGroupCall) -> Self {
        match tl {
            tg_tl::enums::InputGroupCall::InputGroupCall(c) => Self::Id {
                id: CallId::from_tl(c.id),
                access_hash: c.access_hash,
            },
            tg_tl::enums::InputGroupCall::Slug(s) => Self::Slug { slug: s.slug },
            tg_tl::enums::InputGroupCall::InviteMessage(m) => Self::InviteMessage {
                msg_id: MsgId::from(m.msg_id),
            },
        }
    }
}

impl TryFrom<InputGroupCall> for tg_tl::enums::InputGroupCall {
    type Error = TlConversionError;

    fn try_from(val: InputGroupCall) -> Result<Self, Self::Error> {
        Ok(match val {
            InputGroupCall::Id { id, access_hash } => {
                Self::InputGroupCall(tg_tl::types::InputGroupCall {
                    id: id.to_tl(),
                    access_hash,
                })
            }
            InputGroupCall::Slug { slug } => Self::Slug(tg_tl::types::InputGroupCallSlug { slug }),
            InputGroupCall::InviteMessage { msg_id } => {
                let raw = msg_id
                    .as_i32()
                    .ok_or(TlConversionError::MsgIdOutOfRange(msg_id.0))?;
                Self::InviteMessage(tg_tl::types::InputGroupCallInviteMessage { msg_id: raw })
            }
        })
    }
}

impl From<tg_tl::types::GroupCallParticipantVideoSourceGroup>
    for GroupCallParticipantVideoSourceGroup
{
    fn from(tl: tg_tl::types::GroupCallParticipantVideoSourceGroup) -> Self {
        Self {
            semantics: tl.semantics,
            sources: tl.sources.into_iter().map(|s| s as u32).collect(),
        }
    }
}

impl From<tg_tl::enums::GroupCallParticipantVideoSourceGroup>
    for GroupCallParticipantVideoSourceGroup
{
    fn from(tl: tg_tl::enums::GroupCallParticipantVideoSourceGroup) -> Self {
        match tl {
            tg_tl::enums::GroupCallParticipantVideoSourceGroup::GroupCallParticipantVideoSourceGroup(
                inner,
            ) => inner.into(),
        }
    }
}

impl From<GroupCallParticipantVideoSourceGroup>
    for tg_tl::types::GroupCallParticipantVideoSourceGroup
{
    fn from(val: GroupCallParticipantVideoSourceGroup) -> Self {
        Self {
            semantics: val.semantics,
            sources: val.sources.into_iter().map(|s| s as i32).collect(),
        }
    }
}

impl From<tg_tl::types::GroupCallParticipantVideo> for GroupCallParticipantVideo {
    fn from(tl: tg_tl::types::GroupCallParticipantVideo) -> Self {
        Self {
            paused: tl.paused,
            endpoint: tl.endpoint,
            source_groups: tl.source_groups.into_iter().map(Into::into).collect(),
            audio_source: tl.audio_source.map(|s| s as u32),
        }
    }
}

impl From<tg_tl::enums::GroupCallParticipantVideo> for GroupCallParticipantVideo {
    fn from(tl: tg_tl::enums::GroupCallParticipantVideo) -> Self {
        match tl {
            tg_tl::enums::GroupCallParticipantVideo::GroupCallParticipantVideo(inner) => {
                inner.into()
            }
        }
    }
}

impl From<GroupCallParticipantVideo> for tg_tl::types::GroupCallParticipantVideo {
    fn from(val: GroupCallParticipantVideo) -> Self {
        Self {
            paused: val.paused,
            endpoint: val.endpoint,
            source_groups: val
                .source_groups
                .into_iter()
                .map(|sg| {
                    tg_tl::enums::GroupCallParticipantVideoSourceGroup::GroupCallParticipantVideoSourceGroup(
                        sg.into(),
                    )
                })
                .collect(),
            audio_source: val.audio_source.map(|s| s as i32),
        }
    }
}

impl From<tg_tl::types::GroupCallParticipant> for GroupCallParticipantData {
    fn from(tl: tg_tl::types::GroupCallParticipant) -> Self {
        Self {
            muted: tl.muted,
            left: tl.left,
            can_self_unmute: tl.can_self_unmute,
            just_joined: tl.just_joined,
            versioned: tl.versioned,
            min: tl.min,
            muted_by_you: tl.muted_by_you,
            volume_by_admin: tl.volume_by_admin,
            is_self: tl.self_,
            video_joined: tl.video_joined,
            peer: peer_from_tl(&tl.peer),
            date: tl.date,
            active_date: tl.active_date,
            source: tl.source as u32,
            volume: tl.volume,
            about: tl.about,
            raise_hand_rating: tl.raise_hand_rating.map(|r| r as u64),
            video: tl.video.map(Into::into),
            presentation: tl.presentation.map(Into::into),
        }
    }
}

impl From<tg_tl::enums::GroupCallParticipant> for GroupCallParticipantData {
    fn from(tl: tg_tl::enums::GroupCallParticipant) -> Self {
        match tl {
            tg_tl::enums::GroupCallParticipant::GroupCallParticipant(inner) => inner.into(),
        }
    }
}

impl From<tg_tl::types::GroupCall> for GroupCallInfo {
    fn from(tl: tg_tl::types::GroupCall) -> Self {
        Self {
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
            participants_count: tl.participants_count,
            title: tl.title,
            stream_dc_id: tl.stream_dc_id,
            record_start_date: tl.record_start_date,
            schedule_date: tl.schedule_date,
            unmuted_video_limit: tl.unmuted_video_limit,
            version: tl.version,
            join_muted: tl.join_muted,
            can_change_join_muted: tl.can_change_join_muted,
            join_date_asc: tl.join_date_asc,
            schedule_start_subscribed: tl.schedule_start_subscribed,
            record_video_active: tl.record_video_active,
            rtmp_stream: tl.rtmp_stream,
            listeners_hidden: tl.listeners_hidden,
            conference: tl.conference,
            creator: tl.creator,
            messages_enabled: tl.messages_enabled,
            can_change_messages_enabled: tl.can_change_messages_enabled,
            invite_link: tl.invite_link,
            send_paid_messages_stars: tl.send_paid_messages_stars,
        }
    }
}

impl From<tg_tl::types::GroupCallDiscarded> for GroupCallDiscarded {
    fn from(tl: tg_tl::types::GroupCallDiscarded) -> Self {
        Self {
            id: CallId::from_tl(tl.id),
            access_hash: tl.access_hash,
            duration: tl.duration,
        }
    }
}

impl From<tg_tl::enums::GroupCall> for GroupCallObject {
    fn from(tl: tg_tl::enums::GroupCall) -> Self {
        match tl {
            tg_tl::enums::GroupCall::GroupCall(info) => Self::Active(info.into()),
            tg_tl::enums::GroupCall::Discarded(disc) => Self::Discarded(disc.into()),
        }
    }
}

/// Decoded `phone.groupCall` RPC response payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneGroupCallResponse {
    /// Group call metadata (`groupCall` or `groupCallDiscarded`).
    pub call: GroupCallObject,
    /// Initial participant slice.
    pub participants: Vec<GroupCallParticipantData>,
    /// Pagination offset for `phone.getGroupParticipants`.
    pub participants_next_offset: String,
}

impl From<tg_tl::types::phone::GroupCall> for PhoneGroupCallResponse {
    fn from(tl: tg_tl::types::phone::GroupCall) -> Self {
        Self {
            call: tl.call.into(),
            participants: tl.participants.into_iter().map(Into::into).collect(),
            participants_next_offset: tl.participants_next_offset,
        }
    }
}

impl From<tg_tl::enums::phone::GroupCall> for PhoneGroupCallResponse {
    fn from(tl: tg_tl::enums::phone::GroupCall) -> Self {
        match tl {
            tg_tl::enums::phone::GroupCall::GroupCall(inner) => inner.into(),
        }
    }
}

impl TryFrom<tg_tl::types::UpdateGroupCallParticipants> for UpdateGroupCallParticipants {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::UpdateGroupCallParticipants) -> Result<Self, Self::Error> {
        let call_id = match tl.call {
            tg_tl::enums::InputGroupCall::InputGroupCall(c) => CallId::from_tl(c.id),
            _ => return Err(TlConversionError::MissingGroupCallId),
        };
        Ok(Self {
            call_id,
            participants: tl.participants.into_iter().map(Into::into).collect(),
            version: tl.version,
        })
    }
}

impl From<tg_tl::types::UpdateGroupCallConnection> for UpdateGroupCallConnection {
    fn from(tl: tg_tl::types::UpdateGroupCallConnection) -> Self {
        let params_json = match tl.params {
            tg_tl::enums::DataJSON::DataJSON(d) => d.data,
        };
        Self {
            presentation: tl.presentation,
            params_json,
        }
    }
}

impl TryFrom<tg_tl::types::UpdateGroupCallChainBlocks> for UpdateGroupCallChainBlocks {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::UpdateGroupCallChainBlocks) -> Result<Self, Self::Error> {
        let call_id = match tl.call {
            tg_tl::enums::InputGroupCall::InputGroupCall(c) => CallId::from_tl(c.id),
            _ => return Err(TlConversionError::MissingGroupCallId),
        };
        Ok(Self {
            call_id,
            sub_chain_id: tl.sub_chain_id,
            blocks: tl.blocks,
            next_offset: tl.next_offset,
        })
    }
}

fn entity_from_tl(entity: &tg_tl::enums::MessageEntity) -> Option<GroupCallMessageEntity> {
    match entity {
        tg_tl::enums::MessageEntity::Bold(e) => Some(GroupCallMessageEntity::Bold {
            offset: e.offset,
            length: e.length,
        }),
        tg_tl::enums::MessageEntity::Italic(e) => Some(GroupCallMessageEntity::Italic {
            offset: e.offset,
            length: e.length,
        }),
        tg_tl::enums::MessageEntity::Underline(e) => Some(GroupCallMessageEntity::Underline {
            offset: e.offset,
            length: e.length,
        }),
        tg_tl::enums::MessageEntity::Strike(e) => Some(GroupCallMessageEntity::Strike {
            offset: e.offset,
            length: e.length,
        }),
        tg_tl::enums::MessageEntity::Spoiler(e) => Some(GroupCallMessageEntity::Spoiler {
            offset: e.offset,
            length: e.length,
        }),
        tg_tl::enums::MessageEntity::Code(e) => Some(GroupCallMessageEntity::Code {
            offset: e.offset,
            length: e.length,
        }),
        tg_tl::enums::MessageEntity::CustomEmoji(e) => Some(GroupCallMessageEntity::CustomEmoji {
            offset: e.offset,
            length: e.length,
            document_id: e.document_id,
        }),
        _ => None,
    }
}

impl From<tg_tl::types::GroupCallMessage> for GroupCallMessageData {
    fn from(tl: tg_tl::types::GroupCallMessage) -> Self {
        let (text, entities) = match tl.message {
            tg_tl::enums::TextWithEntities::TextWithEntities(t) => (
                t.text,
                t.entities.iter().filter_map(entity_from_tl).collect(),
            ),
        };
        Self {
            from_admin: tl.from_admin,
            id: tl.id,
            from_id: peer_from_tl(&tl.from_id),
            date: tl.date,
            text,
            entities,
            paid_message_stars: tl.paid_message_stars,
        }
    }
}

impl From<tg_tl::enums::GroupCallMessage> for GroupCallMessageData {
    fn from(tl: tg_tl::enums::GroupCallMessage) -> Self {
        match tl {
            tg_tl::enums::GroupCallMessage::GroupCallMessage(inner) => inner.into(),
        }
    }
}

impl TryFrom<tg_tl::types::UpdateGroupCallMessage> for UpdateGroupCallMessage {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::UpdateGroupCallMessage) -> Result<Self, Self::Error> {
        let call_id = match tl.call {
            tg_tl::enums::InputGroupCall::InputGroupCall(c) => CallId::from_tl(c.id),
            _ => return Err(TlConversionError::MissingGroupCallId),
        };
        Ok(Self {
            call_id,
            message: tl.message.into(),
        })
    }
}

impl TryFrom<tg_tl::types::UpdateGroupCallEncryptedMessage> for UpdateGroupCallEncryptedMessage {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::UpdateGroupCallEncryptedMessage) -> Result<Self, Self::Error> {
        let call_id = match tl.call {
            tg_tl::enums::InputGroupCall::InputGroupCall(c) => CallId::from_tl(c.id),
            _ => return Err(TlConversionError::MissingGroupCallId),
        };
        Ok(Self {
            call_id,
            from_id: peer_from_tl(&tl.from_id),
            encrypted_message: tl.encrypted_message,
        })
    }
}

impl TryFrom<tg_tl::types::UpdateDeleteGroupCallMessages> for UpdateDeleteGroupCallMessages {
    type Error = TlConversionError;

    fn try_from(tl: tg_tl::types::UpdateDeleteGroupCallMessages) -> Result<Self, Self::Error> {
        let call_id = match tl.call {
            tg_tl::enums::InputGroupCall::InputGroupCall(c) => CallId::from_tl(c.id),
            _ => return Err(TlConversionError::MissingGroupCallId),
        };
        Ok(Self {
            call_id,
            messages: tl.messages,
        })
    }
}

impl From<tg_tl::types::GroupCallDonor> for GroupCallDonorData {
    fn from(tl: tg_tl::types::GroupCallDonor) -> Self {
        Self {
            top: tl.top,
            my: tl.my,
            peer_id: tl.peer_id.as_ref().map(peer_from_tl),
            stars: tl.stars,
        }
    }
}

impl From<tg_tl::enums::GroupCallDonor> for GroupCallDonorData {
    fn from(tl: tg_tl::enums::GroupCallDonor) -> Self {
        match tl {
            tg_tl::enums::GroupCallDonor::GroupCallDonor(inner) => inner.into(),
        }
    }
}

impl From<tg_tl::types::phone::GroupCallStars> for PhoneGroupCallStarsResponse {
    fn from(tl: tg_tl::types::phone::GroupCallStars) -> Self {
        Self {
            total_stars: tl.total_stars,
            top_donors: tl.top_donors.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<tg_tl::enums::phone::GroupCallStars> for PhoneGroupCallStarsResponse {
    fn from(tl: tg_tl::enums::phone::GroupCallStars) -> Self {
        match tl {
            tg_tl::enums::phone::GroupCallStars::GroupCallStars(inner) => inner.into(),
        }
    }
}

impl From<tg_tl::types::GroupCallStreamChannel> for GroupCallStreamChannel {
    fn from(tl: tg_tl::types::GroupCallStreamChannel) -> Self {
        Self {
            channel: tl.channel,
            scale: tl.scale,
            last_timestamp_ms: tl.last_timestamp_ms,
        }
    }
}

impl From<tg_tl::enums::GroupCallStreamChannel> for GroupCallStreamChannel {
    fn from(tl: tg_tl::enums::GroupCallStreamChannel) -> Self {
        match tl {
            tg_tl::enums::GroupCallStreamChannel::GroupCallStreamChannel(inner) => inner.into(),
        }
    }
}

/// Decoded `phone.groupCallStreamChannels` RPC response payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhoneGroupCallStreamChannelsResponse {
    /// Available RTMP stream channels.
    pub channels: Vec<GroupCallStreamChannel>,
}

impl From<tg_tl::types::phone::GroupCallStreamChannels> for PhoneGroupCallStreamChannelsResponse {
    fn from(tl: tg_tl::types::phone::GroupCallStreamChannels) -> Self {
        Self {
            channels: tl.channels.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<tg_tl::enums::phone::GroupCallStreamChannels> for PhoneGroupCallStreamChannelsResponse {
    fn from(tl: tg_tl::enums::phone::GroupCallStreamChannels) -> Self {
        match tl {
            tg_tl::enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(inner) => {
                inner.into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_roundtrip_all_variants() {
        let u = PeerId::from_user(UserId(12345));
        let tl_u = peer_to_tl(u).unwrap();
        assert_eq!(peer_from_tl(&tl_u), u);

        let c = PeerId::from_chat(ChatId(678));
        let tl_c = peer_to_tl(c).unwrap();
        assert_eq!(peer_from_tl(&tl_c), c);

        let ch = PeerId::from_channel(ChannelId(999_999));
        let tl_ch = peer_to_tl(ch).unwrap();
        assert_eq!(peer_from_tl(&tl_ch), ch);

        assert!(peer_to_tl(PeerId::ZERO).is_err());
    }

    #[test]
    fn phone_call_and_connection_conversions() {
        let tl_call = tg_tl::enums::phone::PhoneCall::PhoneCall(tg_tl::types::phone::PhoneCall {
            phone_call: tg_tl::enums::PhoneCall::PhoneCall(tg_tl::types::PhoneCall {
                p2p_allowed: true,
                video: true,
                conference_supported: true,
                id: 42,
                access_hash: -7,
                date: 1_700_000_000,
                admin_id: 100,
                participant_id: 200,
                g_a_or_b: vec![1, 2, 3],
                key_fingerprint: -1,
                protocol: tg_tl::enums::PhoneCallProtocol::PhoneCallProtocol(
                    tg_tl::types::PhoneCallProtocol {
                        udp_p2p: true,
                        udp_reflector: true,
                        min_layer: 65,
                        max_layer: 92,
                        library_versions: vec!["11.0.0".into()],
                    },
                ),
                connections: vec![
                    tg_tl::enums::PhoneConnection::PhoneConnection(tg_tl::types::PhoneConnection {
                        tcp: false,
                        id: 1,
                        ip: "1.2.3.4".into(),
                        ipv6: "::1".into(),
                        port: 599,
                        peer_tag: vec![0xAA; 16],
                    }),
                    tg_tl::enums::PhoneConnection::Webrtc(tg_tl::types::PhoneConnectionWebrtc {
                        turn: true,
                        stun: true,
                        id: 2,
                        ip: "5.6.7.8".into(),
                        ipv6: String::new(),
                        port: 3478,
                        username: "u".into(),
                        password: "p".into(),
                    }),
                ],
                start_date: 1_700_000_005,
                custom_parameters: Some(tg_tl::enums::DataJSON::DataJSON(tg_tl::types::DataJSON {
                    data: "{}".into(),
                })),
            }),
            users: vec![],
        });

        let converted = PhoneCallUpdate::try_from(tl_call).unwrap();
        let PhoneCallUpdate::Active(active) = converted else {
            panic!("expected Active");
        };
        assert_eq!(active.id, CallId(42));
        assert_eq!(active.admin_id, UserId(100));
        assert_eq!(active.participant_id, UserId(200));
        assert_eq!(active.key_fingerprint, u64::MAX);
        assert_eq!(active.connections.len(), 2);
        assert_eq!(active.custom_parameters.as_deref(), Some("{}"));

        // Invalid port rejected.
        let bad_conn =
            tg_tl::enums::PhoneConnection::PhoneConnection(tg_tl::types::PhoneConnection {
                tcp: false,
                id: 1,
                ip: "1.2.3.4".into(),
                ipv6: String::new(),
                port: 70_000,
                peer_tag: vec![],
            });
        assert_eq!(
            PhoneConnection::try_from(bad_conn),
            Err(TlConversionError::InvalidPort(70_000))
        );
    }

    #[test]
    fn dh_config_and_group_call_conversions() {
        let dh_tl = tg_tl::enums::messages::DhConfig::DhConfig(tg_tl::types::messages::DhConfig {
            g: 3,
            p: vec![0xFF; 256],
            version: 14,
            random: vec![0x11; 256],
        });
        let dh = DhConfigResponse::from(dh_tl.clone());
        assert_eq!(tg_tl::enums::messages::DhConfig::from(dh), dh_tl);

        let igc = InputGroupCall::InviteMessage { msg_id: MsgId(77) };
        let igc_tl = tg_tl::enums::InputGroupCall::try_from(igc.clone()).unwrap();
        assert_eq!(InputGroupCall::from(igc_tl), igc);

        let upd = tg_tl::types::UpdateGroupCallParticipants {
            call: tg_tl::enums::InputGroupCall::InputGroupCall(tg_tl::types::InputGroupCall {
                id: 500,
                access_hash: 99,
            }),
            participants: vec![tg_tl::enums::GroupCallParticipant::GroupCallParticipant(
                tg_tl::types::GroupCallParticipant {
                    muted: true,
                    left: false,
                    can_self_unmute: true,
                    just_joined: true,
                    versioned: true,
                    min: false,
                    muted_by_you: false,
                    volume_by_admin: false,
                    self_: true,
                    video_joined: false,
                    peer: tg_tl::enums::Peer::User(tg_tl::types::PeerUser { user_id: 7 }),
                    date: 1000,
                    active_date: Some(1005),
                    source: -1,
                    volume: Some(10_000),
                    about: None,
                    raise_hand_rating: Some(5),
                    video: None,
                    presentation: None,
                    paid_stars_total: None,
                },
            )],
            version: 3,
        };
        let converted = UpdateGroupCallParticipants::try_from(upd).unwrap();
        assert_eq!(converted.call_id, CallId(500));
        assert_eq!(converted.participants.len(), 1);
        assert_eq!(converted.participants[0].peer, PeerId::from_user(UserId(7)));
        assert_eq!(converted.participants[0].source, u32::MAX);
        assert!(converted.participants[0].is_self);

        let conn_upd = tg_tl::types::UpdateGroupCallConnection {
            presentation: true,
            params: tg_tl::enums::DataJSON::DataJSON(tg_tl::types::DataJSON {
                data: r#"{"ok":true}"#.into(),
            }),
        };
        let conn = UpdateGroupCallConnection::from(conn_upd);
        assert!(conn.presentation);
        assert_eq!(conn.params_json, r#"{"ok":true}"#);
    }
}
