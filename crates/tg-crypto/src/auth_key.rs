//! MTProto `AuthKey` and MTProto 2.0 / 1.0 message encryption & decryption.
//!
//! Port of `mtproto/mtproto_auth_key.h`, `mtproto/mtproto_auth_key.cpp`,
//! `mtproto/session_private.cpp`, `mtproto/details/mtproto_serialized_request.cpp`,
//! and `mtproto/details/mtproto_dc_key_binder.cpp`.

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::aes::{
    AES_BLOCK_SIZE, aes_ige_decrypt_in_place, aes_ige_encrypt_in_place, try_aes_ige_decrypt,
    try_aes_ige_encrypt,
};
use crate::error::CryptoError;
use crate::hash::{const_time_eq, sha1, sha256};
use crate::random::{random_array, random_bytes};

/// Size of an MTProto authorization key in bytes (2048 bits).
pub const AUTH_KEY_SIZE: usize = 256;

/// Minimum random padding length in MTProto 2.0 encrypted messages.
pub const MIN_PADDING_V2: usize = 12;

/// Maximum random padding length in MTProto 2.0 encrypted messages.
pub const MAX_PADDING_V2: usize = 1024;

/// Direction of an MTProto encrypted message, selecting the `x` offset in key derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Client-to-server (`x = 0`, `send = true` in tdesktop).
    Client,
    /// Server-to-client (`x = 8`, `send = false` in tdesktop).
    Server,
}

impl Side {
    /// Return the MTProto key-derivation byte offset `x` (`0` for Client, `8` for Server).
    pub const fn x(self) -> usize {
        match self {
            Self::Client => 0,
            Self::Server => 8,
        }
    }
}

/// A 256-byte (2048-bit) MTProto authorization key (or local storage key).
///
/// Port of `MTP::AuthKey` in `mtproto/mtproto_auth_key.h/.cpp`. Like tdesktop's
/// `AuthKey::countKeyId`, the `auth_key_id` is computed once when the key is built: every
/// encrypted packet carries it, so it must not cost a SHA-1 of the key per packet. The key
/// bytes are read-only after construction so the cached id cannot go stale.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct AuthKey {
    data: [u8; AUTH_KEY_SIZE],
    key_id: [u8; 8],
}

impl std::fmt::Debug for AuthKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthKey")
            .field("key_id", &format_args!("{:#018x}", self.key_id_u64()))
            .finish_non_exhaustive()
    }
}

