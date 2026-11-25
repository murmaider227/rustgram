//! Pure-Rust DTLS 1.2 + SRTP engine for Telegram group calls (SFU WebRTC transport).
//!
//! Reference:
//! - RFC 5764 (DTLS-SRTP), RFC 3711 (SRTP), RFC 7714 (AES-GCM for SRTP), RFC 7983 (demux)
//! - `TelegramMessenger/tgcalls` (`GroupInstanceCustomImpl.cpp` DTLS fingerprint & SRTP setup)
//!
//! Stack selection (zero CMake, zero OpenSSL, zero `aws-lc-rs`):
//! - **DTLS 1.2**: `dimpl` (`rust-crypto` feature) — pure-Rust, `forbid(unsafe_code)`, Sans-IO
//!   DTLS 1.2/1.3 engine with RFC 5764 `use_srtp` key export.
//! - **SRTP / SRTCP**: `webrtc-srtp` (default pure-Rust backend) — RFC 3711 / RFC 7714
//!   `SRTP_AES128_CM_HMAC_SHA1_80`, `SRTP_AEAD_AES_128_GCM`, `SRTP_AEAD_AES_256_GCM`.
//! - **Certificate generation**: Pure-Rust ephemeral ECDSA P-256 self-signed X.509 v3 DER
//!   certificate + PKCS#8 private key built directly with `p256`.

use std::sync::Arc;
use std::time::Instant;

use dimpl::crypto::rust_crypto;
use dimpl::{Config, Dtls, DtlsCertificate, Output, SrtpProfile};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{DerSignature, SigningKey};
use p256::pkcs8::EncodePrivateKey;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tg_calls::DtlsFingerprint;
use webrtc_srtp::context::Context as SrtpContext;
use webrtc_srtp::protection_profile::ProtectionProfile;

use crate::error::{VoipError, VoipResult};

/// RFC 7983 first-byte demultiplexing classification for WebRTC UDP datagrams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRtcPacketKind {
    /// STUN / ICE connectivity check (`0..=3`).
    Stun,
    /// DTLS handshake or alert record (`20..=63`).
    Dtls,
    /// SRTP or SRTCP media/control packet (`128..=191`).
    SrtpOrSrtcp,
    /// Unrecognized first byte.
    Unknown,
}

/// Classify a UDP datagram received on a WebRTC ICE/DTLS/SRTP socket per RFC 7983 §7.
#[must_use]
pub fn classify_webrtc_packet(data: &[u8]) -> WebRtcPacketKind {
    let Some(&first) = data.first() else {
        return WebRtcPacketKind::Unknown;
    };
    match first {
        0..=3 => WebRtcPacketKind::Stun,
        20..=63 => WebRtcPacketKind::Dtls,
        128..=191 => WebRtcPacketKind::SrtpOrSrtcp,
        _ => WebRtcPacketKind::Unknown,
    }
}

/// Returns `true` if an SRTP/SRTCP packet (first byte `128..=191`) is RTCP per RFC 5761 §4
/// (second byte in `192..=223`, i.e., PT `64..=95` without/with marker bit).
#[must_use]
pub fn is_rtcp_packet(data: &[u8]) -> bool {
    data.len() >= 2 && (192..=223).contains(&data[1])
}

/// Self-signed ephemeral ECDSA P-256 DTLS identity (X.509 v3 DER + PKCS#8 DER + SHA-256 fingerprint).
#[derive(Debug, Clone)]
pub struct DtlsIdentity {
    /// DER-encoded X.509 v3 leaf certificate.
    pub cert_der: Vec<u8>,
    /// PKCS#8 DER-encoded ECDSA P-256 private key.
    pub key_der: Vec<u8>,
    /// Uppercase colon-separated SHA-256 fingerprint (e.g. `"AA:BB:..."`).
    pub fingerprint_sha256: String,
}

impl DtlsIdentity {
    /// Generate a fresh ephemeral ECDSA P-256 self-signed X.509 v3 certificate in pure Rust.
    pub fn generate() -> VoipResult<Self> {
        // Generate random P-256 scalar via `rand::rngs::OsRng` (compatible across rand versions).
        let signing_key = loop {
            let mut raw = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut raw);
            if let Ok(key) = SigningKey::from_slice(&raw) {
                break key;
            }
        };

        let pkcs8_doc = signing_key
            .to_pkcs8_der()
            .map_err(|e| VoipError::Dtls(format!("PKCS#8 encoding failed: {e}")))?;
        let key_der = pkcs8_doc.as_bytes().to_vec();

