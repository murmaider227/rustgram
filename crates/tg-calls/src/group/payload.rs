//! Group call join payload and join response JSON structures.
//!
//! Port of `ParseJoinResponse` (`calls/group/calls_group_call.cpp:89-129`) and the
//! `tgcalls::GroupJoinPayload` / `updateGroupCallConnection.params` JSON schemas.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::GroupCallError;

/// RTMP livestream ingestion credentials (`Calls::Group::RtmpInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtmpInfo {
    /// RTMP server ingest URL (`rtmp_stream_url`).
    pub url: String,
    /// RTMP stream key (`rtmp_stream_key`).
    pub key: String,
}

/// DTLS certificate fingerprint entry in group call JSON params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DtlsFingerprint {
    /// Hash algorithm (e.g. `"sha-256"`).
    pub hash: String,
    /// DTLS setup role (e.g. `"active"`, `"passive"`, `"actpass"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// Colon-separated hex fingerprint string.
    pub fingerprint: String,
}

/// SSRC group descriptor inside a group call join payload (`"ssrc-groups"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSsrcGroup {
    /// Grouping semantics (e.g. `"SIM"`, `"FID"`).
    pub semantics: String,
    /// SSRC numbers in this group.
    pub sources: Vec<u32>,
}

/// Local WebRTC join payload serialized to `DataJSON` for `phone.joinGroupCall` and
/// `phone.joinGroupCallPresentation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupJoinPayload {
    /// Random non-zero local audio SSRC.
    pub ssrc: u32,
    /// ICE username fragment.
    pub ufrag: String,
    /// ICE password.
    pub pwd: String,
    /// Local DTLS certificate fingerprints.
    pub fingerprints: Vec<DtlsFingerprint>,
    /// Optional video SSRC groups (simulcast/RTX) when publishing video.
    #[serde(rename = "ssrc-groups", default, skip_serializing_if = "Vec::is_empty")]
    pub ssrc_groups: Vec<JoinSsrcGroup>,
}

impl GroupJoinPayload {
    /// Serialize the join payload to compact JSON for `MTP_dataJSON`.
    pub fn to_json(&self) -> Result<String, GroupCallError> {
        serde_json::to_string(self).map_err(|e| GroupCallError::InvalidJson(e.to_string()))
    }

    /// Deserialize a join payload from JSON.
    pub fn from_json(json: &str) -> Result<Self, GroupCallError> {
        serde_json::from_str(json).map_err(|e| GroupCallError::InvalidJson(e.to_string()))
    }
}

/// ICE candidate returned by the Telegram SFU in `updateGroupCallConnection.params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceCandidateParam {
    /// Candidate port (often encoded as a JSON string by Telegram SFU).
    #[serde(default)]
    pub port: String,
    /// Transport protocol (`"udp"` / `"tcp"`).
    #[serde(default)]
    pub protocol: String,
    /// Network ID.
    #[serde(default)]
    pub network: String,
    /// Candidate generation.
    #[serde(default)]
    pub generation: String,
    /// Candidate identifier.
    #[serde(default)]
    pub id: String,
    /// ICE component (`"1"` for RTP).
    #[serde(default)]
    pub component: String,
    /// ICE foundation.
    #[serde(default)]
    pub foundation: String,
    /// ICE priority.
    #[serde(default)]
    pub priority: String,
    /// IPv4 or IPv6 address.
    #[serde(default)]
    pub ip: String,
    /// Candidate type (`"host"`, `"srflx"`, `"relay"`).
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// SFU transport section of `updateGroupCallConnection.params`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinResponseTransport {
    /// Remote ICE username fragment.
    #[serde(default)]
    pub ufrag: String,
    /// Remote ICE password.
    #[serde(default)]
    pub pwd: String,
    /// Remote DTLS fingerprints.
    #[serde(default)]
    pub fingerprints: Vec<DtlsFingerprint>,
    /// Remote ICE candidates.
    #[serde(default)]
    pub candidates: Vec<IceCandidateParam>,
    /// Whether RTCP-mux is enabled.
    #[serde(rename = "rtcp-mux", default)]
    pub rtcp_mux: bool,
}