impl AuthKey {
    /// Construct an `AuthKey` from an exact 256-byte array.
    pub fn new(data: [u8; AUTH_KEY_SIZE]) -> Self {
        let hash = sha1(&[&data]);
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&hash[12..20]);
        Self { data, key_id }
    }

    /// Raw 256-byte key material.
    pub const fn data(&self) -> &[u8; AUTH_KEY_SIZE] {
        &self.data
    }

    /// Construct an `AuthKey` from big-endian DH key bytes, left-padding with zero bytes
    /// to 256 bytes (port of `MTP::AuthKey::FillData`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() > AUTH_KEY_SIZE {
            return Err(CryptoError::InvalidLength(
                "auth key bytes must not exceed 256 bytes",
            ));
        }
        let mut data = [0u8; AUTH_KEY_SIZE];
        let offset = AUTH_KEY_SIZE - bytes.len();
        data[offset..].copy_from_slice(bytes);
        let key = Self::new(data);
        data.zeroize();
        Ok(key)
    }

    /// Return the 8-byte `auth_key_id` (lower 64 bits of `SHA1(auth_key)`, i.e. bytes `12..20`).
    pub const fn key_id(&self) -> [u8; 8] {
        self.key_id
    }

    /// Return `auth_key_id` as a little-endian `u64` (matching `MTP::AuthKey::keyId()`).
    pub const fn key_id_u64(&self) -> u64 {
        u64::from_le_bytes(self.key_id)
    }

    /// Return the 8-byte `auth_key_aux_hash` (first 64 bits of `SHA1(auth_key)`, i.e. bytes `0..8`).
    ///
    /// Used in `dh_gen_ok`/`dh_gen_retry`/`dh_gen_fail` (`new_nonce_hash1..3`).
    pub fn aux_hash(&self) -> [u8; 8] {
        let hash = sha1(&[&self.data]);
        let mut aux = [0u8; 8];
        aux.copy_from_slice(&hash[0..8]);
        aux
    }

    /// Return `auth_key_aux_hash` as a little-endian `u64`.
    pub fn aux_hash_u64(&self) -> u64 {
        u64::from_le_bytes(self.aux_hash())
    }

    /// Return the 32-byte slice `auth_key[88 + x .. 88 + x + 32]` used for MTProto 2.0 `msg_key`.
    ///
    /// Port of `MTP::AuthKey::partForMsgKey`.
    pub fn part_for_msg_key(&self, side: Side) -> &[u8] {
        let start = 88 + side.x();
        &self.data[start..start + 32]
    }

    /// Derive `(aes_key, aes_iv)` using MTProto 2.0 key derivation (`MTP::AuthKey::prepareAES`).
    pub fn prepare_aes_v2(&self, msg_key: &[u8; 16], side: Side) -> ([u8; 32], [u8; 32]) {
        let x = side.x();
        let sha256_a = sha256(&[msg_key, &self.data[x..x + 36]]);
        let sha256_b = sha256(&[&self.data[40 + x..40 + x + 36], msg_key]);

        let mut aes_key = [0u8; 32];
        aes_key[0..8].copy_from_slice(&sha256_a[0..8]);
        aes_key[8..24].copy_from_slice(&sha256_b[8..24]);
        aes_key[24..32].copy_from_slice(&sha256_a[24..32]);

        let mut aes_iv = [0u8; 32];
        aes_iv[0..8].copy_from_slice(&sha256_b[0..8]);
        aes_iv[8..24].copy_from_slice(&sha256_a[8..24]);
        aes_iv[24..32].copy_from_slice(&sha256_b[24..32]);

        (aes_key, aes_iv)
    }

    /// Derive `(aes_key, aes_iv)` using legacy MTProto 1.0 key derivation
    /// (`MTP::AuthKey::prepareAES_oldmtp`).
    ///
    /// Used by `auth.bindTempAuthKey` (`mtproto_dc_key_binder.cpp`) and tdesktop local storage
    /// (`aesEncryptLocal` / `aesDecryptLocal`, which always pass `Side::Server`, i.e. `x = 8`).
    pub fn prepare_aes_v1(&self, msg_key: &[u8; 16], side: Side) -> ([u8; 32], [u8; 32]) {
        let x = side.x();
        let sha1_a = sha1(&[msg_key, &self.data[x..x + 32]]);
        let sha1_b = sha1(&[
            &self.data[32 + x..48 + x],
            msg_key,
            &self.data[48 + x..64 + x],
        ]);
        let sha1_c = sha1(&[&self.data[64 + x..96 + x], msg_key]);
        let sha1_d = sha1(&[msg_key, &self.data[96 + x..128 + x]]);

        let mut aes_key = [0u8; 32];
        aes_key[0..8].copy_from_slice(&sha1_a[0..8]);
        aes_key[8..20].copy_from_slice(&sha1_b[8..20]);
        aes_key[20..32].copy_from_slice(&sha1_c[4..16]);

        let mut aes_iv = [0u8; 32];
        aes_iv[0..12].copy_from_slice(&sha1_a[8..20]);
        aes_iv[12..20].copy_from_slice(&sha1_b[0..8]);
        aes_iv[20..24].copy_from_slice(&sha1_c[16..20]);
        aes_iv[24..32].copy_from_slice(&sha1_d[0..8]);

        (aes_key, aes_iv)
    }
}

