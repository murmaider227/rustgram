//! MTProto Obfuscated2 stream encryption (AES-256-CTR).
//!
//! Port of `TcpConnection::prepareConnectionStartPrefix` (`mtproto/connection_tcp.cpp`)
//! and `TcpSocket::isGoodStartNonce` (`mtproto/details/mtproto_tcp_socket.cpp`).

use aes::Aes256;
use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Size of the obfuscated2 connection start header (`kConnectionStartPrefixSize`).
pub const OBFUSCATED_HEADER_SIZE: usize = 64;

/// AES-256-CTR cipher with 128-bit big-endian counter increment, matching OpenSSL's
/// `CRYPTO_ctr128_encrypt` used by tdesktop's `aesCtrEncrypt`.
pub type Aes256Ctr = Ctr128BE<Aes256>;

/// Check whether a 64-byte random start nonce avoids all reserved TCP protocol prefixes.
///
/// Port of `TcpSocket::isGoodStartNonce` (`mtproto/details/mtproto_tcp_socket.cpp`).
#[must_use]
pub fn is_good_start_nonce(nonce: &[u8]) -> bool {
    if nonce.len() < 8 {
        return false;
    }
    let zero = nonce[0];
    let first = u32::from_le_bytes([nonce[0], nonce[1], nonce[2], nonce[3]]);
    let second = u32::from_le_bytes([nonce[4], nonce[5], nonce[6], nonce[7]]);

    const RESERVED_01: u8 = 0xEF;
    const RESERVED_11: u32 = 0x4441_4548; // "HEAD"
    const RESERVED_12: u32 = 0x5453_4F50; // "POST"
    const RESERVED_13: u32 = 0x2054_4547; // "GET "
    const RESERVED_14: u32 = 0xEEEE_EEEE; // Intermediate tag
    const RESERVED_15: u32 = 0xDDDD_DDDD; // Padded Intermediate tag
    const RESERVED_16: u32 = 0x0201_0316; // TLS ClientHello prefix
    const RESERVED_21: u32 = 0x0000_0000;

    zero != RESERVED_01
        && first != RESERVED_11
        && first != RESERVED_12
        && first != RESERVED_13
        && first != RESERVED_14
        && first != RESERVED_15
        && first != RESERVED_16
        && second != RESERVED_21
}

/// Derive a 32-byte AES-256 key from a 32-byte nonce slice and an optional 16-byte MTProxy secret.
///
/// Port of `Protocol::Version0::prepareKey` and `Protocol::Version1::prepareKey` in
/// `mtproto/connection_tcp.cpp`.
fn derive_key(source: &[u8; 32], secret: Option<&[u8; 16]>) -> [u8; 32] {
    match secret {
        Some(secret_key) => {
            let mut hasher = Sha256::new();
            hasher.update(source);
            hasher.update(secret_key);
            let digest = hasher.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&digest);
            out
        }
        None => *source,
    }
}

/// Client-side Obfuscated2 cipher pair (send encryptor + receive decryptor) and initial header.
pub struct Obfuscated2 {
    send_cipher: Aes256Ctr,
    recv_cipher: Aes256Ctr,
}

impl std::fmt::Debug for Obfuscated2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Obfuscated2").finish_non_exhaustive()
    }
}

impl Obfuscated2 {
    /// Generate a random valid 64-byte obfuscated2 init header and initialize the
    /// client-to-server and server-to-client AES-256-CTR ciphers.
    ///
    /// - `protocol_tag`: 4-byte framing protocol ID written at bytes `56..60` (e.g. `TAG_INTERMEDIATE`).
    /// - `dc_id`: protocol DC ID written as little-endian `i16` at bytes `60..62` (used by MTProxy;
    ///   negative for media DCs, `+10000` for test mode).
    /// - `secret`: optional 16-byte MTProxy secret key mixed into send/receive keys via SHA-256.
    #[must_use]
    pub fn generate(
        protocol_tag: [u8; 4],
        dc_id: i16,
        secret: Option<&[u8; 16]>,
    ) -> (Self, [u8; OBFUSCATED_HEADER_SIZE]) {
        let mut rng = rand::thread_rng();
        let mut nonce = [0u8; OBFUSCATED_HEADER_SIZE];
        loop {
            rng.fill_bytes(&mut nonce);
            if is_good_start_nonce(&nonce) {
                break;
            }
        }
        Self::from_nonce(nonce, protocol_tag, dc_id, secret)
    }

