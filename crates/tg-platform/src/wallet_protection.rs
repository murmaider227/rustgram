//! Windows DPAPI key protection for the built-in TON wallet vault.
//!
//! Port origin: `Telegram/SourceFiles/platform/win/wallet_protection_win.h` and
//! `Telegram/SourceFiles/platform/win/wallet_protection_win.cpp`.
//!
//! Provides a platform-abstracted [`WalletKeyProtector`] trait implemented by
//! [`WindowsDpapiKeyProtector`] (backed by Win32 `CryptProtectData` / `CryptUnprotectData`
//! scoped to the current Windows user with domain-separated entropy and
//! `CRYPTPROTECT_UI_FORBIDDEN`) and [`FakeWalletKeyProtector`] for deterministic cross-platform
//! unit and integration tests.

use std::sync::Arc;

use crate::{PlatformError, Result};

/// Domain-separation prefix mixed into DPAPI optional entropy so wallet blobs cannot be
/// cross-decrypted with unrelated DPAPI payloads created by the same Windows user.
pub const WALLET_DPAPI_ENTROPY_LABEL: &[u8] = b"tgfork-wallet-vault/windows-dpapi-wrap/v1";

/// Classification of wallet protection failures matching tdesktop's `Wallet::ProtectionError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletProtectionErrorKind {
    /// No error.
    None,
    /// Platform protection is unavailable or returned an unexpected OS error.
    Unavailable,
    /// User cancelled an interactive OS prompt.
    Cancelled,
    /// Protection key/credential is absent.
    Absent,
    /// Protected payload is malformed, tampered, or failed integrity verification.
    Corrupt,
}

/// Platform key-protection provider for sealing and unsealing wallet vault unlock material.
pub trait WalletKeyProtector: Send + Sync {
    /// Returns `true` when this protector can seal/unseal keys on the current host.
    fn is_available(&self) -> bool;

    /// Short human-readable name for Settings UI and diagnostics.
    fn label(&self) -> &'static str;

    /// Protects `plaintext` key material bound to caller-supplied `entropy` (e.g. account ID).
    fn protect(&self, plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>>;

    /// Unprotects a blob previously returned by [`Self::protect`] with the same `entropy`.
    fn unprotect(&self, protected: &[u8], entropy: &[u8]) -> Result<Vec<u8>>;
}

/// Returns the default platform [`WalletKeyProtector`] (`WindowsDpapiKeyProtector`).
#[must_use]
pub fn default_wallet_key_protector() -> Arc<dyn WalletKeyProtector> {
    Arc::new(WindowsDpapiKeyProtector::new())
}

/// Builds domain-separated entropy combining [`WALLET_DPAPI_ENTROPY_LABEL`] and caller `entropy`.
fn build_entropy(entropy: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(WALLET_DPAPI_ENTROPY_LABEL.len() + 1 + entropy.len());
    out.extend_from_slice(WALLET_DPAPI_ENTROPY_LABEL);
    out.push(0);
    out.extend_from_slice(entropy);
    out
}

/// Windows DPAPI (`CryptProtectData` / `CryptUnprotectData`) implementation of
/// [`WalletKeyProtector`].
///
/// Ciphertexts are bound to the current Windows user logon credentials and cannot be decrypted
/// by other Windows users or on other machines without the user's DPAPI master key.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsDpapiKeyProtector;

impl WindowsDpapiKeyProtector {
    /// Creates a new [`WindowsDpapiKeyProtector`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
mod win_impl {
    use std::ptr;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;

    use super::build_entropy;
    use crate::{PlatformError, Result};

    /// RAII guard that zeroes and frees a `CRYPT_INTEGER_BLOB` allocated by Win32 DPAPI.
    struct DpapiBlobGuard {
        blob: CRYPT_INTEGER_BLOB,
        zero_on_drop: bool,
    }

    impl DpapiBlobGuard {
        fn new(zero_on_drop: bool) -> Self {
            Self {
                blob: CRYPT_INTEGER_BLOB::default(),
                zero_on_drop,
            }
        }