        let pub_uncompressed = signing_key
            .verifying_key()
            .to_sec1_point(false)
            .as_bytes()
            .to_vec();

        // Build X.509 v3 TBSCertificate DER.
        let mut serial = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut serial);
        serial[0] &= 0x7F; // positive ASN.1 INTEGER
        if serial[0] == 0 {
            serial[0] = 1;
        }

        let tbs_der = build_x509_tbs_p256(&serial, &pub_uncompressed);
        let sig: DerSignature = signing_key.sign(&tbs_der);
        let cert_der = build_x509_cert_der(&tbs_der, sig.as_bytes());
        let fingerprint_sha256 = sha256_fingerprint(&cert_der);

        Ok(Self {
            cert_der,
            key_der,
            fingerprint_sha256,
        })
    }

    /// Build a `tg_calls::DtlsFingerprint` entry for [`tg_calls::GroupJoinPayload`].
    #[must_use]
    pub fn to_group_fingerprint(&self, setup: &str) -> DtlsFingerprint {
        DtlsFingerprint {
            hash: "sha-256".to_string(),
            setup: Some(setup.to_string()),
            fingerprint: self.fingerprint_sha256.clone(),
        }
    }

    fn to_dimpl_cert(&self) -> DtlsCertificate {
        DtlsCertificate {
            certificate: self.cert_der.clone(),
            private_key: self.key_der.clone(),
        }
    }
}

/// Compute RFC 8122 uppercase colon-separated SHA-256 fingerprint of a DER certificate.
#[must_use]
pub fn sha256_fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut out = String::with_capacity(digest.len() * 3 - 1);
    for (i, b) in digest.iter().enumerate() {
        if i > 0 {
            out.push(':');
        }
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02X}");
    }
    out
}