/// Parsed fields of a decrypted MTProto 2.0 message (`salt`, `session_id`, `msg_id`, `seq_no`, `body`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedMessage {
    /// 64-bit server salt.
    pub salt: i64,
    /// 64-bit session identifier.
    pub session_id: i64,
    /// 64-bit message identifier.
    pub msg_id: i64,
    /// 32-bit sequence number.
    pub seq_no: i32,
    /// Message payload bytes (padding stripped).
    pub body: Vec<u8>,
}

/// Compute MTProto 2.0 `msg_key` over `padded_plaintext` (`plaintext || padding`).
pub fn compute_msg_key_v2(auth_key: &AuthKey, side: Side, padded_plaintext: &[u8]) -> [u8; 16] {
    let msg_key_large = sha256(&[auth_key.part_for_msg_key(side), padded_plaintext]);
    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&msg_key_large[8..24]);
    msg_key
}

/// Encrypt `plaintext` using MTProto 2.0 with explicit `padding` bytes.
///
/// The combined length `plaintext.len() + padding.len()` must be a multiple of 16,
/// and `padding.len()` must be within `12..=1024`.
/// Returns `auth_key_id (8) || msg_key (16) || aes_ige_encrypted_data`.
pub fn encrypt_data_v2_with_padding(
    plaintext: &[u8],
    padding: &[u8],
    auth_key: &AuthKey,
    side: Side,
) -> Result<Vec<u8>, CryptoError> {
    if padding.len() < MIN_PADDING_V2 || padding.len() > MAX_PADDING_V2 {
        return Err(CryptoError::InvalidMessageFraming(
            "MTProto 2.0 padding must be 12..=1024 bytes",
        ));
    }
    let total_len = plaintext.len() + padding.len();
    if !total_len.is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: total_len });
    }

    let mut packet = Vec::with_capacity(PACKET_HEADER_SIZE + total_len);
    packet.resize(PACKET_HEADER_SIZE, 0);
    packet.extend_from_slice(plaintext);
    packet.extend_from_slice(padding);
    encrypt_packet_v2_in_place(&mut packet, auth_key, side)?;
    Ok(packet)
}

/// Size of the unencrypted MTProto 2.0 packet header: `auth_key_id (8) || msg_key (16)`.
pub const PACKET_HEADER_SIZE: usize = 8 + 16;

/// Largest random padding [`encrypt_reserved_v2`] appends (`27 + 15 * 16` bytes).
pub const MAX_RANDOM_PADDING_V2: usize = MIN_PADDING_V2 + AES_BLOCK_SIZE - 1 + 15 * AES_BLOCK_SIZE;

/// Encrypt an MTProto 2.0 packet in place.
///
/// `packet[24..]` holds `plaintext || padding` and must be a multiple of 16 bytes; the caller
/// chooses the padding (12..=1024 bytes per the protocol). The plaintext is replaced by the
/// AES-256-IGE ciphertext and `packet[..24]` is overwritten with `auth_key_id || msg_key`, so the
/// buffer becomes the wire packet without further copies.
pub fn encrypt_packet_v2_in_place(
    packet: &mut [u8],
    auth_key: &AuthKey,
    side: Side,
) -> Result<(), CryptoError> {
    let Some((header, padded)) = packet.split_at_mut_checked(PACKET_HEADER_SIZE) else {
        return Err(CryptoError::InvalidLength(
            "MTProto 2.0 packet shorter than its 24-byte header",
        ));
    };
    if !padded.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: padded.len() });
    }
    let msg_key = compute_msg_key_v2(auth_key, side, padded);
    let (mut aes_key, mut aes_iv) = auth_key.prepare_aes_v2(&msg_key, side);
    let encrypted = aes_ige_encrypt_in_place(padded, &aes_key, &aes_iv);
    aes_key.zeroize();
    aes_iv.zeroize();
    encrypted?;
    header[..8].copy_from_slice(&auth_key.key_id());
    header[8..].copy_from_slice(&msg_key);
    Ok(())
}