        fn as_slice(&self) -> &[u8] {
            if self.blob.pbData.is_null() || self.blob.cbData == 0 {
                return &[];
            }
            // SAFETY: Win32 `CryptProtectData` / `CryptUnprotectData` populated `pbData` with
            // `cbData` valid initialized bytes allocated via `LocalAlloc`.
            unsafe { std::slice::from_raw_parts(self.blob.pbData, self.blob.cbData as usize) }
        }
    }

    impl Drop for DpapiBlobGuard {
        fn drop(&mut self) {
            if !self.blob.pbData.is_null() {
                if self.zero_on_drop && self.blob.cbData > 0 {
                    // SAFETY: `pbData` points to `cbData` writable bytes owned by this guard.
                    unsafe {
                        ptr::write_bytes(self.blob.pbData, 0, self.blob.cbData as usize);
                    }
                }
                // SAFETY: `pbData` was allocated by `LocalAlloc` inside DPAPI and must be freed
                // with `LocalFree`.
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.blob.pbData.cast())));
                }
                self.blob.pbData = ptr::null_mut();
                self.blob.cbData = 0;
            }
        }
    }

    pub fn dpapi_protect(plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        if plaintext.is_empty() {
            return Err(PlatformError::InvalidArgument(
                "wallet key plaintext cannot be empty".to_owned(),
            ));
        }
        let cb_in = u32::try_from(plaintext.len()).map_err(|_| {
            PlatformError::InvalidArgument("wallet key plaintext exceeds u32 length".to_owned())
        })?;
        let mut entropy_buf = build_entropy(entropy);
        let cb_entropy = u32::try_from(entropy_buf.len()).map_err(|_| {
            PlatformError::InvalidArgument("wallet key entropy exceeds u32 length".to_owned())
        })?;

        let data_in = CRYPT_INTEGER_BLOB {
            cbData: cb_in,
            pbData: plaintext.as_ptr().cast_mut(),
        };
        let entropy_blob = CRYPT_INTEGER_BLOB {
            cbData: cb_entropy,
            pbData: entropy_buf.as_mut_ptr(),
        };
        let description: Vec<u16> = "Rustgram TON Wallet Vault\0".encode_utf16().collect();
        let mut out = DpapiBlobGuard::new(false);

        // SAFETY: All input pointers (`data_in`, `description`, `entropy_blob`) remain valid for
        // the duration of the synchronous FFI call, and `out.blob` is freed via `LocalFree` in
        // `DpapiBlobGuard::drop`.
        let res = unsafe {
            CryptProtectData(
                &data_in,
                PCWSTR(description.as_ptr()),
                Some(&entropy_blob),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out.blob,
            )
        };
        if let Err(err) = res {
            return Err(PlatformError::from_win("CryptProtectData", err));
        }
        Ok(out.as_slice().to_vec())
    }

    pub fn dpapi_unprotect(protected: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        if protected.is_empty() {
            return Err(PlatformError::InvalidArgument(
                "protected wallet key blob cannot be empty".to_owned(),
            ));
        }
        let cb_in = u32::try_from(protected.len()).map_err(|_| {
            PlatformError::InvalidArgument("protected blob exceeds u32 length".to_owned())
        })?;
        let mut entropy_buf = build_entropy(entropy);
        let cb_entropy = u32::try_from(entropy_buf.len()).map_err(|_| {
            PlatformError::InvalidArgument("wallet key entropy exceeds u32 length".to_owned())
        })?;

        let data_in = CRYPT_INTEGER_BLOB {
            cbData: cb_in,
            pbData: protected.as_ptr().cast_mut(),
        };
        let entropy_blob = CRYPT_INTEGER_BLOB {
            cbData: cb_entropy,
            pbData: entropy_buf.as_mut_ptr(),
        };
        let mut out = DpapiBlobGuard::new(true);

        // SAFETY: All input pointers remain valid during the synchronous FFI call; `out.blob`
        // contains decrypted key bytes and is zeroed then freed with `LocalFree` on drop.
        let res = unsafe {
            CryptUnprotectData(
                &data_in,
                None,
                Some(&entropy_blob),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out.blob,
            )
        };
        if let Err(err) = res {
            return Err(PlatformError::from_win("CryptUnprotectData", err));
        }
        Ok(out.as_slice().to_vec())
    }
}

