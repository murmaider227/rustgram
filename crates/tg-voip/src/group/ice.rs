//! ICE-lite connectivity client and `GroupJoinPayload` builder for Telegram group calls SFU.
//!
//! Reference:
//! - RFC 8445 (ICE / ICE-lite), RFC 5389 (STUN MESSAGE-INTEGRITY & FINGERPRINT), RFC 7675 (consent)
//! - `TelegramMessenger/tgcalls` (`GroupInstanceCustomImpl.cpp`, `GroupJoinPayload`)
//! - `crates/tg-calls/src/group/payload.rs` (`GroupJoinPayload`, `RtcJoinResponse`)

use std::net::{IpAddr, SocketAddr};

use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;
use tg_calls::{GroupJoinPayload, IceCandidateParam, JoinSsrcGroup, RtcJoinResponse};

use crate::error::{VoipError, VoipResult};
use crate::group::dtls_srtp::DtlsIdentity;
use crate::reflector::{
    STUN_BINDING_REQUEST, STUN_BINDING_RESPONSE, STUN_MAGIC_COOKIE, is_stun_packet, stun_attr,
};

/// RFC 8445 STUN attribute types for ICE connectivity checks.
mod ice_attr {
    /// `PRIORITY` (4-byte unsigned integer, RFC 8445 §16.1).
    pub const PRIORITY: u16 = 0x0024;
    /// `USE-CANDIDATE` (0-byte flag, RFC 8445 §16.1).
    pub const USE_CANDIDATE: u16 = 0x0025;
    /// `ICE-CONTROLLING` (8-byte tie-breaker, RFC 8445 §16.1).
    pub const ICE_CONTROLLING: u16 = 0x802A;
}

/// ICE-lite consent freshness keepalive interval (5 seconds, RFC 7675).
pub const ICE_CONSENT_INTERVAL_MS: u64 = 5_000;

/// ICE connectivity check retransmission interval while connecting (250 ms).
pub const ICE_CHECK_RETRY_MS: u64 = 250;

/// Local ICE credentials and audio SSRC generated for `phone.joinGroupCall`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIceCredentials {
    /// Positive non-zero 32-bit audio SSRC (`1..=i32::MAX as u32` for TL `int` compatibility).
    pub audio_ssrc: u32,
    /// ICE username fragment (4–8 alphanumeric chars).
    pub ufrag: String,
    /// ICE password (24 alphanumeric chars).
    pub pwd: String,
    /// 64-bit ICE tie-breaker.
    pub tie_breaker: u64,
}

impl LocalIceCredentials {
    /// Generate fresh random ICE credentials and a non-zero Telegram-compatible audio SSRC.
    #[must_use]
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let audio_ssrc = rng.gen_range(1..=(i32::MAX as u32));
        let ufrag = random_ice_token(&mut rng, 6);
        let pwd = random_ice_token(&mut rng, 24);
        let tie_breaker: u64 = rng.r#gen();
        Self {
            audio_ssrc,
            ufrag,
            pwd,
            tie_breaker,
        }
    }
}

fn random_ice_token<R: Rng>(rng: &mut R, len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect()
}

/// Build a `tg_calls::GroupJoinPayload` compatible with `phone.joinGroupCall` and
/// `phone.joinGroupCallPresentation`.
///
/// Uses `setup = "active"` by default so our client initiates DTLS 1.2 against Telegram's
/// passive SFU (matching `tgcalls::GroupInstanceCustomImpl`).
#[must_use]
pub fn build_group_join_payload(
    creds: &LocalIceCredentials,
    identity: &DtlsIdentity,
    ssrc_groups: Vec<JoinSsrcGroup>,
) -> GroupJoinPayload {
    GroupJoinPayload {
        ssrc: creds.audio_ssrc,
        ufrag: creds.ufrag.clone(),
        pwd: creds.pwd.clone(),
        fingerprints: vec![identity.to_group_fingerprint("active")],
        ssrc_groups,
    }
}

/// Parsed UDP candidate returned by the Telegram SFU in `updateGroupCallConnection.params`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfuUdpCandidate {
    /// Target SFU UDP socket address.
    pub addr: SocketAddr,
    /// Parsed ICE candidate priority (higher is preferred).
    pub priority: u32,
    /// Candidate type (`"host"`, `"srflx"`, `"relay"`).
    pub kind: String,
    /// Whether at least one STUN binding response succeeded on this candidate.
    pub verified: bool,
    /// Most recent STUN transaction ID sent to this candidate.
    pub last_tx_id: Option<[u8; 12]>,
    /// Timestamp (ms) of last STUN check transmitted.
    pub last_check_ms: u64,
    /// Timestamp (ms) of last valid STUN response received.
    pub last_resp_ms: u64,
}