/// Normalize a colon-separated hex fingerprint for case-insensitive comparison.
fn normalize_fingerprint(fp: &str) -> String {
    fp.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Returns `true` if `fingerprints` contains at least one non-empty SHA-256 DTLS fingerprint.
#[must_use]
pub(crate) fn has_valid_sha256_fingerprint(fingerprints: &[DtlsFingerprint]) -> bool {
    fingerprints.iter().any(|fp| {
        fp.hash.eq_ignore_ascii_case("sha-256")
            && !normalize_fingerprint(&fp.fingerprint).is_empty()
    })
}

/// Outputs produced by draining [`DtlsSrtpSession::poll_outputs`].
#[derive(Debug, Default)]
pub struct DtlsPollOutput {
    /// Outgoing DTLS UDP datagrams to transmit to the remote peer/SFU.
    pub outgoing_packets: Vec<Vec<u8>>,
    /// Next retransmission timeout deadline, if any.
    pub next_timeout: Option<Instant>,
    /// `true` if the DTLS-SRTP handshake just completed and SRTP keys are active.
    pub just_connected: bool,
    /// Decrypted DTLS application data payloads (if any).
    pub app_data: Vec<Vec<u8>>,
    /// `true` if peer sent `close_notify`.
    pub closed: bool,
}

/// Combined Sans-IO DTLS 1.2 handshake + SRTP/SRTCP media encryption session.
pub struct DtlsSrtpSession {
    dtls: Dtls,
    is_client: bool,
    expected_fingerprints: Vec<DtlsFingerprint>,
    peer_fingerprint: Option<String>,
    peer_verified: bool,
    connected: bool,
    negotiated_profile: Option<SrtpProfile>,
    tx_srtp: Option<SrtpContext>,
    rx_srtp: Option<SrtpContext>,
    scratch: Vec<u8>,
}

impl DtlsSrtpSession {
    /// Create a new DTLS 1.2 session (`is_client = true` for active Telegram SFU client,
    /// `is_client = false` for passive server in loopback tests).
    ///
    /// Fails closed if `expected_fingerprints` does not contain at least one SHA-256 peer
    /// fingerprint.
    pub fn new(
        identity: &DtlsIdentity,
        is_client: bool,
        expected_fingerprints: Vec<DtlsFingerprint>,
        now: Instant,
    ) -> VoipResult<Self> {
        if !has_valid_sha256_fingerprint(&expected_fingerprints) {
            return Err(VoipError::Dtls(
                "DTLS requires at least one expected SHA-256 peer fingerprint".into(),
            ));
        }
        let config = Config::builder()
            .with_crypto_provider(rust_crypto::default_provider())
            .use_server_cookie(false)
            .require_client_certificate(true)
            .build()
            .map_err(|e| VoipError::Dtls(format!("dimpl Config::build failed: {e}")))?;

        let mut dtls = Dtls::new_12(Arc::new(config), identity.to_dimpl_cert(), now);
        dtls.set_active(is_client);
        // Always call `handle_timeout(now)` once so dimpl initializes `random` (server)
        // and primes the initial ClientHello flight (client).
        dtls.handle_timeout(now)
            .map_err(|e| VoipError::Dtls(format!("DTLS initial handle_timeout failed: {e}")))?;

        Ok(Self {
            dtls,
            is_client,
            expected_fingerprints,
            peer_fingerprint: None,
            peer_verified: false,
            connected: false,
            negotiated_profile: None,
            tx_srtp: None,
            rx_srtp: None,
            scratch: vec![0u8; 4096],
        })
    }

    /// Returns `true` once DTLS 1.2 handshake succeeded, peer fingerprint matched (if configured),
    /// and SRTP send/receive contexts are initialized.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.connected && self.tx_srtp.is_some() && self.rx_srtp.is_some()
    }

    /// Observed SHA-256 fingerprint of the peer's leaf certificate, once received.
    #[must_use]
    pub fn peer_fingerprint(&self) -> Option<&str> {
        self.peer_fingerprint.as_deref()
    }

    /// Negotiated RFC 5764 SRTP protection profile, once exported.
    #[must_use]
    pub const fn negotiated_profile(&self) -> Option<SrtpProfile> {
        self.negotiated_profile
    }

    /// Feed an incoming DTLS UDP datagram into the handshake/record engine.
    pub fn handle_packet(&mut self, packet: &[u8]) -> VoipResult<()> {
        self.dtls
            .handle_packet(packet)
            .map_err(|e| VoipError::Dtls(format!("DTLS handle_packet failed: {e}")))
    }

    /// Advance DTLS retransmission timers.
    pub fn handle_timeout(&mut self, now: Instant) -> VoipResult<()> {
        self.dtls
            .handle_timeout(now)
            .map_err(|e| VoipError::Dtls(format!("DTLS handle_timeout failed: {e}")))
    }

    /// Poll all pending DTLS outputs (outgoing records, peer cert verification, SRTP key export).
    pub fn poll_outputs(&mut self) -> VoipResult<DtlsPollOutput> {
        let mut out = DtlsPollOutput::default();
        loop {
            match self.dtls.poll_output(&mut self.scratch) {
                Output::Packet(bytes) => {
                    out.outgoing_packets.push(bytes.to_vec());
                }
                Output::Timeout(deadline) => {
                    out.next_timeout = Some(deadline);
                    break;
                }
                Output::PeerCert(cert_der) => {
                    let fp = sha256_fingerprint(cert_der);
                    self.verify_peer_fingerprint(&fp)?;
                    self.peer_fingerprint = Some(fp);
                    self.peer_verified = true;
                }
                Output::KeyingMaterial(km, profile) => {
                    let (tx, rx) = derive_srtp_contexts(&km, profile, self.is_client)?;
                    self.tx_srtp = Some(tx);
                    self.rx_srtp = Some(rx);
                    self.negotiated_profile = Some(profile);
                }
                Output::Connected => {
                    if !self.peer_verified {
                        return Err(VoipError::Dtls(
                            "DTLS connected without peer certificate verification".into(),
                        ));
                    }
                    self.connected = true;
                    out.just_connected = true;
                }
                Output::ApplicationData(data) => {
                    out.app_data.push(data.to_vec());
                }
                Output::CloseNotify => {
                    out.closed = true;
                    break;
                }
                _ => break,
            }
        }
        Ok(out)
    }

    /// Encrypt and authenticate a plaintext RTP packet into SRTP.
    pub fn protect_rtp(&mut self, rtp_bytes: &[u8]) -> VoipResult<Vec<u8>> {
        let ctx = self
            .tx_srtp
            .as_mut()
            .ok_or_else(|| VoipError::Srtp("SRTP transmit context not ready".into()))?;
        let encrypted = ctx
            .encrypt_rtp(rtp_bytes)
            .map_err(|e| VoipError::Srtp(format!("encrypt_rtp failed: {e}")))?;
        Ok(encrypted.to_vec())
    }

    /// Authenticate and decrypt an incoming SRTP packet into plaintext RTP.
    pub fn unprotect_rtp(&mut self, srtp_bytes: &[u8]) -> VoipResult<Vec<u8>> {
        let ctx = self
            .rx_srtp
            .as_mut()
            .ok_or_else(|| VoipError::Srtp("SRTP receive context not ready".into()))?;
        let decrypted = ctx
            .decrypt_rtp(srtp_bytes)
            .map_err(|e| VoipError::Srtp(format!("decrypt_rtp failed: {e}")))?;
        Ok(decrypted.to_vec())
    }

    /// Encrypt and authenticate a plaintext RTCP packet into SRTCP.
    pub fn protect_rtcp(&mut self, rtcp_bytes: &[u8]) -> VoipResult<Vec<u8>> {
        let ctx = self
            .tx_srtp
            .as_mut()
            .ok_or_else(|| VoipError::Srtp("SRTCP transmit context not ready".into()))?;
        let encrypted = ctx
            .encrypt_rtcp(rtcp_bytes)
            .map_err(|e| VoipError::Srtp(format!("encrypt_rtcp failed: {e}")))?;
        Ok(encrypted.to_vec())
    }

    /// Authenticate and decrypt an incoming SRTCP packet into plaintext RTCP.
    pub fn unprotect_rtcp(&mut self, srtcp_bytes: &[u8]) -> VoipResult<Vec<u8>> {
        let ctx = self
            .rx_srtp
            .as_mut()
            .ok_or_else(|| VoipError::Srtp("SRTCP receive context not ready".into()))?;
        let decrypted = ctx
            .decrypt_rtcp(srtcp_bytes)
            .map_err(|e| VoipError::Srtp(format!("decrypt_rtcp failed: {e}")))?;
        Ok(decrypted.to_vec())
    }

    fn verify_peer_fingerprint(&self, observed_sha256: &str) -> VoipResult<()> {
        if self.expected_fingerprints.is_empty() {
            return Err(VoipError::Dtls(
                "remote DTLS fingerprint list is empty".into(),
            ));
        }
        let observed_norm = normalize_fingerprint(observed_sha256);
        let matched = self.expected_fingerprints.iter().any(|fp| {
            fp.hash.eq_ignore_ascii_case("sha-256")
                && normalize_fingerprint(&fp.fingerprint) == observed_norm
        });
        if !matched {
            return Err(VoipError::Dtls(format!(
                "remote DTLS SHA-256 fingerprint mismatch (observed {observed_sha256})"
            )));
        }
        Ok(())
    }
}