impl WalletKeyProtector for WindowsDpapiKeyProtector {
    fn is_available(&self) -> bool {
        cfg!(windows)
    }

    fn label(&self) -> &'static str {
        "Windows DPAPI"
    }

    fn protect(&self, plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            win_impl::dpapi_protect(plaintext, entropy)
        }
        #[cfg(not(windows))]
        {
            let _ = (plaintext, entropy);
            Err(PlatformError::Unsupported)
        }
    }

    fn unprotect(&self, protected: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            win_impl::dpapi_unprotect(protected, entropy)
        }
        #[cfg(not(windows))]
        {
            let _ = (protected, entropy);
            Err(PlatformError::Unsupported)
        }
    }
}

/// Deterministic in-memory fake [`WalletKeyProtector`] for unit and integration tests.
///
/// Binds protected blobs to `entropy` and a 64-bit integrity tag so wrong entropy,
/// tampered ciphertexts, and unavailable-protector states behave like Windows DPAPI without
/// touching OS credentials.
#[derive(Debug, Clone)]
pub struct FakeWalletKeyProtector {
    available: bool,
    master_secret: [u8; 32],
}

impl Default for FakeWalletKeyProtector {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWalletKeyProtector {
    const MAGIC: &'static [u8; 4] = b"FKDP";