/// Encrypt a buffer laid out as `24 reserved bytes || plaintext` using MTProto 2.0.
///
/// Appends 12..=267 random padding bytes so the encrypted payload length is a multiple of 16
/// bytes (matching `CountPaddingPrimesCount` in `mtproto_serialized_request.cpp`), encrypts in
/// place and returns the same buffer as `auth_key_id (8) || msg_key (16) || ciphertext`.
/// Reserve [`MAX_RANDOM_PADDING_V2`] extra capacity to avoid a reallocation.
pub fn encrypt_reserved_v2(
    mut packet: Vec<u8>,
    auth_key: &AuthKey,
    side: Side,
) -> Result<Vec<u8>, CryptoError> {
    let Some(plain_len) = packet.len().checked_sub(PACKET_HEADER_SIZE) else {
        return Err(CryptoError::InvalidLength(
            "MTProto 2.0 packet shorter than its 24-byte header",
        ));
    };
    let pad_len = random_padding_len_v2(plain_len);
    let start = packet.len();
    packet.resize(start + pad_len, 0);
    random_bytes(&mut packet[start..]);
    encrypt_packet_v2_in_place(&mut packet, auth_key, side)?;
    Ok(packet)
}

/// Random MTProto 2.0 padding length for `plain_len` plaintext bytes: the 12..=27 bytes that
/// align to 16, plus 0..=15 extra 16-byte blocks (same distribution as tdesktop:
/// `(RandomValue<uchar>() & 0x0F) << 2` primes = 0..=240 bytes).
fn random_padding_len_v2(plain_len: usize) -> usize {
    let rem = plain_len % AES_BLOCK_SIZE;
    let mut pad_len = if rem == 0 { 0 } else { AES_BLOCK_SIZE - rem };
    if pad_len < MIN_PADDING_V2 {
        pad_len += AES_BLOCK_SIZE;
    }
    let extra_blocks = (random_array::<1>()[0] & 0x0F) as usize;
    pad_len + extra_blocks * AES_BLOCK_SIZE
}

/// Encrypt `plaintext` using MTProto 2.0, adding 12..=267 random padding bytes so the
/// encrypted payload length is a multiple of 16 bytes (matching `CountPaddingPrimesCount`
/// in `mtproto_serialized_request.cpp`).
///
/// Returns `auth_key_id (8) || msg_key (16) || aes_ige_encrypted_data`.
pub fn encrypt_data_v2(plaintext: &[u8], auth_key: &AuthKey, side: Side) -> Vec<u8> {
    let mut packet =
        Vec::with_capacity(PACKET_HEADER_SIZE + plaintext.len() + MAX_RANDOM_PADDING_V2);
    packet.resize(PACKET_HEADER_SIZE, 0);
    packet.extend_from_slice(plaintext);
    match encrypt_reserved_v2(packet, auth_key, side) {
        Ok(out) => out,
        Err(_) => unreachable!("the 24-byte header is reserved and the padding block-aligns"),
    }
}

/// Validate the outer MTProto 2.0 packet framing and `auth_key_id` (constant time) and return
/// the packet's `msg_key`.
fn check_packet_v2(encrypted: &[u8], auth_key: &AuthKey) -> Result<[u8; 16], CryptoError> {
    if encrypted.len() < PACKET_HEADER_SIZE + AES_BLOCK_SIZE {
        return Err(CryptoError::InvalidLength(
            "MTProto 2.0 packet shorter than 40 bytes",
        ));
    }
    let ciphertext_len = encrypted.len() - PACKET_HEADER_SIZE;
    if !ciphertext_len.is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength {
            len: ciphertext_len,
        });
    }
    if !const_time_eq(&encrypted[0..8], &auth_key.key_id()) {
        return Err(CryptoError::AuthKeyIdMismatch);
    }
    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&encrypted[8..PACKET_HEADER_SIZE]);
    Ok(msg_key)
}

