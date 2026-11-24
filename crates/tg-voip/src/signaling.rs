//! `tgcalls` v2 JSON signaling messages and encrypted signaling channel.
//!
//! Port of `TelegramMessenger/tgcalls` (`development` branch):
//! - `tgcalls/v2/Signaling.h`
//! - `tgcalls/v2/Signaling.cpp`
//! - `tgcalls/v2/SignalingEncryption.cpp`
//!
//! Serializes and parses the exact `@type`-tagged JSON messages exchanged via
//! `phone.sendSignalingData` and `updatePhoneCallSignalingData`:
//! - `InitialSetup`
//! - `NegotiateChannels`
//! - `Candidates`
//! - `MediaState`

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tg_calls::{RemoteAudioState, RemoteBatteryState, VideoState};

use crate::crypto::{AUTH_KEY_SIZE, EncryptedConnection};
use crate::error::{VoipError, VoipResult};

/// DTLS certificate fingerprint entry (`signaling::DtlsFingerprint`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalingDtlsFingerprint {
    /// Hash algorithm name (e.g. `"sha-256"`).
    pub hash: String,
    /// DTLS setup role (`"active"`, `"passive"`, `"actpass"`).
    pub setup: String,
    /// Colon-separated hex digest.
    pub fingerprint: String,
}

/// Trickle ICE candidate JSON entry (`signaling::IceCandidate`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalingIceCandidate {
    /// SDP candidate attribute string (e.g. `"candidate:1 1 udp ..."`).
    #[serde(rename = "sdpString")]
    pub sdp_string: String,
}

/// SSRC grouping entry (`signaling::SsrcGroup`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalingSsrcGroup {
    /// Grouped SSRCs (serialized as decimal strings in `tgcalls`).
    pub ssrcs: Vec<u32>,
    /// Semantics (e.g. `"FID"` or `"SIM"`).
    pub semantics: String,
}

/// RTCP feedback entry (`signaling::FeedbackType`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalingFeedbackType {
    /// Feedback type (e.g. `"transport-cc"`, `"nack"`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Feedback subtype (empty or e.g. `"pli"`).
    pub subtype: String,
}

/// RTP header extension entry (`webrtc::RtpExtension`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalingRtpExtension {
    /// 1-based extension ID.
    pub id: u32,
    /// Extension URI.
    pub uri: String,
}

/// Negotiated codec payload type (`signaling::PayloadType`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalingPayloadType {
    /// RTP payload type ID (e.g. `111` for Opus).
    pub id: u32,
    /// Codec name (e.g. `"opus"`).
    pub name: String,
    /// Clock rate in Hz (e.g. `48000`).
    pub clockrate: u32,
    /// Channel count (e.g. `2` for Opus RTP).
    #[serde(default)]
    pub channels: u32,
    /// RTCP feedback capabilities.
    #[serde(rename = "feedbackTypes", default)]
    pub feedback_types: Vec<SignalingFeedbackType>,
    /// Codec fmtp parameters (e.g. `useinbandfec=1`, `minptime=10`).
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
}

/// Media track kind (`signaling::MediaContent::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalingMediaType {
    /// Audio track.
    Audio,
    /// Video track.
    Video,
}

/// Negotiated media content section (`signaling::MediaContent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalingMediaContent {
    /// Audio or video.
    pub media_type: SignalingMediaType,
    /// Primary RTP SSRC.
    pub ssrc: u32,
    /// Optional SSRC groups.
    pub ssrc_groups: Vec<SignalingSsrcGroup>,
    /// Supported payload types.
    pub payload_types: Vec<SignalingPayloadType>,
    /// Supported RTP header extensions.
    pub rtp_extensions: Vec<SignalingRtpExtension>,
}

/// Video state values in `MediaState` JSON (`Signaling.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignalingVideoState {
    /// Video off (`"inactive"`).
    #[default]
    Inactive,
    /// Video paused/suspended (`"suspended"`).
    Suspended,
    /// Video streaming (`"active"`).
    Active,
}

impl SignalingVideoState {
    /// Convert to `tg_calls::VideoState`.
    #[must_use]
    pub const fn to_call_video_state(self) -> VideoState {
        match self {
            SignalingVideoState::Inactive => VideoState::Inactive,
            SignalingVideoState::Suspended => VideoState::Paused,
            SignalingVideoState::Active => VideoState::Active,
        }
    }