    /// Creates an available fake protector with a deterministic test master key.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            available: true,
            master_secret: [0xA5; 32],
        }
    }

    /// Creates a fake protector with a custom 32-byte master key (useful for simulating a
    /// different Windows user profile).
    #[must_use]
    pub const fn with_master_secret(master_secret: [u8; 32]) -> Self {
        Self {
            available: true,
            master_secret,
        }
    }

    /// Creates a fake protector that reports unavailable and rejects all calls with
    /// [`PlatformError::Unsupported`].
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            available: false,
            master_secret: [0; 32],
        }
    }

    fn derive_mask_byte(&self, entropy: &[u8], index: usize) -> u8 {
        let mut acc: u64 = 0xCBF2_9CE4_8422_2325;
        for &b in &self.master_secret {
            acc ^= u64::from(b);
            acc = acc.wrapping_mul(0x0000_0100_0000_01B3);
        }
        for &b in entropy {
            acc ^= u64::from(b);
            acc = acc.wrapping_mul(0x0000_0100_0000_01B3);
        }
        acc ^= index as u64;
        acc = acc.wrapping_mul(0x0000_0100_0000_01B3);
        (acc ^ (acc >> 32)) as u8
    }

    fn compute_tag(&self, entropy: &[u8], ciphertext: &[u8]) -> [u8; 8] {
        let mut h: u64 = 0x517C_C1B7_2722_0A95;
        for &b in &self.master_secret {
            h = h.rotate_left(7) ^ u64::from(b);
            h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        for &b in entropy {
            h = h.rotate_left(11) ^ u64::from(b);
            h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        for &b in ciphertext {
            h = h.rotate_left(13) ^ u64::from(b);
            h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        h.to_be_bytes()
    }
}

impl WalletKeyProtector for FakeWalletKeyProtector {
    fn is_available(&self) -> bool {
        self.available
    }

    fn label(&self) -> &'static str {
        "Fake Wallet Key Protector"
    }

    fn protect(&self, plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        if !self.available {
            return Err(PlatformError::Unsupported);
        }
        if plaintext.is_empty() {
            return Err(PlatformError::InvalidArgument(
                "wallet key plaintext cannot be empty".to_owned(),
            ));
        }
        let full_entropy = build_entropy(entropy);
        let ciphertext: Vec<u8> = plaintext
            .iter()
            .enumerate()
            .map(|(idx, &b)| b ^ self.derive_mask_byte(&full_entropy, idx))
            .collect();
        let tag = self.compute_tag(&full_entropy, &ciphertext);

        let mut out = Vec::with_capacity(Self::MAGIC.len() + ciphertext.len() + tag.len());
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(&ciphertext);
        out.extend_from_slice(&tag);
        Ok(out)
    }

    fn unprotect(&self, protected: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        if !self.available {
            return Err(PlatformError::Unsupported);
        }
        if protected.len() <= Self::MAGIC.len() + 8 {
            return Err(PlatformError::WalletProtection(
                "protected blob is truncated".to_owned(),
            ));
        }
        if &protected[..4] != Self::MAGIC {
            return Err(PlatformError::WalletProtection(
                "invalid protected blob magic".to_owned(),
            ));
        }
        let ciphertext = &protected[4..protected.len() - 8];
        let stored_tag = &protected[protected.len() - 8..];
        let full_entropy = build_entropy(entropy);
        let expected_tag = self.compute_tag(&full_entropy, ciphertext);
        if stored_tag != expected_tag {
            return Err(PlatformError::WalletProtection(
                "integrity verification failed (wrong entropy, user key, or corrupt blob)"
                    .to_owned(),
            ));
        }
        let plaintext: Vec<u8> = ciphertext
            .iter()
            .enumerate()
            .map(|(idx, &b)| b ^ self.derive_mask_byte(&full_entropy, idx))
            .collect();
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_wallet_key_protector_round_trip_and_errors() {
        let protector = FakeWalletKeyProtector::new();
        assert!(protector.is_available());

        let secret = b"synthetic-32-byte-wallet-vault-key";
        let entropy = b"account:42";
        let sealed = protector.protect(secret, entropy).unwrap();
        assert_ne!(sealed.as_slice(), secret);

        let opened = protector.unprotect(&sealed, entropy).unwrap();
        assert_eq!(opened.as_slice(), secret);

        // Wrong entropy fails.
        assert!(protector.unprotect(&sealed, b"account:99").is_err());

        // Different master secret fails.
        let other_user = FakeWalletKeyProtector::with_master_secret([0x5A; 32]);
        assert!(other_user.unprotect(&sealed, entropy).is_err());

        // Tampered ciphertext fails.
        let mut tampered = sealed.clone();
        tampered[5] ^= 0xFF;
        assert!(protector.unprotect(&tampered, entropy).is_err());

        // Empty plaintext rejected.
        assert!(protector.protect(&[], entropy).is_err());

        // Unavailable protector rejects protect/unprotect.
        let disabled = FakeWalletKeyProtector::unavailable();
        assert!(!disabled.is_available());
        assert!(matches!(
            disabled.protect(secret, entropy),
            Err(PlatformError::Unsupported)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_dpapi_round_trip_and_entropy_binding() {
        let protector = WindowsDpapiKeyProtector::new();
        assert!(protector.is_available());
        assert_eq!(protector.label(), "Windows DPAPI");

        let secret = b"synthetic-dpapi-test-key-0123456789abcdef";
        let entropy = b"account:777";
        let protected = protector.protect(secret, entropy).unwrap();
        assert!(!protected.is_empty());
        assert_ne!(protected.as_slice(), secret);

        // Same user + same entropy decrypts cleanly.
        let decrypted = protector.unprotect(&protected, entropy).unwrap();
        assert_eq!(decrypted.as_slice(), secret);

        // Mismatched entropy must fail DPAPI decryption.
        assert!(protector.unprotect(&protected, b"account:888").is_err());

        // Corrupted DPAPI blob must fail decryption.
        let mut corrupted = protected.clone();
        let mid = corrupted.len() / 2;
        corrupted[mid] ^= 0xAA;
        assert!(protector.unprotect(&corrupted, entropy).is_err());

        // Empty plaintext/ciphertext rejected.
        assert!(protector.protect(&[], entropy).is_err());
        assert!(protector.unprotect(&[], entropy).is_err());
    }
}
