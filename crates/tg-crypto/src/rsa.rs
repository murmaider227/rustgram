//! RSA public keys, PEM parsing, Telegram fingerprints, and RSA_PAD encryption.
//!
//! Port of `mtproto/details/mtproto_rsa_public_key.cpp`, `mtproto/mtproto_dc_options.cpp`
//! (`kPublicRSAKeys` / `kTestPublicRSAKeys`), `mtproto/special_config_request.cpp`, and
//! `mtproto/details/mtproto_dc_key_creator.cpp` (`EncryptPQInnerRSA` / `IsGoodEncryptedInner`).

use num_bigint::BigUint;
use zeroize::Zeroize;

use crate::aes::try_aes_ige_encrypt;
use crate::error::CryptoError;
use crate::hash::{sha1, sha256};
use crate::random::{random_array, random_bytes};

/// Built-in Telegram production server RSA public keys (`kPublicRSAKeys` in
/// `mtproto/mtproto_dc_options.cpp`).
pub const PRODUCTION_PUBLIC_KEYS: &[&str] = &["-----BEGIN RSA PUBLIC KEY-----\n\
MIIBCgKCAQEA6LszBcC1LGzyr992NzE0ieY+BSaOW622Aa9Bd4ZHLl+TuFQ4lo4g\n\
5nKaMBwK/BIb9xUfg0Q29/2mgIR6Zr9krM7HjuIcCzFvDtr+L0GQjae9H0pRB2OO\n\
62cECs5HKhT5DZ98K33vmWiLowc621dQuwKWSQKjWf50XYFw42h21P2KXUGyp2y/\n\
+aEyZ+uVgLLQbRA1dEjSDZ2iGRy12Mk5gpYc397aYp438fsJoHIgJ2lgMv5h7WY9\n\
t6N/byY9Nw9p21Og3AoXSL2q/2IJ1WRUhebgAdGVMlV1fkuOQoEzR7EdpqtQD9Cs\n\
5+bfo3Nhmcyvk5ftB0WkJ9z6bNZ7yxrP8wIDAQAB\n\
-----END RSA PUBLIC KEY-----"];

/// Built-in Telegram test DC RSA public keys (`kTestPublicRSAKeys` in
/// `mtproto/mtproto_dc_options.cpp`).
pub const TEST_PUBLIC_KEYS: &[&str] = &["-----BEGIN RSA PUBLIC KEY-----\n\
MIIBCgKCAQEAyMEdY1aR+sCR3ZSJrtztKTKqigvO/vBfqACJLZtS7QMgCGXJ6XIR\n\
yy7mx66W0/sOFa7/1mAZtEoIokDP3ShoqF4fVNb6XeqgQfaUHd8wJpDWHcR2OFwv\n\
plUUI1PLTktZ9uW2WE23b+ixNwJjJGwBDJPQEQFBE+vfmH0JP503wr5INS1poWg/\n\
j25sIWeYPHYeOrFp/eXaqhISP6G+q2IeTaWTXpwZj4LzXq5YOpk4bYEQ6mvRq7D1\n\
aHWfYmlEGepfaYR8Q0YqvvhYtMte3ITnuSJs171+GDqpdKcSwHnd6FudwGO4pcCO\n\
j4WcDuXc2CTHgH8gFTNhp/Y8/SpDOhvn9QIDAQAB\n\
-----END RSA PUBLIC KEY-----"];

/// Built-in Telegram special config RSA public key (`kPublicKey` in
/// `mtproto/special_config_request.cpp`).
pub const SPECIAL_CONFIG_PUBLIC_KEY: &str = "\
-----BEGIN RSA PUBLIC KEY-----\n\
MIIBCgKCAQEAyr+18Rex2ohtVy8sroGPBwXD3DOoKCSpjDqYoXgCqB7ioln4eDCF\n\
fOBUlfXUEvM/fnKCpF46VkAftlb4VuPDeQSS/ZxZYEGqHaywlroVnXHIjgqoxiAd\n\
192xRGreuXIaUKmkwlM9JID9WS2jUsTpzQ91L8MEPLJ/4zrBwZua8W5fECwCCh2c\n\
9G5IzzBm+otMS/YKwmR1olzRCyEkyAEjXWqBI9Ftv5eG8m0VkBzOG655WIYdyV0H\n\
fDK/NWcvGqa0w/nriMD6mDjKOryamw0OP9QuYgMN0C9xMW9y8SmP4h92OAWodTYg\n\
Y1hZCxdv6cs5UnW9+PWvS+WIbkh+GaWYxwIDAQAB\n\
-----END RSA PUBLIC KEY-----";