/// Parse and priority-sort UDP candidates from `RtcJoinResponse.transport.candidates`.
#[must_use]
pub fn parse_sfu_udp_candidates(raw: &[IceCandidateParam]) -> Vec<SfuUdpCandidate> {
    let mut out: Vec<SfuUdpCandidate> = raw
        .iter()
        .filter(|c| c.protocol.is_empty() || c.protocol.eq_ignore_ascii_case("udp"))
        .filter_map(|c| {
            let ip: IpAddr = c.ip.trim().parse().ok()?;
            let port: u16 = c.port.trim().parse().ok()?;
            if port == 0 {
                return None;
            }
            let priority = c.priority.trim().parse::<u32>().unwrap_or(1_000);
            Some(SfuUdpCandidate {
                addr: SocketAddr::new(ip, port),
                priority,
                kind: if c.kind.is_empty() {
                    "host".to_string()
                } else {
                    c.kind.clone()
                },
                verified: false,
                last_tx_id: None,
                last_check_ms: 0,
                last_resp_ms: 0,
            })
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.priority));
    out
}

/// ICE-lite client managing STUN connectivity checks and candidate nomination toward a Telegram SFU.
#[derive(Debug, Clone)]
pub struct IceLiteClient {
    local: LocalIceCredentials,
    remote_ufrag: String,
    remote_pwd: String,
    candidates: Vec<SfuUdpCandidate>,
    nominated: Option<SocketAddr>,
}

impl IceLiteClient {
    /// Construct an ICE-lite client from local credentials and the parsed SFU [`RtcJoinResponse`].
    pub fn from_join_response(
        local: LocalIceCredentials,
        response: &RtcJoinResponse,
    ) -> VoipResult<Self> {
        let candidates = parse_sfu_udp_candidates(&response.transport.candidates);
        if candidates.is_empty() {
            return Err(VoipError::Ice(
                "SFU join response contained no valid UDP ICE candidates".into(),
            ));
        }
        Ok(Self {
            local,
            remote_ufrag: response.transport.ufrag.clone(),
            remote_pwd: response.transport.pwd.clone(),
            candidates,
            nominated: None,
        })
    }

    /// Currently nominated SFU UDP address, once a STUN binding check succeeds.
    #[must_use]
    pub const fn nominated_addr(&self) -> Option<SocketAddr> {
        self.nominated
    }

    /// Best candidate address (nominated if ready, otherwise highest-priority candidate).
    #[must_use]
    pub fn primary_addr(&self) -> Option<SocketAddr> {
        self.nominated
            .or_else(|| self.candidates.first().map(|c| c.addr))
    }

    /// All parsed SFU UDP candidates.
    #[must_use]
    pub fn candidates(&self) -> &[SfuUdpCandidate] {
        &self.candidates
    }

    /// Build STUN Binding Requests due at `now_ms` for probing (before nomination) or consent
    /// freshness (every 5 s after nomination).
    pub fn poll_outgoing_checks(&mut self, now_ms: u64) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        let username = format!("{}:{}", self.remote_ufrag, self.local.ufrag);

