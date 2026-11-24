//! Hash functions, HMAC, PBKDF2, and constant-time comparison.
//!
//! Port of `Telegram/lib_base/base/openssl_help.h`, `core/utils.cpp`,
//! and `ConstTimeIsDifferent` from `mtproto/session_private.cpp`.

use hmac::{Hmac, Mac};
use md5::{Digest as _, Md5};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

/// Compute SHA-1 over a slice of byte chunks.
pub fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Compute SHA-1 over a single byte slice.
pub fn sha1_bytes(data: &[u8]) -> [u8; 20] {
    sha1(&[data])
}

/// Compute SHA-256 over a slice of byte chunks.
pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Compute SHA-256 over a single byte slice.
pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    sha256(&[data])
}

/// Compute SHA-512 over a slice of byte chunks.
pub fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Compute SHA-512 over a single byte slice.
pub fn sha512_bytes(data: &[u8]) -> [u8; 64] {
    sha512(&[data])
}

/// Compute MD5 over a slice of byte chunks.
pub fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut hasher = Md5::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Compute MD5 over a single byte slice.
pub fn md5_bytes(data: &[u8]) -> [u8; 16] {
    md5(&[data])
}

/// Compute lowercase hexadecimal MD5 digest (matching `hashMd5Hex` in `core/utils.h`).
pub fn md5_hex(data: &[u8]) -> String {
    let digest = md5_bytes(data);
    let mut out = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Incremental SHA-256 hasher for streaming file verification.
#[derive(Clone, Default)]
pub struct Sha256Hasher {
    inner: Sha256,
}

impl Sha256Hasher {
    /// Create a new incremental SHA-256 hasher.
    pub fn new() -> Self {
        Self {
            inner: Sha256::new(),
        }
    }

    /// Feed bytes into the SHA-256 state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalize and return the 32-byte SHA-256 digest.
    pub fn finalize(self) -> [u8; 32] {
        self.inner.finalize().into()
    }
}

/// Incremental MD5 hasher (port of `HashMd5` in `core/utils.h`).
#[derive(Clone, Default)]
pub struct Md5Hasher {
    inner: Md5,
}

impl Md5Hasher {
    /// Create a new incremental MD5 hasher.
    pub fn new() -> Self {
        Self { inner: Md5::new() }
    }

    /// Feed bytes into the MD5 state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalize and return the 16-byte MD5 digest.
    pub fn finalize(self) -> [u8; 16] {
        self.inner.finalize().into()
    }
}

/// Compute HMAC-SHA256 (`openssl::HmacSha256` in `lib_base/base/openssl_help.h`).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    hmac_sha256_parts(key, &[data])
}

/// Compute HMAC-SHA256 over a slice of byte chunks without concatenating.
pub fn hmac_sha256_parts(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    // HMAC accepts keys of any length; new_from_slice never fails for HMAC.
    let mut mac = match Hmac::<Sha256>::new_from_slice(key) {
        Ok(m) => m,
        Err(_) => unreachable!("HMAC-SHA256 accepts keys of any size"),
    };
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// Compute HMAC-SHA512.
pub fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    hmac_sha512_parts(key, &[data])
}

/// Compute HMAC-SHA512 over a slice of byte chunks without concatenating.
pub fn hmac_sha512_parts(key: &[u8], parts: &[&[u8]]) -> [u8; 64] {
    let mut mac = match Hmac::<Sha512>::new_from_slice(key) {
        Ok(m) => m,
        Err(_) => unreachable!("HMAC-SHA512 accepts keys of any size"),
    };
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// Derive bytes using PBKDF2-HMAC-SHA512 (`PKCS5_PBKDF2_HMAC` with `EVP_sha512`).
pub fn pbkdf2_hmac_sha512(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    pbkdf2::pbkdf2_hmac::<Sha512>(password, salt, iterations, out);
}

/// Derive a 64-byte key using PBKDF2-HMAC-SHA512 (`openssl::Pbkdf2Sha512`).
pub fn pbkdf2_hmac_sha512_64(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 64] {
    let mut out = [0u8; 64];
    pbkdf2_hmac_sha512(password, salt, iterations, &mut out);
    out
}

/// Derive bytes using PBKDF2-HMAC-SHA1 (`PKCS5_PBKDF2_HMAC_SHA1`).
pub fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, iterations, out);
}

/// Constant-time byte-slice equality check (port of `ConstTimeIsDifferent`).
pub fn const_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