/// OID `1.2.840.113549.1.1.1` (`rsaEncryption`).
const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

/// An RSA public key with precomputed MTProto 64-bit fingerprint.
///
/// Port of `MTP::details::RSAPublicKey` in `mtproto/details/mtproto_rsa_public_key.h/.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    n: BigUint,
    e: BigUint,
    n_bytes: Vec<u8>,
    e_bytes: Vec<u8>,
    fingerprint: u64,
}

impl PublicKey {
    /// Parse an RSA public key from PEM text (`-----BEGIN RSA PUBLIC KEY-----` PKCS#1 or
    /// `-----BEGIN PUBLIC KEY-----` X.509 SubjectPublicKeyInfo).
    pub fn from_pem(pem: &str) -> Result<Self, CryptoError> {
        let (is_spki, b64) = extract_pem_body(pem)?;
        let der = decode_base64(&b64)?;
        let (n_bytes, e_bytes) = if is_spki {
            parse_spki_der(&der)?
        } else {
            parse_pkcs1_der(&der)?
        };
        Self::from_components(&n_bytes, &e_bytes)
    }

    /// Construct an RSA public key from big-endian unsigned modulus `n` and exponent `e`.
    pub fn from_components(n_bytes: &[u8], e_bytes: &[u8]) -> Result<Self, CryptoError> {
        let n = BigUint::from_bytes_be(n_bytes);
        let e = BigUint::from_bytes_be(e_bytes);
        if n == BigUint::ZERO || e == BigUint::ZERO {
            return Err(CryptoError::InvalidRsaKey(
                "modulus and exponent must be positive",
            ));
        }
        let canonical_n = n.to_bytes_be();
        let canonical_e = e.to_bytes_be();
        let fingerprint = compute_fingerprint(&canonical_n, &canonical_e);
        Ok(Self {
            n,
            e,
            n_bytes: canonical_n,
            e_bytes: canonical_e,
            fingerprint,
        })
    }

    /// Return the canonical big-endian unsigned modulus bytes (`BN_bn2bin(n)`).
    pub fn n(&self) -> &[u8] {
        &self.n_bytes
    }

    /// Return the canonical big-endian unsigned exponent bytes (`BN_bn2bin(e)`).
    pub fn e(&self) -> &[u8] {
        &self.e_bytes
    }