/// Decrypt `ciphertext` in place and verify `msg_key` over the result (constant time).
fn open_in_place_v2(
    ciphertext: &mut [u8],
    msg_key: &[u8; 16],
    auth_key: &AuthKey,
    side: Side,
) -> Result<(), CryptoError> {
    let (mut aes_key, mut aes_iv) = auth_key.prepare_aes_v2(msg_key, side);
    let decrypted = aes_ige_decrypt_in_place(ciphertext, &aes_key, &aes_iv);
    aes_key.zeroize();
    aes_iv.zeroize();
    decrypted?;
    let expected_msg_key = compute_msg_key_v2(auth_key, side, ciphertext);
    if !const_time_eq(msg_key, &expected_msg_key) {
        return Err(CryptoError::MsgKeyMismatch);
    }
    Ok(())
}

/// Decrypt an MTProto 2.0 encrypted packet (`auth_key_id (8) || msg_key (16) || ciphertext`),
/// verifying `auth_key_id` and `msg_key` in constant time.
///
/// Returns the decrypted plaintext **including trailing random padding** (because MTProto 2.0
/// stores the unpadded payload length inside the 32-byte inner message header). Use
/// [`decrypt_message_v2`] if you also want the inner MTProto header parsed and padding
/// validated & stripped.
pub fn decrypt_data_v2(
    encrypted: &[u8],
    auth_key: &AuthKey,
    side: Side,
) -> Result<Vec<u8>, CryptoError> {
    let msg_key = check_packet_v2(encrypted, auth_key)?;
    let mut decrypted = encrypted[PACKET_HEADER_SIZE..].to_vec();
    open_in_place_v2(&mut decrypted, &msg_key, auth_key, side)?;
    Ok(decrypted)
}

/// Decrypt an MTProto 2.0 packet and parse/validate its 32-byte inner header and padding
/// (port of `SessionPrivate::handleReceived` in `mtproto/session_private.cpp`).
///
/// Enforces:
/// - `auth_key_id` and `msg_key` verification
/// - `message_data_length >= 4` and multiple of 4
/// - `12 <= padding_len <= 1024`
pub fn decrypt_message_v2(
    encrypted: &[u8],
    auth_key: &AuthKey,
    side: Side,
) -> Result<DecryptedMessage, CryptoError> {
    let decrypted = decrypt_data_v2(encrypted, auth_key, side)?;
    parse_inner_message_v2(decrypted)
}

/// Parse the 32-byte inner header of the decrypted payload, validate the payload length and
/// padding, and turn the buffer into the message body in place.
fn parse_inner_message_v2(mut decrypted: Vec<u8>) -> Result<DecryptedMessage, CryptoError> {
    const INNER_HEADER_LEN: usize = 32;
    if decrypted.len() < INNER_HEADER_LEN + 4 + MIN_PADDING_V2 {
        return Err(CryptoError::InvalidMessageFraming(
            "decrypted payload too short for MTProto inner header + padding",
        ));
    }
    let header: &[u8; INNER_HEADER_LEN] = decrypted
        .first_chunk()
        .ok_or(CryptoError::InvalidMessageFraming("inner header slice"))?;
    let le_i64 = |at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&header[at..at + 8]);
        i64::from_le_bytes(bytes)
    };
    let le_u32 = |at: usize| {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&header[at..at + 4]);
        u32::from_le_bytes(bytes)
    };
    let salt = le_i64(0);
    let session_id = le_i64(8);
    let msg_id = le_i64(16);
    let seq_no = le_u32(24).cast_signed();
    let msg_len = le_u32(28) as usize;

    if msg_len < 4 || !msg_len.is_multiple_of(4) {
        return Err(CryptoError::InvalidMessageFraming(
            "message_data_length must be >= 4 and a multiple of 4",
        ));
    }

    let full_data_len =
        INNER_HEADER_LEN
            .checked_add(msg_len)
            .ok_or(CryptoError::InvalidMessageFraming(
                "message_data_length overflow",
            ))?;
    if full_data_len > decrypted.len() {
        return Err(CryptoError::InvalidMessageFraming(
            "message_data_length exceeds decrypted buffer",
        ));
    }
    let padding_len = decrypted.len() - full_data_len;
    if !(MIN_PADDING_V2..=MAX_PADDING_V2).contains(&padding_len) {
        return Err(CryptoError::InvalidMessageFraming(
            "padding length outside 12..=1024",
        ));
    }

    decrypted.truncate(full_data_len);
    decrypted.drain(..INNER_HEADER_LEN);
    Ok(DecryptedMessage {
        salt,
        session_id,
        msg_id,
        seq_no,
        body: decrypted,
    })
}