    /// Convert from `tg_calls::VideoState`.
    #[must_use]
    pub const fn from_call_video_state(state: VideoState) -> Self {
        match state {
            VideoState::Inactive => SignalingVideoState::Inactive,
            VideoState::Paused => SignalingVideoState::Suspended,
            VideoState::Active => SignalingVideoState::Active,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            SignalingVideoState::Inactive => "inactive",
            SignalingVideoState::Suspended => "suspended",
            SignalingVideoState::Active => "active",
        }
    }

    fn parse_str(s: &str) -> Self {
        match s {
            "active" => SignalingVideoState::Active,
            "suspended" => SignalingVideoState::Suspended,
            _ => SignalingVideoState::Inactive,
        }
    }
}

/// `tgcalls` v2 JSON signaling messages (`signaling::Message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalingMessage {
    /// `@type = "InitialSetup"`: ICE credentials, renomination support, and DTLS fingerprints.
    InitialSetup {
        /// ICE username fragment.
        ufrag: String,
        /// ICE password.
        pwd: String,
        /// Whether ICE renomination is supported.
        supports_renomination: bool,
        /// DTLS certificate fingerprints.
        fingerprints: Vec<SignalingDtlsFingerprint>,
    },
    /// `@type = "NegotiateChannels"`: audio/video SSRCs, payload types, and RTP extensions.
    NegotiateChannels {
        /// Monotonic negotiation exchange identifier.
        exchange_id: u32,
        /// Offered or answered media sections.
        contents: Vec<SignalingMediaContent>,
    },
    /// `@type = "Candidates"`: trickle ICE candidates (host, srflx, relay, reflector).
    Candidates {
        /// Encoded SDP candidate strings.
        candidates: Vec<SignalingIceCandidate>,
    },
    /// `@type = "MediaState"`: mute, video, screencast, rotation, and low-battery flags.
    MediaState {
        /// Whether local microphone is muted.
        muted: bool,
        /// Whether local device battery is low.
        low_battery: bool,
        /// Camera video state.
        video_state: SignalingVideoState,
        /// Video rotation in degrees (`0`, `90`, `180`, `270`).
        video_rotation: u16,
        /// Screencast state.
        screencast_state: SignalingVideoState,
    },
}

impl SignalingMessage {
    /// Helper returning the default Opus 48 kHz audio `NegotiateChannels` payload.
    #[must_use]
    pub fn default_audio_negotiate(exchange_id: u32, audio_ssrc: u32) -> Self {
        SignalingMessage::NegotiateChannels {
            exchange_id,
            contents: vec![Self::default_audio_content(audio_ssrc)],
        }
    }

    /// Helper returning a `NegotiateChannels` payload advertising both Opus audio and H.264 video
    /// (with RFC 6184 `packetization-mode=1`, `nack`, `nack/pli`, `ccm/fir`, and `transport-cc`).
    #[must_use]
    pub fn default_audio_video_negotiate(
        exchange_id: u32,
        audio_ssrc: u32,
        video_ssrc: u32,
    ) -> Self {
        let mut h264_params = BTreeMap::new();
        h264_params.insert("level-asymmetry-allowed".to_string(), "1".to_string());
        h264_params.insert("packetization-mode".to_string(), "1".to_string());
        h264_params.insert("profile-level-id".to_string(), "42e01f".to_string());

        let video_content = SignalingMediaContent {
            media_type: SignalingMediaType::Video,
            ssrc: video_ssrc,
            ssrc_groups: Vec::new(),
            payload_types: vec![SignalingPayloadType {
                id: u32::from(crate::rtp::H264_RTP_PAYLOAD_TYPE),
                name: "H264".to_string(),
                clockrate: crate::rtp::H264_CLOCK_RATE,
                channels: 0,
                feedback_types: vec![
                    SignalingFeedbackType {
                        kind: "transport-cc".to_string(),
                        subtype: String::new(),
                    },
                    SignalingFeedbackType {
                        kind: "nack".to_string(),
                        subtype: String::new(),
                    },
                    SignalingFeedbackType {
                        kind: "nack".to_string(),
                        subtype: "pli".to_string(),
                    },
                    SignalingFeedbackType {
                        kind: "ccm".to_string(),
                        subtype: "fir".to_string(),
                    },
                ],
                parameters: h264_params,
            }],
            rtp_extensions: vec![
                SignalingRtpExtension {
                    id: 2,
                    uri: "urn:3gpp:video-orientation".to_string(),
                },
                SignalingRtpExtension {
                    id: 3,
                    uri:
                        "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"
                            .to_string(),
                },
            ],
        };

        SignalingMessage::NegotiateChannels {
            exchange_id,
            contents: vec![Self::default_audio_content(audio_ssrc), video_content],
        }
    }