    /// Return the 64-bit MTProto RSA key fingerprint (lower 64 bits of
    /// `SHA1(tl_bytes(n) || tl_bytes(e))` as little-endian `u64`).
    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Return the fingerprint as a signed `i64` (matching TL `long` in
    /// `server_public_key_fingerprints`).
    pub const fn fingerprint_i64(&self) -> i64 {
        self.fingerprint as i64
    }

    /// Raw textbook RSA encryption (`m^e mod n`) for a 256-byte big-endian block
    /// (`RSAPublicKey::encrypt` with `RSA_NO_PADDING`).
    pub fn encrypt_raw(&self, data: &[u8; 256]) -> Result<[u8; 256], CryptoError> {
        if self.n_bytes.len() > 256 {
            return Err(CryptoError::RsaFailed("modulus exceeds 256 bytes"));
        }
        let m = BigUint::from_bytes_be(data);
        if m >= self.n {
            return Err(CryptoError::RsaFailed("input integer >= RSA modulus"));
        }
        let c = m.modpow(&self.e, &self.n);
        biguint_to_fixed_256(&c)
    }

    /// Raw textbook RSA public-key operation (`c^e mod n`, used for signature verification;
    /// port of `RSAPublicKey::decrypt` with `RSA_NO_PADDING`).
    pub fn decrypt_raw(&self, data: &[u8; 256]) -> Result<[u8; 256], CryptoError> {
        self.encrypt_raw(data)
    }

    /// Encrypt `p_q_inner_data` using MTProto 2.0 `RSA_PAD`
    /// (`EncryptPQInnerRSA` in `mtproto/details/mtproto_dc_key_creator.cpp`).
    ///
    /// `data` must be at most 144 bytes.
    pub fn encrypt_hashed(&self, data: &[u8]) -> Result<[u8; 256], CryptoError> {
        const MAX_DATA_LEN: usize = 144;
        const DATA_WITH_PADDING_LEN: usize = 192;

        if data.len() > MAX_DATA_LEN {
            return Err(CryptoError::RsaFailed(
                "RSA_PAD input exceeds maximum length of 144 bytes",
            ));
        }

        let mut padding = vec![0u8; DATA_WITH_PADDING_LEN - data.len()];
        random_bytes(&mut padding);

        for _ in 0..1024 {
            let mut temp_key = random_array::<32>();
            let res = self.encrypt_hashed_with_random(data, &padding, &temp_key);
            temp_key.zeroize();
            match res {
                Ok(encrypted) => {
                    padding.zeroize();
                    return Ok(encrypted);
                }
                Err(CryptoError::RsaFailed("key_aes_encrypted >= modulus")) => continue,
                Err(e) => {
                    padding.zeroize();
                    return Err(e);
                }
            }
        }
        padding.zeroize();
        Err(CryptoError::RsaFailed(
            "failed to sample valid RSA_PAD temp_key",
        ))
    }

    /// Deterministic variant of [`Self::encrypt_hashed`] taking explicit `padding`
    /// (`padding.len() == 192 - data.len()`) and `temp_key` (32 bytes).
    ///
    /// Returns `Err(CryptoError::RsaFailed("key_aes_encrypted >= modulus"))` if the
    /// constructed 256-byte block is `>= n`.
    pub fn encrypt_hashed_with_random(
        &self,
        data: &[u8],
        padding: &[u8],
        temp_key: &[u8; 32],
    ) -> Result<[u8; 256], CryptoError> {
        const MAX_DATA_LEN: usize = 144;
        const DATA_WITH_PADDING_LEN: usize = 192;

        if data.len() > MAX_DATA_LEN {
            return Err(CryptoError::RsaFailed(
                "RSA_PAD input exceeds maximum length of 144 bytes",
            ));
        }
        if data.len() + padding.len() != DATA_WITH_PADDING_LEN {
            return Err(CryptoError::InvalidLength(
                "data.len() + padding.len() must equal 192",
            ));
        }

        // data_with_padding := data + random_padding_bytes (192 bytes)
        let mut data_with_padding = [0u8; DATA_WITH_PADDING_LEN];
        data_with_padding[..data.len()].copy_from_slice(data);
        data_with_padding[data.len()..].copy_from_slice(padding);

        // data_pad_reversed := BYTE_REVERSE(data_with_padding)
        let mut data_with_hash = [0u8; DATA_WITH_PADDING_LEN + 32];
        data_with_hash[..DATA_WITH_PADDING_LEN].copy_from_slice(&data_with_padding);
        data_with_hash[..DATA_WITH_PADDING_LEN].reverse();

        // data_with_hash := data_pad_reversed + SHA256(temp_key + data_with_padding)
        let hash = sha256(&[temp_key, &data_with_padding]);
        data_with_hash[DATA_WITH_PADDING_LEN..].copy_from_slice(&hash);

        // aes_encrypted := AES256_IGE(data_with_hash, temp_key, 0)
        let zero_iv = [0u8; 32];
        let aes_encrypted = try_aes_ige_encrypt(&data_with_hash, temp_key, &zero_iv)?;

        // temp_key_xor := temp_key XOR SHA256(aes_encrypted)
        let aes_hash = sha256(&[&aes_encrypted]);
        let mut key_aes_encrypted = [0u8; 256];
        for i in 0..32 {
            key_aes_encrypted[i] = temp_key[i] ^ aes_hash[i];
        }
        // key_aes_encrypted := temp_key_xor + aes_encrypted
        key_aes_encrypted[32..].copy_from_slice(&aes_encrypted);

        data_with_padding.zeroize();
        data_with_hash.zeroize();

        if !self.is_less_than_modulus(&key_aes_encrypted) {
            key_aes_encrypted.zeroize();
            return Err(CryptoError::RsaFailed("key_aes_encrypted >= modulus"));
        }

        let result = self.encrypt_raw(&key_aes_encrypted);
        key_aes_encrypted.zeroize();
        result
    }

    /// Legacy MTProto 1.0 plain RSA encryption:
    /// `SHA1(data) (20) || data || random_padding` to 255 bytes, left-padded with `0x00`
    /// to 256 bytes, then raw RSA `m^e mod n`.
    ///
    /// `data` must be at most 235 bytes.
    pub fn encrypt_legacy(&self, data: &[u8]) -> Result<[u8; 256], CryptoError> {
        const MAX_LEGACY_LEN: usize = 255 - 20;
        if data.len() > MAX_LEGACY_LEN {
            return Err(CryptoError::RsaFailed(
                "legacy RSA input exceeds maximum length of 235 bytes",
            ));
        }

        let digest = sha1(&[data]);
        let mut block = [0u8; 256];
        // Leading zero byte ensures block < 2^2040 < n for any 2048-bit modulus.
        block[1..21].copy_from_slice(&digest);
        block[21..21 + data.len()].copy_from_slice(data);
        random_bytes(&mut block[21 + data.len()..]);

        let result = self.encrypt_raw(&block);
        block.zeroize();
        result
    }

    /// RSAES-OAEP encryption with SHA-1 and MGF1-SHA1 (`RSAPublicKey::encryptOAEPpadding`,
    /// matching OpenSSL `RSA_PKCS1_OAEP_PADDING`, used by Telegram Passport).
    pub fn encrypt_oaep(&self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        const H_LEN: usize = 20;
        let k = self.n_bytes.len();
        if k < 2 * H_LEN + 2 {
            return Err(CryptoError::RsaFailed("modulus too small for OAEP"));
        }
        let max_len = k - 2 * H_LEN - 2;
        if data.len() > max_len {
            return Err(CryptoError::RsaFailed("data too long for RSA-OAEP"));
        }

        let l_hash = sha1(&[b""]);
        let db_len = k - H_LEN - 1;
        let mut db = vec![0u8; db_len];
        db[..H_LEN].copy_from_slice(&l_hash);
        let sep_idx = db_len - data.len() - 1;
        db[sep_idx] = 0x01;
        db[sep_idx + 1..].copy_from_slice(data);

        let mut seed = random_array::<H_LEN>();
        let db_mask = mgf1_sha1(&seed, db_len);
        for (d, m) in db.iter_mut().zip(db_mask.iter()) {
            *d ^= *m;
        }
        let seed_mask = mgf1_sha1(&db, H_LEN);
        let mut masked_seed = [0u8; H_LEN];
        for i in 0..H_LEN {
            masked_seed[i] = seed[i] ^ seed_mask[i];
        }
        seed.zeroize();

        let mut em = Vec::with_capacity(k);
        em.push(0x00);
        em.extend_from_slice(&masked_seed);
        em.extend_from_slice(&db);

        let m = BigUint::from_bytes_be(&em);
        em.zeroize();
        if m >= self.n {
            return Err(CryptoError::RsaFailed("OAEP representative >= modulus"));
        }
        let c = m.modpow(&self.e, &self.n);
        let c_bytes = c.to_bytes_be();
        if c_bytes.len() > k {
            return Err(CryptoError::RsaFailed("ciphertext exceeds modulus size"));
        }
        let mut out = vec![0u8; k];
        out[k - c_bytes.len()..].copy_from_slice(&c_bytes);
        Ok(out)
    }

    /// Compare a 256-byte big-endian block strictly `< n` (port of `IsGoodEncryptedInner`).
    fn is_less_than_modulus(&self, block: &[u8; 256]) -> bool {
        if self.n_bytes.len() > 256 {
            return true;
        }
        let shift = 256 - self.n_bytes.len();
        for (i, &a) in block.iter().enumerate() {
            let b = if i < shift {
                0
            } else {
                self.n_bytes[i - shift]
            };
            match a.cmp(&b) {
                std::cmp::Ordering::Greater => return false,
                std::cmp::Ordering::Less => return true,
                std::cmp::Ordering::Equal => {}
            }
        }
        false
    }
}

/// Parse all built-in Telegram production server RSA public keys.
pub fn builtin_production_keys() -> Vec<PublicKey> {
    PRODUCTION_PUBLIC_KEYS
        .iter()
        .filter_map(|pem| PublicKey::from_pem(pem).ok())
        .collect()
}

/// Parse all built-in Telegram test DC RSA public keys.
pub fn builtin_test_keys() -> Vec<PublicKey> {
    TEST_PUBLIC_KEYS
        .iter()
        .filter_map(|pem| PublicKey::from_pem(pem).ok())
        .collect()
}

/// Find the first key in `keys` matching any fingerprint in `fingerprints` (signed TL `long`).
pub fn find_by_fingerprints<'a>(
    keys: &'a [PublicKey],
    fingerprints: &[i64],
) -> Option<&'a PublicKey> {
    for &fp in fingerprints {
        if let Some(key) = keys.iter().find(|k| k.fingerprint_i64() == fp) {
            return Some(key);
        }
    }
    None
}