        if let Some(nom_addr) = self.nominated {
            if let Some(cand) = self.candidates.iter_mut().find(|c| c.addr == nom_addr)
                && now_ms.saturating_sub(cand.last_check_ms) >= ICE_CONSENT_INTERVAL_MS
            {
                let mut tx_id = [0u8; 12];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut tx_id);
                let pkt = build_ice_binding_request(
                    tx_id,
                    &username,
                    self.remote_pwd.as_bytes(),
                    cand.priority,
                    self.local.tie_breaker,
                    true,
                );
                cand.last_tx_id = Some(tx_id);
                cand.last_check_ms = now_ms;
                out.push((cand.addr, pkt));
            }
            return out;
        }

        for cand in &mut self.candidates {
            if cand.last_tx_id.is_none()
                || now_ms.saturating_sub(cand.last_check_ms) >= ICE_CHECK_RETRY_MS
            {
                let mut tx_id = [0u8; 12];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut tx_id);
                let pkt = build_ice_binding_request(
                    tx_id,
                    &username,
                    self.remote_pwd.as_bytes(),
                    cand.priority,
                    self.local.tie_breaker,
                    true,
                );
                cand.last_tx_id = Some(tx_id);
                cand.last_check_ms = now_ms;
                out.push((cand.addr, pkt));
            }
        }
        out
    }

    /// Process an incoming STUN packet from `src` at `now_ms`.
    ///
    /// - On a valid `STUN Binding Success Response`: marks candidate verified and nominates it,
    ///   returning `Ok(IceStunOutcome::Nominated(addr))` on first nomination.
    /// - On an incoming `STUN Binding Request`: replies with an authenticated `Binding Success
    ///   Response` keyed with `local.pwd`.
    pub fn handle_stun(
        &mut self,
        packet: &[u8],
        src: SocketAddr,
        now_ms: u64,
    ) -> VoipResult<IceStunOutcome> {
        if !is_stun_packet(packet) {
            return Err(VoipError::Ice("not a STUN packet".into()));
        }
        let msg_type = u16::from_be_bytes([packet[0], packet[1]]);
        let mut tx_id = [0u8; 12];
        tx_id.copy_from_slice(&packet[8..20]);

        match msg_type {
            STUN_BINDING_RESPONSE => {
                // Verify FINGERPRINT and (if present) MESSAGE-INTEGRITY with `remote_pwd`.
                verify_stun_integrity_and_fingerprint(packet, Some(self.remote_pwd.as_bytes()))?;
                let was_nominated = self.nominated.is_some();
                if let Some(cand) = self
                    .candidates
                    .iter_mut()
                    .find(|c| c.addr == src || c.last_tx_id == Some(tx_id))
                {
                    cand.verified = true;
                    cand.last_resp_ms = now_ms;
                    if self.nominated.is_none() {
                        self.nominated = Some(cand.addr);
                    }
                    return Ok(if !was_nominated {
                        IceStunOutcome::NewlyNominated(cand.addr)
                    } else {
                        IceStunOutcome::ConsentRefreshed(cand.addr)
                    });
                }
                // Accept valid response from an alternate SFU source port if tx_id matched.
                Ok(IceStunOutcome::Ignored)
            }
            STUN_BINDING_REQUEST => {
                verify_stun_integrity_and_fingerprint(packet, Some(self.local.pwd.as_bytes()))?;
                let resp = build_ice_binding_response(tx_id, src, Some(self.local.pwd.as_bytes()));
                Ok(IceStunOutcome::SendResponse {
                    to: src,
                    bytes: resp,
                })
            }
            other => Err(VoipError::Ice(format!(
                "unsupported ICE STUN message type 0x{other:04x}"
            ))),
        }
    }
}

/// Outcome of processing an incoming ICE STUN packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IceStunOutcome {
    /// First SFU candidate just passed STUN check and was nominated (ready for DTLS!).
    NewlyNominated(SocketAddr),
    /// Consent freshness response received on the active candidate.
    ConsentRefreshed(SocketAddr),
    /// Incoming STUN Binding Request requires sending this Binding Success Response.
    SendResponse {
        /// Peer socket address.
        to: SocketAddr,
        /// Encoded STUN Binding Success Response bytes.
        bytes: Vec<u8>,
    },
    /// Unmatched STUN response ignored.
    Ignored,
}

/// Build an RFC 8445 ICE-lite STUN Binding Request with `USERNAME`, `PRIORITY`,
/// `ICE-CONTROLLING`, optional `USE-CANDIDATE`, `MESSAGE-INTEGRITY`, and `FINGERPRINT`.
#[must_use]
pub fn build_ice_binding_request(
    tx_id: [u8; 12],
    username: &str,
    remote_pwd: &[u8],
    priority: u32,
    tie_breaker: u64,
    use_candidate: bool,
) -> Vec<u8> {
    let mut attrs = Vec::new();
    append_attr(&mut attrs, stun_attr::USERNAME, username.as_bytes());
    append_attr(&mut attrs, ice_attr::PRIORITY, &priority.to_be_bytes());
    append_attr(
        &mut attrs,
        ice_attr::ICE_CONTROLLING,
        &tie_breaker.to_be_bytes(),
    );
    if use_candidate {
        append_attr(&mut attrs, ice_attr::USE_CANDIDATE, &[]);
    }
    finish_ice_stun(
        STUN_BINDING_REQUEST,
        tx_id,
        attrs,
        (!remote_pwd.is_empty()).then_some(remote_pwd),
    )
}