/// RTP codec payload type descriptor returned by the SFU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtpPayloadTypeParam {
    /// RTP payload type ID.
    pub id: u32,
    /// Codec name (e.g. `"opus"`, `"VP8"`, `"H264"`, `"rtx"`).
    pub name: String,
    /// Clock rate in Hz (e.g. 48000, 90000).
    #[serde(default)]
    pub clockrate: u32,
    /// Channel count (audio codecs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// Codec-specific parameters (e.g. `minptime`, `useinbandfec`, `apt`).
    #[serde(default)]
    pub parameters: serde_json::Map<String, Value>,
}

/// RTP header extension mapping returned by the SFU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtpHeaderExtensionParam {
    /// Extension numeric ID.
    pub id: u32,
    /// Extension URI.
    pub uri: String,
}

/// Media description (`audio` / `video`) inside `updateGroupCallConnection.params`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinResponseMediaSection {
    /// Server-assigned video endpoint ID (present in `video` section for RTC calls).
    #[serde(default)]
    pub endpoint: String,
    /// Supported RTP payload types.
    #[serde(rename = "payload-types", default)]
    pub payload_types: Vec<RtpPayloadTypeParam>,
    /// Supported RTP header extensions.
    #[serde(rename = "rtp-hdrexts", default)]
    pub rtp_hdrexts: Vec<RtpHeaderExtensionParam>,
}

/// Parsed RTC join response from `updateGroupCallConnection.params`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtcJoinResponse {
    /// SFU ICE/DTLS transport parameters.
    #[serde(default)]
    pub transport: JoinResponseTransport,
    /// Audio codec/extension parameters.
    #[serde(default)]
    pub audio: JoinResponseMediaSection,
    /// Video codec/extension parameters and assigned endpoint ID.
    #[serde(default)]
    pub video: JoinResponseMediaSection,
}

/// Classification of `updateGroupCallConnection.params`, porting `JoinClientFields` and
/// `ParseJoinResponse` (`calls/group/calls_group_call.cpp:89-129`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedJoinResponse {
    /// Broadcast / RTMP stream mode (`"stream": true`).
    BroadcastStream {
        /// Whether this stream uses RTMP (`"rtmp": true`).
        rtmp: bool,
        /// Optional RTMP ingest URL and key.
        rtmp_info: RtmpInfo,
    },
    /// Interactive WebRTC SFU mode.
    Rtc {
        /// Assigned video endpoint ID (`video.endpoint`, may be empty).
        video_endpoint: String,
        /// Full parsed RTC response (best-effort; preserves unknown SFU fields gracefully).
        params: Box<RtcJoinResponse>,
    },
}

/// Parse `updateGroupCallConnection.params` JSON exactly as `ParseJoinResponse` in
/// `calls/group/calls_group_call.cpp:103-129`.
pub fn parse_join_response(json: &str) -> Result<ParsedJoinResponse, GroupCallError> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| GroupCallError::InvalidJson(e.to_string()))?;
    let Some(obj) = root.as_object() else {
        return Err(GroupCallError::NotJsonObject);
    };

    if obj.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let rtmp = obj.get("rtmp").and_then(Value::as_bool).unwrap_or(false);
        let url = obj
            .get("rtmp_stream_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let key = obj
            .get("rtmp_stream_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(ParsedJoinResponse::BroadcastStream {
            rtmp,
            rtmp_info: RtmpInfo { url, key },
        });
    }

    let video_endpoint = obj
        .get("video")
        .and_then(Value::as_object)
        .and_then(|v| v.get("endpoint"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let params = serde_json::from_value::<RtcJoinResponse>(root).unwrap_or_else(|_| {
        let mut fallback = RtcJoinResponse::default();
        fallback.video.endpoint = video_endpoint.clone();
        fallback
    });

    Ok(ParsedJoinResponse::Rtc {
        video_endpoint,
        params: Box::new(params),
    })
}