fn compute_fingerprint(n_bytes: &[u8], e_bytes: &[u8]) -> u64 {
    let mut tl = Vec::with_capacity(n_bytes.len() + e_bytes.len() + 16);
    write_tl_bytes(&mut tl, n_bytes);
    write_tl_bytes(&mut tl, e_bytes);
    let digest = sha1(&[&tl]);
    let mut fp = [0u8; 8];
    fp.copy_from_slice(&digest[12..20]);
    u64::from_le_bytes(fp)
}

fn write_tl_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = bytes.len();
    let header_len = if len <= 253 {
        out.push(len as u8);
        1
    } else {
        out.push(254);
        out.push((len & 0xff) as u8);
        out.push(((len >> 8) & 0xff) as u8);
        out.push(((len >> 16) & 0xff) as u8);
        4
    };
    out.extend_from_slice(bytes);
    let rem = (header_len + len) % 4;
    if rem != 0 {
        out.resize(out.len() + (4 - rem), 0);
    }
}

fn biguint_to_fixed_256(value: &BigUint) -> Result<[u8; 256], CryptoError> {
    let bytes = value.to_bytes_be();
    if bytes.len() > 256 {
        return Err(CryptoError::RsaFailed("RSA result exceeds 256 bytes"));
    }
    let mut out = [0u8; 256];
    out[256 - bytes.len()..].copy_from_slice(&bytes);
    Ok(out)
}

