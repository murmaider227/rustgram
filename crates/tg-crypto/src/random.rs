//! Cryptographically secure random number generation.
//!
//! Port of `lib_base/base/random.h` / `base/bytes.h` (`bytes::set_random`).

use rand_core::{OsRng, RngCore};

/// Fill `dest` with cryptographically secure random bytes from the OS CSPRNG.
pub fn random_bytes(dest: &mut [u8]) {
    OsRng.fill_bytes(dest);
}

/// Generate a fixed-size array of cryptographically secure random bytes.
pub fn random_array<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    OsRng.fill_bytes(&mut buf);
    buf
}

/// Generate a cryptographically secure random `u32`.
pub fn random_u32() -> u32 {
    OsRng.next_u32()
}

/// Generate a cryptographically secure random `u64`.
pub fn random_u64() -> u64 {
    OsRng.next_u64()
}

/// Generate a cryptographically secure random `i64`.
pub fn random_i64() -> i64 {
    OsRng.next_u64() as i64
}