/// Split RFC 5764 §4.2 exported keying material into `(tx_srtp, rx_srtp)` contexts.
fn derive_srtp_contexts(
    km: &[u8],
    profile: SrtpProfile,
    is_client: bool,
) -> VoipResult<(SrtpContext, SrtpContext)> {
    let (wp_profile, key_len, salt_len) = match profile {
        SrtpProfile::AES128_CM_SHA1_80 => (ProtectionProfile::Aes128CmHmacSha1_80, 16, 14),
        SrtpProfile::AEAD_AES_128_GCM => (ProtectionProfile::AeadAes128Gcm, 16, 12),
        SrtpProfile::AEAD_AES_256_GCM => (ProtectionProfile::AeadAes256Gcm, 32, 12),
        other => {
            return Err(VoipError::Srtp(format!(
                "unsupported SRTP profile {other:?}"
            )));
        }
    };

    let needed = (key_len + salt_len) * 2;
    if km.len() < needed {
        return Err(VoipError::Srtp(format!(
            "exported DTLS-SRTP keying material too short: {} < {needed}",
            km.len()
        )));
    }

    // RFC 5764 §4.2 layout:
    // client_write_SRTP_master_key || server_write_SRTP_master_key
    // || client_write_SRTP_master_salt || server_write_SRTP_master_salt
    let client_key = &km[0..key_len];
    let server_key = &km[key_len..key_len * 2];
    let salt_base = key_len * 2;
    let client_salt = &km[salt_base..salt_base + salt_len];
    let server_salt = &km[salt_base + salt_len..salt_base + salt_len * 2];

    let (tx_key, tx_salt, rx_key, rx_salt) = if is_client {
        (client_key, client_salt, server_key, server_salt)
    } else {
        (server_key, server_salt, client_key, client_salt)
    };

    let tx_ctx = SrtpContext::new(tx_key, tx_salt, wp_profile, None, None)
        .map_err(|e| VoipError::Srtp(format!("tx SrtpContext::new failed: {e}")))?;

    let rx_ctx = SrtpContext::new(
        rx_key,
        rx_salt,
        wp_profile,
        Some(webrtc_srtp::option::srtp_replay_protection(128)),
        Some(webrtc_srtp::option::srtcp_replay_protection(128)),
    )
    .map_err(|e| VoipError::Srtp(format!("rx SrtpContext::new failed: {e}")))?;

    Ok((tx_ctx, rx_ctx))
}