fn mgf1_sha1(seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let block = sha1(&[seed, &counter.to_be_bytes()]);
        let take = (len - out.len()).min(block.len());
        out.extend_from_slice(&block[..take]);
        counter = counter.wrapping_add(1);
    }
    out
}

fn extract_pem_body(pem: &str) -> Result<(bool, String), CryptoError> {
    const PKCS1_BEGIN: &str = "-----BEGIN RSA PUBLIC KEY-----";
    const PKCS1_END: &str = "-----END RSA PUBLIC KEY-----";
    const SPKI_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";
    const SPKI_END: &str = "-----END PUBLIC KEY-----";

    let (is_spki, begin, end) = if pem.contains(PKCS1_BEGIN) {
        (false, PKCS1_BEGIN, PKCS1_END)
    } else if pem.contains(SPKI_BEGIN) {
        (true, SPKI_BEGIN, SPKI_END)
    } else {
        return Err(CryptoError::InvalidRsaKey("missing PEM header"));
    };

    let start_idx = pem
        .find(begin)
        .ok_or(CryptoError::InvalidRsaKey("missing PEM begin marker"))?
        + begin.len();
    let rest = &pem[start_idx..];
    let end_idx = rest
        .find(end)
        .ok_or(CryptoError::InvalidRsaKey("missing PEM end marker"))?;
    let body: String = rest[..end_idx]
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    if body.is_empty() {
        return Err(CryptoError::InvalidRsaKey("empty PEM body"));
    }
    Ok((is_spki, body))
}

fn decode_base64(input: &str) -> Result<Vec<u8>, CryptoError> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(CryptoError::InvalidRsaKey(
            "invalid base64 length in PEM body",
        ));
    }
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    for chunk in bytes.as_chunks::<4>().0 {
        let mut vals = [0u8; 4];
        let mut pad = 0usize;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                if i < 2 || (i == 2 && chunk[3] != b'=') {
                    return Err(CryptoError::InvalidRsaKey("invalid base64 padding"));
                }
                pad += 1;
                vals[i] = 0;
            } else {
                if pad > 0 {
                    return Err(CryptoError::InvalidRsaKey("data after base64 padding"));
                }
                vals[i] = match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => return Err(CryptoError::InvalidRsaKey("invalid base64 character")),
                };
            }
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);
        out.push(((n >> 16) & 0xff) as u8);
        if pad < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if pad < 1 {
            out.push((n & 0xff) as u8);
        }
    }
    Ok(out)
}