    /// Build an [`Obfuscated2`] cipher pair and wire init header from a caller-provided
    /// 64-byte `nonce`.
    ///
    /// Useful for deterministic known-answer tests and for fake-TLS where `isGoodStartNonce`
    /// is not required.
    #[must_use]
    pub fn from_nonce(
        mut nonce: [u8; OBFUSCATED_HEADER_SIZE],
        protocol_tag: [u8; 4],
        dc_id: i16,
        secret: Option<&[u8; 16]>,
    ) -> (Self, [u8; OBFUSCATED_HEADER_SIZE]) {
        // Send key & IV come from nonce[8..40] and nonce[40..56].
        let mut raw_send_key = [0u8; 32];
        raw_send_key.copy_from_slice(&nonce[8..40]);
        let mut send_iv = [0u8; 16];
        send_iv.copy_from_slice(&nonce[40..56]);
        let send_key = derive_key(&raw_send_key, secret);

        // Receive key & IV come from reversed nonce[8..56].
        let mut reversed = [0u8; 48];
        reversed.copy_from_slice(&nonce[8..56]);
        reversed.reverse();
        let mut raw_recv_key = [0u8; 32];
        raw_recv_key.copy_from_slice(&reversed[0..32]);
        let mut recv_iv = [0u8; 16];
        recv_iv.copy_from_slice(&reversed[32..48]);
        let recv_key = derive_key(&raw_recv_key, secret);

        // Write protocol tag at 56..60 and DC ID at 60..62.
        nonce[56..60].copy_from_slice(&protocol_tag);
        nonce[60..62].copy_from_slice(&dc_id.to_le_bytes());

        let mut header = [0u8; OBFUSCATED_HEADER_SIZE];
        header[..56].copy_from_slice(&nonce[..56]);

        let mut send_cipher = Aes256Ctr::new((&send_key).into(), (&send_iv).into());
        let recv_cipher = Aes256Ctr::new((&recv_key).into(), (&recv_iv).into());

        // Encrypt all 64 bytes of nonce so send_cipher advances by 64 bytes (4 AES blocks),
        // then copy only the encrypted last 8 bytes into the wire header.
        send_cipher.apply_keystream(&mut nonce);
        header[56..64].copy_from_slice(&nonce[56..64]);

        (
            Self {
                send_cipher,
                recv_cipher,
            },
            header,
        )
    }

    /// Encrypt outgoing bytes in place using the client-to-server AES-256-CTR stream.
    pub fn encrypt_outgoing(&mut self, data: &mut [u8]) {
        self.send_cipher.apply_keystream(data);
    }

    /// Decrypt incoming bytes in place using the server-to-client AES-256-CTR stream.
    pub fn decrypt_incoming(&mut self, data: &mut [u8]) {
        self.recv_cipher.apply_keystream(data);
    }

    /// Server-side counterpart of [`Self::from_nonce`]: parse a 64-byte client init header
    /// to recover `(protocol_tag, dc_id)` and the server's cipher pair
    /// (where server's `decrypt_incoming` matches client's `encrypt_outgoing` and vice versa).
    #[must_use]
    pub fn accept_client_header(
        header: &[u8; OBFUSCATED_HEADER_SIZE],
        secret: Option<&[u8; 16]>,
    ) -> (Self, [u8; 4], i16) {
        let mut raw_client_send_key = [0u8; 32];
        raw_client_send_key.copy_from_slice(&header[8..40]);
        let mut client_send_iv = [0u8; 16];
        client_send_iv.copy_from_slice(&header[40..56]);
        let client_send_key = derive_key(&raw_client_send_key, secret);

        let mut reversed = [0u8; 48];
        reversed.copy_from_slice(&header[8..56]);
        reversed.reverse();
        let mut raw_client_recv_key = [0u8; 32];
        raw_client_recv_key.copy_from_slice(&reversed[0..32]);
        let mut client_recv_iv = [0u8; 16];
        client_recv_iv.copy_from_slice(&reversed[32..48]);
        let client_recv_key = derive_key(&raw_client_recv_key, secret);

        let mut decrypted = *header;
        let mut server_recv_cipher =
            Aes256Ctr::new((&client_send_key).into(), (&client_send_iv).into());
        server_recv_cipher.apply_keystream(&mut decrypted);

        let server_send_cipher =
            Aes256Ctr::new((&client_recv_key).into(), (&client_recv_iv).into());

        let mut tag = [0u8; 4];
        tag.copy_from_slice(&decrypted[56..60]);
        let dc_id = i16::from_le_bytes([decrypted[60], decrypted[61]]);

        (
            Self {
                send_cipher: server_send_cipher,
                recv_cipher: server_recv_cipher,
            },
            tag,
            dc_id,
        )
    }
}