    fn default_audio_content(audio_ssrc: u32) -> SignalingMediaContent {
        let mut params = BTreeMap::new();
        params.insert("minptime".to_string(), "10".to_string());
        params.insert("useinbandfec".to_string(), "1".to_string());

        SignalingMediaContent {
            media_type: SignalingMediaType::Audio,
            ssrc: audio_ssrc,
            ssrc_groups: Vec::new(),
            payload_types: vec![SignalingPayloadType {
                id: u32::from(crate::rtp::OPUS_RTP_PAYLOAD_TYPE),
                name: "opus".to_string(),
                clockrate: 48_000,
                channels: 2,
                feedback_types: vec![SignalingFeedbackType {
                    kind: "transport-cc".to_string(),
                    subtype: String::new(),
                }],
                parameters: params,
            }],
            rtp_extensions: vec![SignalingRtpExtension {
                id: 1,
                uri: "urn:ietf:params:rtp-hdrext:ssrc-audio-level".to_string(),
            }],
        }
    }

    /// Extract `(RemoteAudioState, VideoState, RemoteBatteryState)` if this is a `MediaState`.
    #[must_use]
    pub const fn as_call_media_state(
        &self,
    ) -> Option<(RemoteAudioState, VideoState, RemoteBatteryState)> {
        match self {
            SignalingMessage::MediaState {
                muted,
                low_battery,
                video_state,
                ..
            } => Some((
                if *muted {
                    RemoteAudioState::Muted
                } else {
                    RemoteAudioState::Active
                },
                video_state.to_call_video_state(),
                if *low_battery {
                    RemoteBatteryState::Low
                } else {
                    RemoteBatteryState::Normal
                },
            )),
            _ => None,
        }
    }

    /// Serialize to UTF-8 JSON bytes matching `tgcalls/v2/Signaling.cpp`.
    pub fn serialize(&self) -> VoipResult<Vec<u8>> {
        let value = match self {
            SignalingMessage::InitialSetup {
                ufrag,
                pwd,
                supports_renomination,
                fingerprints,
            } => serde_json::json!({
                "@type": "InitialSetup",
                "ufrag": ufrag,
                "pwd": pwd,
                "renomination": supports_renomination,
                "fingerprints": fingerprints,
            }),
            SignalingMessage::NegotiateChannels {
                exchange_id,
                contents,
            } => {
                let json_contents: Vec<serde_json::Value> = contents
                    .iter()
                    .map(|c| {
                        let mut obj = serde_json::Map::new();
                        obj.insert(
                            "type".into(),
                            serde_json::Value::String(match c.media_type {
                                SignalingMediaType::Audio => "audio".into(),
                                SignalingMediaType::Video => "video".into(),
                            }),
                        );
                        obj.insert(
                            "ssrc".into(),
                            serde_json::Value::String(c.ssrc.to_string()),
                        );
                        if !c.ssrc_groups.is_empty() {
                            let groups: Vec<serde_json::Value> = c
                                .ssrc_groups
                                .iter()
                                .map(|g| {
                                    serde_json::json!({
                                        "semantics": g.semantics,
                                        "ssrcs": g.ssrcs.iter().map(u32::to_string).collect::<Vec<_>>(),
                                    })
                                })
                                .collect();
                            obj.insert("ssrcGroups".into(), serde_json::Value::Array(groups));
                        }
                        if !c.payload_types.is_empty() {
                            obj.insert(
                                "payloadTypes".into(),
                                serde_json::to_value(&c.payload_types).unwrap_or_default(),
                            );
                        }
                        obj.insert(
                            "rtpExtensions".into(),
                            serde_json::to_value(&c.rtp_extensions).unwrap_or_default(),
                        );
                        serde_json::Value::Object(obj)
                    })
                    .collect();

                serde_json::json!({
                    "@type": "NegotiateChannels",
                    "exchangeId": exchange_id.to_string(),
                    "contents": json_contents,
                })
            }
            SignalingMessage::Candidates { candidates } => serde_json::json!({
                "@type": "Candidates",
                "candidates": candidates,
            }),
            SignalingMessage::MediaState {
                muted,
                low_battery,
                video_state,
                video_rotation,
                screencast_state,
            } => serde_json::json!({
                "@type": "MediaState",
                "muted": muted,
                "lowBattery": low_battery,
                "videoState": video_state.as_str(),
                "videoRotation": video_rotation,
                "screencastState": screencast_state.as_str(),
            }),
        };

        serde_json::to_vec(&value).map_err(|e| VoipError::Signaling(e.to_string()))
    }