fn read_der_tlv(input: &[u8]) -> Result<(u8, &[u8], &[u8]), CryptoError> {
    let (&tag, rest) = input
        .split_first()
        .ok_or(CryptoError::InvalidRsaKey("truncated DER tag"))?;
    let (&first_len, rest) = rest
        .split_first()
        .ok_or(CryptoError::InvalidRsaKey("truncated DER length"))?;

    let (len, rest) = if first_len < 0x80 {
        (first_len as usize, rest)
    } else {
        let num_bytes = (first_len & 0x7f) as usize;
        if num_bytes == 0 || num_bytes > 4 || rest.len() < num_bytes {
            return Err(CryptoError::InvalidRsaKey("invalid DER length encoding"));
        }
        let mut len = 0usize;
        for &b in &rest[..num_bytes] {
            len = (len << 8) | (b as usize);
        }
        (len, &rest[num_bytes..])
    };

    if rest.len() < len {
        return Err(CryptoError::InvalidRsaKey("DER value exceeds input"));
    }
    Ok((tag, &rest[..len], &rest[len..]))
}

fn read_der_positive_int(input: &[u8]) -> Result<(Vec<u8>, &[u8]), CryptoError> {
    let (tag, val, rest) = read_der_tlv(input)?;
    if tag != 0x02 || val.is_empty() {
        return Err(CryptoError::InvalidRsaKey("expected DER INTEGER"));
    }
    if val[0] & 0x80 != 0 {
        return Err(CryptoError::InvalidRsaKey("negative RSA DER INTEGER"));
    }
    let stripped = match val.iter().position(|&b| b != 0) {
        Some(pos) => &val[pos..],
        None => return Err(CryptoError::InvalidRsaKey("zero RSA DER INTEGER")),
    };
    Ok((stripped.to_vec(), rest))
}

fn parse_pkcs1_der(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let (tag, seq, trailing) = read_der_tlv(der)?;
    if tag != 0x30 || !trailing.is_empty() {
        return Err(CryptoError::InvalidRsaKey("expected PKCS#1 SEQUENCE"));
    }
    let (n, rest) = read_der_positive_int(seq)?;
    let (e, rest) = read_der_positive_int(rest)?;
    if !rest.is_empty() {
        return Err(CryptoError::InvalidRsaKey(
            "unexpected trailing bytes in PKCS#1 RSAPublicKey",
        ));
    }
    Ok((n, e))
}

fn parse_spki_der(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let (tag, seq, trailing) = read_der_tlv(der)?;
    if tag != 0x30 || !trailing.is_empty() {
        return Err(CryptoError::InvalidRsaKey("expected SPKI SEQUENCE"));
    }

    // AlgorithmIdentifier SEQUENCE { OID rsaEncryption, optional NULL }
    let (alg_tag, alg_seq, rest) = read_der_tlv(seq)?;
    if alg_tag != 0x30 {
        return Err(CryptoError::InvalidRsaKey(
            "expected AlgorithmIdentifier SEQUENCE",
        ));
    }
    let (oid_tag, oid, alg_rest) = read_der_tlv(alg_seq)?;
    if oid_tag != 0x06 || oid != OID_RSA_ENCRYPTION {
        return Err(CryptoError::InvalidRsaKey(
            "SPKI algorithm is not rsaEncryption",
        ));
    }
    if !alg_rest.is_empty() {
        let (null_tag, null_val, alg_tail) = read_der_tlv(alg_rest)?;
        if null_tag != 0x05 || !null_val.is_empty() || !alg_tail.is_empty() {
            return Err(CryptoError::InvalidRsaKey(
                "invalid SPKI algorithm parameters",
            ));
        }
    }

    // subjectPublicKey BIT STRING { 0x00, PKCS#1 RSAPublicKey }
    let (bit_tag, bit_str, tail) = read_der_tlv(rest)?;
    if bit_tag != 0x03 || !tail.is_empty() || bit_str.is_empty() || bit_str[0] != 0x00 {
        return Err(CryptoError::InvalidRsaKey(
            "invalid SPKI subjectPublicKey BIT STRING",
        ));
    }
    parse_pkcs1_der(&bit_str[1..])
}
