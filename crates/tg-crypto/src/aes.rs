//! AES-256-IGE, stateful AES-256-CTR, and AES-256-CBC modes.
//!
//! Port of `mtproto/mtproto_auth_key.cpp` (`aesIgeEncryptRaw`, `aesIgeDecryptRaw`,
//! `aesCtrEncrypt`, `CTRState`) and `passport/passport_encryption.cpp` (AES-256-CBC).

use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::CryptoError;

/// AES block size in bytes (128 bits).
pub const AES_BLOCK_SIZE: usize = 16;

/// Encrypt `data` with AES-256-IGE, returning an error if `data.len()` is not a multiple of 16.
///
/// Follows OpenSSL's `AES_ige_encrypt(..., AES_ENCRYPT)` convention:
/// `iv[0..16]` is $y_0$ (previous ciphertext block) and `iv[16..32]` is $x_0$ (previous plaintext block).
pub fn try_aes_ige_encrypt(
    data: &[u8],
    key: &[u8; 32],
    iv: &[u8; 32],
) -> Result<Vec<u8>, CryptoError> {
    if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: data.len() });
    }
    let mut out = data.to_vec();
    aes_ige_encrypt_in_place(&mut out, key, iv)?;
    Ok(out)
}

/// Decrypt `data` with AES-256-IGE, returning an error if `data.len()` is not a multiple of 16.
///
/// Follows OpenSSL's `AES_ige_encrypt(..., AES_DECRYPT)` convention:
/// `iv[0..16]` is $y_0$ (previous ciphertext block) and `iv[16..32]` is $x_0$ (previous plaintext block).
pub fn try_aes_ige_decrypt(
    data: &[u8],
    key: &[u8; 32],
    iv: &[u8; 32],
) -> Result<Vec<u8>, CryptoError> {
    if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: data.len() });
    }
    let mut out = data.to_vec();
    aes_ige_decrypt_in_place(&mut out, key, iv)?;
    Ok(out)
}

/// Encrypt `data` with AES-256-IGE.
///
/// # Panics
/// Panics if `data.len()` is not a multiple of 16. For untrusted/external inputs,
/// use [`try_aes_ige_encrypt`] instead.
pub fn aes_ige_encrypt(data: &[u8], key: &[u8; 32], iv: &[u8; 32]) -> Vec<u8> {
    assert!(
        data.len().is_multiple_of(AES_BLOCK_SIZE),
        "AES-IGE plaintext length ({}) must be a multiple of 16",
        data.len()
    );
    let mut out = data.to_vec();
    let _ = aes_ige_encrypt_in_place(&mut out, key, iv);
    out
}

/// Decrypt `data` with AES-256-IGE.
///
/// # Panics
/// Panics if `data.len()` is not a multiple of 16. For untrusted/external inputs,
/// use [`try_aes_ige_decrypt`] instead.
pub fn aes_ige_decrypt(data: &[u8], key: &[u8; 32], iv: &[u8; 32]) -> Vec<u8> {
    assert!(
        data.len().is_multiple_of(AES_BLOCK_SIZE),
        "AES-IGE ciphertext length ({}) must be a multiple of 16",
        data.len()
    );
    let mut out = data.to_vec();
    let _ = aes_ige_decrypt_in_place(&mut out, key, iv);
    out
}

/// Encrypt `buf` in place using AES-256-IGE.
pub fn aes_ige_encrypt_in_place(
    buf: &mut [u8],
    key: &[u8; 32],
    iv: &[u8; 32],
) -> Result<(), CryptoError> {
    if !buf.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: buf.len() });
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut prev_cipher = [0u8; AES_BLOCK_SIZE];
    let mut prev_plain = [0u8; AES_BLOCK_SIZE];
    prev_cipher.copy_from_slice(&iv[..AES_BLOCK_SIZE]);
    prev_plain.copy_from_slice(&iv[AES_BLOCK_SIZE..]);

    for chunk in buf.as_chunks_mut::<AES_BLOCK_SIZE>().0 {
        let plain = *chunk;

        let mut block = [0u8; AES_BLOCK_SIZE];
        for i in 0..AES_BLOCK_SIZE {
            block[i] = plain[i] ^ prev_cipher[i];
        }
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut block));
        for i in 0..AES_BLOCK_SIZE {
            block[i] ^= prev_plain[i];
        }

        *chunk = block;
        prev_cipher = block;
        prev_plain = plain;
    }

    prev_cipher.zeroize();
    prev_plain.zeroize();
    Ok(())
}