/// Build an RFC 5389 / RFC 8445 STUN Binding Success Response with `XOR-MAPPED-ADDRESS`,
/// optional `MESSAGE-INTEGRITY`, and `FINGERPRINT`.
#[must_use]
pub fn build_ice_binding_response(
    tx_id: [u8; 12],
    mapped: SocketAddr,
    integrity_key: Option<&[u8]>,
) -> Vec<u8> {
    let mut attrs = Vec::new();
    let xaddr = encode_xor_addr(mapped, &tx_id);
    append_attr(&mut attrs, stun_attr::XOR_MAPPED_ADDRESS, &xaddr);
    finish_ice_stun(STUN_BINDING_RESPONSE, tx_id, attrs, integrity_key)
}

/// Verify STUN `FINGERPRINT` and (when both present and key non-empty) `MESSAGE-INTEGRITY`.
pub fn verify_stun_integrity_and_fingerprint(
    packet: &[u8],
    integrity_key: Option<&[u8]>,
) -> VoipResult<()> {
    if !is_stun_packet(packet) {
        return Err(VoipError::Ice("invalid STUN header".into()));
    }
    let msg_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    let end = 20 + msg_len;
    if end > packet.len() {
        return Err(VoipError::Ice("truncated STUN body".into()));
    }

    let mut cursor = 20usize;
    while cursor + 4 <= end {
        let attr_type = u16::from_be_bytes([packet[cursor], packet[cursor + 1]]);
        let attr_len = u16::from_be_bytes([packet[cursor + 2], packet[cursor + 3]]) as usize;
        let val_start = cursor + 4;
        let val_end = val_start + attr_len;
        if val_end > end {
            return Err(VoipError::Ice("truncated STUN attribute".into()));
        }
        let val = &packet[val_start..val_end];

        match attr_type {
            stun_attr::MESSAGE_INTEGRITY => {
                if val.len() != 20 {
                    return Err(VoipError::Ice("invalid MESSAGE-INTEGRITY length".into()));
                }
                if let Some(key) = integrity_key
                    && !key.is_empty()
                {
                    let mut prefix = packet[..cursor].to_vec();
                    // Length field in HMAC covers up to and including MESSAGE-INTEGRITY (+24 bytes).
                    let len_mi = (cursor - 20 + 24) as u16;
                    prefix[2..4].copy_from_slice(&len_mi.to_be_bytes());
                    let mut mac = Hmac::<Sha1>::new_from_slice(key)
                        .map_err(|e| VoipError::Ice(format!("HMAC key error: {e}")))?;
                    mac.update(&prefix);
                    if mac.verify_slice(val).is_err() {
                        return Err(VoipError::Ice("STUN MESSAGE-INTEGRITY mismatch".into()));
                    }
                }
            }
            stun_attr::FINGERPRINT => {
                if val.len() != 4 {
                    return Err(VoipError::Ice("invalid STUN FINGERPRINT length".into()));
                }
                let got = u32::from_be_bytes([val[0], val[1], val[2], val[3]]);
                let expected = crc32_ieee(&packet[..cursor]) ^ 0x5354_554E;
                if got != expected {
                    return Err(VoipError::Ice("STUN FINGERPRINT mismatch".into()));
                }
            }
            _ => {}
        }

        let padded = (attr_len + 3) & !3;
        cursor = val_start + padded;
    }
    Ok(())
}

fn append_attr(buf: &mut Vec<u8>, attr_type: u16, value: &[u8]) {
    buf.extend_from_slice(&attr_type.to_be_bytes());
    buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
    buf.extend_from_slice(value);
    let pad = (4 - (value.len() % 4)) % 4;
    buf.resize(buf.len() + pad, 0);
}