// ---------------------------------------------------------------------------
// Minimal Pure-Rust ASN.1 DER Self-Signed X.509 v3 Certificate Builder (P-256)
// ---------------------------------------------------------------------------

fn der_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xFF {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push((len & 0xFF) as u8);
    }
}

fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + content.len());
    out.push(tag);
    der_len(content.len(), &mut out);
    out.extend_from_slice(content);
    out
}

fn der_seq(parts: &[&[u8]]) -> Vec<u8> {
    let body: Vec<u8> = parts.iter().flat_map(|p| p.iter().copied()).collect();
    der_tlv(0x30, &body)
}

fn der_set(parts: &[&[u8]]) -> Vec<u8> {
    let body: Vec<u8> = parts.iter().flat_map(|p| p.iter().copied()).collect();
    der_tlv(0x31, &body)
}

fn der_bit_string(bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(1 + bytes.len());
    body.push(0x00); // 0 unused bits
    body.extend_from_slice(bytes);
    der_tlv(0x03, &body)
}

fn der_ecdsa_sha256_alg_id() -> Vec<u8> {
    // OID 1.2.840.10045.4.3.2 (ecdsa-with-SHA256)
    let oid = der_tlv(0x06, &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02]);
    der_seq(&[&oid])
}

fn der_cn_name(cn: &str) -> Vec<u8> {
    // OID 2.5.4.3 (commonName)
    let oid_cn = der_tlv(0x06, &[0x55, 0x04, 0x03]);
    let val = der_tlv(0x0C, cn.as_bytes()); // UTF8String
    let atv = der_seq(&[&oid_cn, &val]);
    let rdn = der_set(&[&atv]);
    der_seq(&[&rdn])
}

fn build_x509_tbs_p256(serial: &[u8; 16], pub_uncompressed: &[u8]) -> Vec<u8> {
    // [0] EXPLICIT Version ::= INTEGER { v3(2) }
    let version_int = der_tlv(0x02, &[0x02]);
    let version = der_tlv(0xA0, &version_int);
    let serial_num = der_tlv(0x02, serial);
    let sig_alg = der_ecdsa_sha256_alg_id();
    let name = der_cn_name("tg-voip");

    // Validity: 2020-01-01T00:00:00Z .. 2049-12-31T23:59:59Z (UTCTime)
    let not_before = der_tlv(0x17, b"200101000000Z");
    let not_after = der_tlv(0x17, b"491231235959Z");
    let validity = der_seq(&[&not_before, &not_after]);

    // SubjectPublicKeyInfo: id-ecPublicKey (1.2.840.10045.2.1) + prime256v1 (1.2.840.10045.3.1.7)
    let oid_ec = der_tlv(0x06, &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01]);
    let oid_p256 = der_tlv(0x06, &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07]);
    let spki_alg = der_seq(&[&oid_ec, &oid_p256]);
    let spki_key = der_bit_string(pub_uncompressed);
    let spki = der_seq(&[&spki_alg, &spki_key]);

    der_seq(&[
        &version,
        &serial_num,
        &sig_alg,
        &name,
        &validity,
        &name,
        &spki,
    ])
}