/// Decrypt `buf` in place using AES-256-IGE.
pub fn aes_ige_decrypt_in_place(
    buf: &mut [u8],
    key: &[u8; 32],
    iv: &[u8; 32],
) -> Result<(), CryptoError> {
    if !buf.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: buf.len() });
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut prev_cipher = [0u8; AES_BLOCK_SIZE];
    let mut prev_plain = [0u8; AES_BLOCK_SIZE];
    prev_cipher.copy_from_slice(&iv[..AES_BLOCK_SIZE]);
    prev_plain.copy_from_slice(&iv[AES_BLOCK_SIZE..]);

    for chunk in buf.as_chunks_mut::<AES_BLOCK_SIZE>().0 {
        let cipher_block = *chunk;

        let mut block = [0u8; AES_BLOCK_SIZE];
        for i in 0..AES_BLOCK_SIZE {
            block[i] = cipher_block[i] ^ prev_plain[i];
        }
        cipher.decrypt_block(GenericArray::from_mut_slice(&mut block));
        for i in 0..AES_BLOCK_SIZE {
            block[i] ^= prev_cipher[i];
        }

        *chunk = block;
        prev_cipher = cipher_block;
        prev_plain = block;
    }

    prev_cipher.zeroize();
    prev_plain.zeroize();
    Ok(())
}

/// Stateful AES-256-CTR cipher (128-bit big-endian counter) for MTProto obfuscated transports.
///
/// Port of `MTP::CTRState` and `MTP::aesCtrEncrypt` in `mtproto/mtproto_auth_key.cpp`,
/// matching OpenSSL's `CRYPTO_ctr128_encrypt` byte-for-byte.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct CtrState {
    key: [u8; 32],
    /// Current 128-bit counter block (incremented as big-endian after each keystream block).
    pub ivec: [u8; AES_BLOCK_SIZE],
    /// Offset within the current keystream block (`0..16`).
    pub num: u32,
    /// Cached keystream block (`AES_encrypt(ivec)`).
    pub ecount: [u8; AES_BLOCK_SIZE],
}

impl CtrState {
    /// Create a new AES-256-CTR state from a 32-byte key and 16-byte initial IV/counter.
    pub fn new(key: [u8; 32], iv: [u8; AES_BLOCK_SIZE]) -> Self {
        Self {
            key,
            ivec: iv,
            num: 0,
            ecount: [0u8; AES_BLOCK_SIZE],
        }
    }

    /// Encrypt or decrypt `data` in place, advancing the CTR keystream state.
    pub fn apply_keystream(&mut self, data: &mut [u8]) {
        let cipher = Aes256::new(GenericArray::from_slice(&self.key));
        let mut n = (self.num as usize) % AES_BLOCK_SIZE;

        for byte in data.iter_mut() {
            if n == 0 {
                self.ecount = self.ivec;
                cipher.encrypt_block(GenericArray::from_mut_slice(&mut self.ecount));
                increment_be_128(&mut self.ivec);
            }
            *byte ^= self.ecount[n];
            n = (n + 1) % AES_BLOCK_SIZE;
        }

        self.num = n as u32;
    }

    /// Alias for [`Self::apply_keystream`].
    pub fn encrypt(&mut self, data: &mut [u8]) {
        self.apply_keystream(data);
    }

    /// Alias for [`Self::apply_keystream`].
    pub fn decrypt(&mut self, data: &mut [u8]) {
        self.apply_keystream(data);
    }
}

/// Apply AES-256-CTR in place using an existing [`CtrState`] (port of `MTP::aesCtrEncrypt`).
pub fn aes_ctr_encrypt(data: &mut [u8], state: &mut CtrState) {
    state.apply_keystream(data);
}

/// Increment a 128-bit big-endian counter by 1 (matching OpenSSL `ctr128_inc`).
pub(crate) fn increment_be_128(counter: &mut [u8; AES_BLOCK_SIZE]) {
    for byte in counter.iter_mut().rev() {
        let (next, overflow) = byte.overflowing_add(1);
        *byte = next;
        if !overflow {
            break;
        }
    }
}

/// Encrypt `data` using AES-256-CBC (no padding; `data.len()` must be a multiple of 16).
///
/// Used by Telegram Passport secret encryption (`passport/passport_encryption.cpp`).
pub fn aes_cbc_encrypt(
    data: &[u8],
    key: &[u8; 32],
    iv: &[u8; AES_BLOCK_SIZE],
) -> Result<Vec<u8>, CryptoError> {
    if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: data.len() });
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for chunk in data.as_chunks::<AES_BLOCK_SIZE>().0 {
        let mut block = [0u8; AES_BLOCK_SIZE];
        for i in 0..AES_BLOCK_SIZE {
            block[i] = chunk[i] ^ prev[i];
        }
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut block));
        out.extend_from_slice(&block);
        prev = block;
    }
    prev.zeroize();
    Ok(out)
}

/// Decrypt `data` using AES-256-CBC (no padding; `data.len()` must be a multiple of 16).
///
/// Used by Telegram Passport secret decryption (`passport/passport_encryption.cpp`).
pub fn aes_cbc_decrypt(
    data: &[u8],
    key: &[u8; 32],
    iv: &[u8; AES_BLOCK_SIZE],
) -> Result<Vec<u8>, CryptoError> {
    if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(CryptoError::InvalidBlockLength { len: data.len() });
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for chunk in data.as_chunks::<AES_BLOCK_SIZE>().0 {
        let mut block = *chunk;
        cipher.decrypt_block(GenericArray::from_mut_slice(&mut block));
        for i in 0..AES_BLOCK_SIZE {
            block[i] ^= prev[i];
        }
        out.extend_from_slice(&block);
        prev = *chunk;
    }
    prev.zeroize();
    Ok(out)
}