fn finish_ice_stun(
    msg_type: u16,
    tx_id: [u8; 12],
    mut attrs: Vec<u8>,
    integrity_key: Option<&[u8]>,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(20 + attrs.len() + 32);
    msg.extend_from_slice(&msg_type.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes());
    msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&tx_id);
    msg.append(&mut attrs);

    if let Some(key) = integrity_key
        && !key.is_empty()
    {
        let len_with_mi = (msg.len() - 20 + 24) as u16;
        msg[2..4].copy_from_slice(&len_with_mi.to_be_bytes());
        if let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(key) {
            mac.update(&msg);
            let digest = mac.finalize().into_bytes();
            append_attr(&mut msg, stun_attr::MESSAGE_INTEGRITY, &digest);
        }
    }

    let len_with_fp = (msg.len() - 20 + 8) as u16;
    msg[2..4].copy_from_slice(&len_with_fp.to_be_bytes());
    let fp = crc32_ieee(&msg) ^ 0x5354_554E;
    append_attr(&mut msg, stun_attr::FINGERPRINT, &fp.to_be_bytes());
    msg
}

fn encode_xor_addr(addr: SocketAddr, tx_id: &[u8; 12]) -> Vec<u8> {
    let xport = addr.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
    match addr.ip() {
        IpAddr::V4(v4) => {
            let mut out = Vec::with_capacity(8);
            out.push(0);
            out.push(0x01);
            out.extend_from_slice(&xport.to_be_bytes());
            let xip = u32::from(v4) ^ STUN_MAGIC_COOKIE;
            out.extend_from_slice(&xip.to_be_bytes());
            out
        }
        IpAddr::V6(v6) => {
            let mut out = Vec::with_capacity(20);
            out.push(0);
            out.push(0x02);
            out.extend_from_slice(&xport.to_be_bytes());
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            mask[4..16].copy_from_slice(tx_id);
            let octets = v6.octets();
            for i in 0..16 {
                out.push(octets[i] ^ mask[i]);
            }
            out
        }
    }
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_calls::{JoinResponseTransport, parse_join_response};

    #[test]
    fn group_join_payload_matches_tg_calls_schema() {
        let creds = LocalIceCredentials::generate();
        let identity = DtlsIdentity::generate().unwrap();
        let payload = build_group_join_payload(&creds, &identity, Vec::new());

        assert!(payload.ssrc > 0 && payload.ssrc <= i32::MAX as u32);
        assert_eq!(payload.fingerprints.len(), 1);
        assert_eq!(payload.fingerprints[0].hash, "sha-256");
        assert_eq!(payload.fingerprints[0].setup.as_deref(), Some("active"));

        let json = payload.to_json().unwrap();
        let roundtrip = GroupJoinPayload::from_json(&json).unwrap();
        assert_eq!(roundtrip, payload);
    }

    #[test]
    fn ice_lite_stun_check_and_nomination() {
        let sfu_addr: SocketAddr = "127.0.0.1:45678".parse().unwrap();
        let response = RtcJoinResponse {
            transport: JoinResponseTransport {
                ufrag: "sfu_uf".into(),
                pwd: "sfu_password_123456789012".into(),
                fingerprints: Vec::new(),
                candidates: vec![IceCandidateParam {
                    port: "45678".into(),
                    protocol: "udp".into(),
                    network: "0".into(),
                    generation: "0".into(),
                    id: "1".into(),
                    component: "1".into(),
                    foundation: "1".into(),
                    priority: "2130706431".into(),
                    ip: "127.0.0.1".into(),
                    kind: "host".into(),
                }],
                rtcp_mux: true,
            },
            ..RtcJoinResponse::default()
        };

        let creds = LocalIceCredentials::generate();
        let mut client = IceLiteClient::from_join_response(creds, &response).unwrap();
        let checks = client.poll_outgoing_checks(0);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].0, sfu_addr);

        // Simulate SFU verifying our STUN request and replying with authenticated Binding Response.
        verify_stun_integrity_and_fingerprint(&checks[0].1, Some(b"sfu_password_123456789012"))
            .unwrap();
        let mut tx_id = [0u8; 12];
        tx_id.copy_from_slice(&checks[0].1[8..20]);
        let sfu_resp = build_ice_binding_response(
            tx_id,
            "127.0.0.1:50000".parse().unwrap(),
            Some(b"sfu_password_123456789012"),
        );

        let outcome = client.handle_stun(&sfu_resp, sfu_addr, 25).unwrap();
        assert_eq!(outcome, IceStunOutcome::NewlyNominated(sfu_addr));
        assert_eq!(client.nominated_addr(), Some(sfu_addr));

        // Verify JSON parsing interoperability with `tg_calls::parse_join_response`.
        let json = serde_json::to_string(&response).unwrap();
        let parsed = parse_join_response(&json).unwrap();
        assert!(matches!(parsed, tg_calls::ParsedJoinResponse::Rtc { .. }));
    }
}