fn build_x509_cert_der(tbs_der: &[u8], sig_der: &[u8]) -> Vec<u8> {
    let sig_alg = der_ecdsa_sha256_alg_id();
    let sig_bits = der_bit_string(sig_der);
    der_seq(&[tbs_der, &sig_alg, &sig_bits])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::{OPUS_RTP_PAYLOAD_TYPE, RtpHeader};

    #[test]
    fn dtls12_srtp_handshake_and_rtp_roundtrip() {
        let client_id = DtlsIdentity::generate().unwrap();
        let server_id = DtlsIdentity::generate().unwrap();

        let now = Instant::now();
        let mut client = DtlsSrtpSession::new(
            &client_id,
            true,
            vec![server_id.to_group_fingerprint("passive")],
            now,
        )
        .unwrap();
        let mut server = DtlsSrtpSession::new(
            &server_id,
            false,
            vec![client_id.to_group_fingerprint("active")],
            now,
        )
        .unwrap();

        // Pump DTLS 1.2 flights back and forth until both endpoints complete handshake + SRTP export.
        for _ in 0..32 {
            let c_out = client.poll_outputs().unwrap();
            for pkt in c_out.outgoing_packets {
                assert_eq!(classify_webrtc_packet(&pkt), WebRtcPacketKind::Dtls);
                server.handle_packet(&pkt).unwrap();
            }
            let s_out = server.poll_outputs().unwrap();
            for pkt in s_out.outgoing_packets {
                assert_eq!(classify_webrtc_packet(&pkt), WebRtcPacketKind::Dtls);
                client.handle_packet(&pkt).unwrap();
            }
            if client.is_ready() && server.is_ready() {
                break;
            }
        }

        assert!(client.is_ready(), "DTLS client must complete handshake");
        assert!(server.is_ready(), "DTLS server must complete handshake");
        assert_eq!(
            client.peer_fingerprint(),
            Some(server_id.fingerprint_sha256.as_str())
        );
        assert_eq!(
            server.peer_fingerprint(),
            Some(client_id.fingerprint_sha256.as_str())
        );

        // Encrypt RTP on client -> decrypt on server, and vice versa.
        let hdr = RtpHeader {
            marker: true,
            payload_type: OPUS_RTP_PAYLOAD_TYPE,
            sequence_number: 1,
            timestamp: 960,
            ssrc: 0x0102_0304,
        };
        let mut rtp = hdr.to_bytes().to_vec();
        rtp.extend_from_slice(&[0xF8, 0xAA, 0xBB, 0xCC, 0xDD]);

        let srtp_c2s = client.protect_rtp(&rtp).unwrap();
        assert_ne!(srtp_c2s, rtp);
        assert_eq!(
            classify_webrtc_packet(&srtp_c2s),
            WebRtcPacketKind::SrtpOrSrtcp
        );
        let dec_c2s = server.unprotect_rtp(&srtp_c2s).unwrap();
        assert_eq!(dec_c2s, rtp);

        // Replay of the exact same SRTP packet must be rejected by server's replay detector.
        assert!(server.unprotect_rtp(&srtp_c2s).is_err());

        // Server -> client direction.
        let srtp_s2c = server.protect_rtp(&rtp).unwrap();
        let dec_s2c = client.unprotect_rtp(&srtp_s2c).unwrap();
        assert_eq!(dec_s2c, rtp);
    }

    #[test]
    fn dtls12_rejects_mismatched_peer_fingerprint() {
        let client_id = DtlsIdentity::generate().unwrap();
        let server_id = DtlsIdentity::generate().unwrap();
        let wrong_fp = DtlsFingerprint {
            hash: "sha-256".into(),
            setup: Some("passive".into()),
            fingerprint: "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF".into(),
        };

        let now = Instant::now();
        let mut client = DtlsSrtpSession::new(&client_id, true, vec![wrong_fp], now).unwrap();
        let mut server = DtlsSrtpSession::new(
            &server_id,
            false,
            vec![client_id.to_group_fingerprint("active")],
            now,
        )
        .unwrap();

        let mut mismatch_err = false;
        for _ in 0..16 {
            match client.poll_outputs() {
                Ok(c_out) => {
                    for pkt in c_out.outgoing_packets {
                        let _ = server.handle_packet(&pkt);
                    }
                }
                Err(_) => {
                    mismatch_err = true;
                    break;
                }
            }
            if let Ok(s_out) = server.poll_outputs() {
                for pkt in s_out.outgoing_packets {
                    if client.handle_packet(&pkt).is_err() {
                        mismatch_err = true;
                        break;
                    }
                }
            }
        }
        assert!(mismatch_err, "mismatched peer fingerprint must fail DTLS");
    }

    #[test]
    fn sec2c_w2_dtls_rejects_empty_fingerprint_list() {
        let id = DtlsIdentity::generate().unwrap();
        let now = Instant::now();

        let err = DtlsSrtpSession::new(&id, true, Vec::new(), now)
            .err()
            .expect("empty DTLS fingerprint list must be rejected");
        assert!(
            err.to_string().contains("fingerprint"),
            "unexpected error for empty fingerprint list: {err}"
        );

        let non_sha256 = vec![DtlsFingerprint {
            hash: "sha-1".into(),
            setup: Some("passive".into()),
            fingerprint: "AA:BB:CC:DD".into(),
        }];
        assert!(
            DtlsSrtpSession::new(&id, true, non_sha256, now).is_err(),
            "fingerprint list with no SHA-256 entry must be rejected"
        );
    }
}