    /// Parse a UTF-8 JSON message produced by `tgcalls/v2/Signaling.cpp`.
    pub fn parse(bytes: &[u8]) -> VoipResult<Self> {
        let root: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| VoipError::Signaling(e.to_string()))?;
        let obj = root
            .as_object()
            .ok_or_else(|| VoipError::Signaling("root JSON must be an object".into()))?;
        let msg_type = obj
            .get("@type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| VoipError::Signaling("missing @type field".into()))?;

        match msg_type {
            "InitialSetup" => {
                let ufrag = require_str(obj, "ufrag")?.to_string();
                let pwd = require_str(obj, "pwd")?.to_string();
                let supports_renomination = obj
                    .get("renomination")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let fps_val = obj
                    .get("fingerprints")
                    .ok_or_else(|| VoipError::Signaling("missing fingerprints".into()))?;
                let fingerprints: Vec<SignalingDtlsFingerprint> =
                    serde_json::from_value(fps_val.clone())
                        .map_err(|e| VoipError::Signaling(e.to_string()))?;
                Ok(SignalingMessage::InitialSetup {
                    ufrag,
                    pwd,
                    supports_renomination,
                    fingerprints,
                })
            }
            "NegotiateChannels" => {
                let exchange_id = parse_u32_or_str(obj.get("exchangeId"), "exchangeId")?;
                let mut contents = Vec::new();
                if let Some(arr) = obj.get("contents").and_then(serde_json::Value::as_array) {
                    for item in arr {
                        let cobj = item
                            .as_object()
                            .ok_or_else(|| VoipError::Signaling("content must be object".into()))?;
                        let media_type = match require_str(cobj, "type")? {
                            "audio" => SignalingMediaType::Audio,
                            "video" => SignalingMediaType::Video,
                            other => {
                                return Err(VoipError::Signaling(format!(
                                    "unknown media type {other}"
                                )));
                            }
                        };
                        let ssrc = parse_u32_or_str(cobj.get("ssrc"), "ssrc")?;
                        let mut ssrc_groups = Vec::new();
                        if let Some(groups) =
                            cobj.get("ssrcGroups").and_then(serde_json::Value::as_array)
                        {
                            for g in groups {
                                let gobj = g.as_object().ok_or_else(|| {
                                    VoipError::Signaling("ssrcGroup must be object".into())
                                })?;
                                let semantics = require_str(gobj, "semantics")?.to_string();
                                let mut ssrcs = Vec::new();
                                if let Some(list) =
                                    gobj.get("ssrcs").and_then(serde_json::Value::as_array)
                                {
                                    for v in list {
                                        ssrcs.push(parse_u32_or_str(Some(v), "ssrcs[]")?);
                                    }
                                }
                                ssrc_groups.push(SignalingSsrcGroup { ssrcs, semantics });
                            }
                        }
                        let payload_types = match cobj.get("payloadTypes") {
                            Some(v) => serde_json::from_value(v.clone())
                                .map_err(|e| VoipError::Signaling(e.to_string()))?,
                            None => Vec::new(),
                        };
                        let rtp_extensions = match cobj.get("rtpExtensions") {
                            Some(v) => serde_json::from_value(v.clone())
                                .map_err(|e| VoipError::Signaling(e.to_string()))?,
                            None => Vec::new(),
                        };
                        contents.push(SignalingMediaContent {
                            media_type,
                            ssrc,
                            ssrc_groups,
                            payload_types,
                            rtp_extensions,
                        });
                    }
                }
                Ok(SignalingMessage::NegotiateChannels {
                    exchange_id,
                    contents,
                })
            }
            "Candidates" => {
                let arr = obj
                    .get("candidates")
                    .ok_or_else(|| VoipError::Signaling("missing candidates".into()))?;
                let candidates: Vec<SignalingIceCandidate> = serde_json::from_value(arr.clone())
                    .map_err(|e| VoipError::Signaling(e.to_string()))?;
                Ok(SignalingMessage::Candidates { candidates })
            }
            "MediaState" => {
                let muted = obj
                    .get("muted")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let low_battery = obj
                    .get("lowBattery")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let video_state = obj
                    .get("videoState")
                    .and_then(serde_json::Value::as_str)
                    .map(SignalingVideoState::parse_str)
                    .unwrap_or_default();
                let screencast_state = obj
                    .get("screencastState")
                    .and_then(serde_json::Value::as_str)
                    .map(SignalingVideoState::parse_str)
                    .unwrap_or_default();
                let video_rotation = obj
                    .get("videoRotation")
                    .and_then(serde_json::Value::as_u64)
                    .map(|r| match r {
                        90 => 90,
                        180 => 180,
                        270 => 270,
                        _ => 0,
                    })
                    .unwrap_or(0);
                Ok(SignalingMessage::MediaState {
                    muted,
                    low_battery,
                    video_state,
                    video_rotation,
                    screencast_state,
                })
            }
            other => Err(VoipError::Signaling(format!(
                "unknown signaling @type: {other}"
            ))),
        }
    }
}