/// Encrypt `plaintext` using legacy MTProto 1.0 framing (`auth.bindTempAuthKey` inner payload).
///
/// Port of `EncryptBindAuthKeyInner` in `mtproto/details/mtproto_dc_key_binder.cpp`:
/// `msg_key = SHA1(plaintext)[4..20]` (computed BEFORE padding), then zero/random pad to
/// 16-byte boundary and encrypt with `prepare_aes_v1(..., side)`.
pub fn encrypt_data_v1(plaintext: &[u8], auth_key: &AuthKey, side: Side) -> Vec<u8> {
    let hash = sha1(&[plaintext]);
    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&hash[4..20]);

    let rem = plaintext.len() % AES_BLOCK_SIZE;
    let pad_len = if rem == 0 { 0 } else { AES_BLOCK_SIZE - rem };
    let mut padded = Vec::with_capacity(plaintext.len() + pad_len);
    padded.extend_from_slice(plaintext);
    if pad_len > 0 {
        let start = padded.len();
        padded.resize(start + pad_len, 0);
        random_bytes(&mut padded[start..]);
    }

    let (mut aes_key, mut aes_iv) = auth_key.prepare_aes_v1(&msg_key, side);
    let encrypted = match try_aes_ige_encrypt(&padded, &aes_key, &aes_iv) {
        Ok(v) => v,
        Err(_) => unreachable!("padded length is block-aligned"),
    };
    padded.zeroize();
    aes_key.zeroize();
    aes_iv.zeroize();

    let mut out = Vec::with_capacity(8 + 16 + encrypted.len());
    out.extend_from_slice(&auth_key.key_id());
    out.extend_from_slice(&msg_key);
    out.extend_from_slice(&encrypted);
    out
}

/// Decrypt a legacy MTProto 1.0 packet (`auth_key_id (8) || msg_key (16) || ciphertext`),
/// verifying `auth_key_id`. Because MTProto 1.0 hashes the unpadded plaintext, the caller
/// must supply `unpadded_len` (or verify `SHA1(decrypted[..unpadded_len])[4..20] == msg_key`).
pub fn decrypt_data_v1(
    encrypted: &[u8],
    auth_key: &AuthKey,
    side: Side,
) -> Result<(Vec<u8>, [u8; 16]), CryptoError> {
    const HEADER_LEN: usize = 8 + 16;
    if encrypted.len() < HEADER_LEN + AES_BLOCK_SIZE {
        return Err(CryptoError::InvalidLength(
            "MTProto 1.0 packet shorter than 40 bytes",
        ));
    }
    let ciphertext = &encrypted[HEADER_LEN..];
    if !ciphertext.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength {
            len: ciphertext.len(),
        });
    }
    if !const_time_eq(&encrypted[0..8], &auth_key.key_id()) {
        return Err(CryptoError::AuthKeyIdMismatch);
    }

    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&encrypted[8..24]);

    let (mut aes_key, mut aes_iv) = auth_key.prepare_aes_v1(&msg_key, side);
    let decrypted = try_aes_ige_decrypt(ciphertext, &aes_key, &aes_iv)?;
    aes_key.zeroize();
    aes_iv.zeroize();

    Ok((decrypted, msg_key))
}