fn require_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> VoipResult<&'a str> {
    obj.get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VoipError::Signaling(format!("missing or non-string field '{key}'")))
}

fn parse_u32_or_str(val: Option<&serde_json::Value>, field: &str) -> VoipResult<u32> {
    let v = val.ok_or_else(|| VoipError::Signaling(format!("missing field '{field}'")))?;
    if let Some(s) = v.as_str() {
        s.parse::<u32>()
            .map_err(|e| VoipError::Signaling(format!("invalid u32 string in '{field}': {e}")))
    } else if let Some(n) = v.as_u64() {
        u32::try_from(n).map_err(|_| VoipError::Signaling(format!("u32 overflow in '{field}'")))
    } else {
        Err(VoipError::Signaling(format!(
            "field '{field}' must be string or number"
        )))
    }
}

/// Encrypted `tgcalls` v2 signaling channel (`SignalingEncryption` + `Signaling`).
#[derive(Debug, Clone)]
pub struct SignalingChannel {
    conn: EncryptedConnection,
}

impl SignalingChannel {
    /// Create a new encrypted signaling channel bound to the call DH key.
    #[must_use]
    pub fn new(auth_key: [u8; AUTH_KEY_SIZE], is_outgoing: bool) -> Self {
        Self {
            conn: EncryptedConnection::new_signaling(auth_key, is_outgoing),
        }
    }

    /// Serialize and encrypt a [`SignalingMessage`] for `phone.sendSignalingData`.
    pub fn encode(&mut self, msg: &SignalingMessage) -> VoipResult<Vec<u8>> {
        let json = msg.serialize()?;
        self.conn.encrypt_raw_packet(&json)
    }

    /// Decrypt and parse an incoming `updatePhoneCallSignalingData` payload.
    pub fn decode(&mut self, encrypted: &[u8]) -> VoipResult<SignalingMessage> {
        let json = self.conn.decrypt_raw_packet(encrypted)?;
        SignalingMessage::parse(&json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signaling_messages_json_roundtrip_and_encryption() {
        let key = [0x42u8; AUTH_KEY_SIZE];
        let mut a = SignalingChannel::new(key, true);
        let mut b = SignalingChannel::new(key, false);

        let msgs = vec![
            SignalingMessage::InitialSetup {
                ufrag: "ufrag123".into(),
                pwd: "pwd456789012345678901234".into(),
                supports_renomination: true,
                fingerprints: vec![SignalingDtlsFingerprint {
                    hash: "sha-256".into(),
                    setup: "actpass".into(),
                    fingerprint: "AA:BB:CC".into(),
                }],
            },
            SignalingMessage::default_audio_negotiate(1, 0x1234_5678),
            SignalingMessage::Candidates {
                candidates: vec![SignalingIceCandidate {
                    sdp_string:
                        "candidate:1 1 udp 2122260223 reflector-1-42.reflector 599 typ relay".into(),
                }],
            },
            SignalingMessage::MediaState {
                muted: true,
                low_battery: true,
                video_state: SignalingVideoState::Suspended,
                video_rotation: 90,
                screencast_state: SignalingVideoState::Inactive,
            },
        ];

        for msg in msgs {
            let enc = a.encode(&msg).unwrap();
            let dec = b.decode(&enc).unwrap();
            assert_eq!(dec, msg);
        }
    }
}
