//! PQ factorization, Diffie-Hellman parameter validation, and modular exponentiation helpers.
//!
//! Port of `mtproto/mtproto_dh_utils.cpp` (`IsPrimeAndGood`, `IsGoodModExpFirst`,
//! `CreateModExp`, `CreateAuthKey`) and `mtproto/details/mtproto_dc_key_creator.cpp`
//! (`FactorizeSmallPQ`, `FactorizeBigPQ`, `FactorizePQ`, DH AES key/IV & nonce hashes).

use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::CryptoError;
use crate::hash::sha1;
use crate::random::{random_bytes, random_u64};

/// Known-good 2048-bit Telegram DH safe prime (`GoodPrime` in `mtproto/mtproto_dh_utils.cpp`).
pub const GOOD_PRIME: [u8; 256] = [
    0xC7, 0x1C, 0xAE, 0xB9, 0xC6, 0xB1, 0xC9, 0x04, 0x8E, 0x6C, 0x52, 0x2F, 0x70, 0xF1, 0x3F, 0x73,
    0x98, 0x0D, 0x40, 0x23, 0x8E, 0x3E, 0x21, 0xC1, 0x49, 0x34, 0xD0, 0x37, 0x56, 0x3D, 0x93, 0x0F,
    0x48, 0x19, 0x8A, 0x0A, 0xA7, 0xC1, 0x40, 0x58, 0x22, 0x94, 0x93, 0xD2, 0x25, 0x30, 0xF4, 0xDB,
    0xFA, 0x33, 0x6F, 0x6E, 0x0A, 0xC9, 0x25, 0x13, 0x95, 0x43, 0xAE, 0xD4, 0x4C, 0xCE, 0x7C, 0x37,
    0x20, 0xFD, 0x51, 0xF6, 0x94, 0x58, 0x70, 0x5A, 0xC6, 0x8C, 0xD4, 0xFE, 0x6B, 0x6B, 0x13, 0xAB,
    0xDC, 0x97, 0x46, 0x51, 0x29, 0x69, 0x32, 0x84, 0x54, 0xF1, 0x8F, 0xAF, 0x8C, 0x59, 0x5F, 0x64,
    0x24, 0x77, 0xFE, 0x96, 0xBB, 0x2A, 0x94, 0x1D, 0x5B, 0xCD, 0x1D, 0x4A, 0xC8, 0xCC, 0x49, 0x88,
    0x07, 0x08, 0xFA, 0x9B, 0x37, 0x8E, 0x3C, 0x4F, 0x3A, 0x90, 0x60, 0xBE, 0xE6, 0x7C, 0xF9, 0xA4,
    0xA4, 0xA6, 0x95, 0x81, 0x10, 0x51, 0x90, 0x7E, 0x16, 0x27, 0x53, 0xB5, 0x6B, 0x0F, 0x6B, 0x41,
    0x0D, 0xBA, 0x74, 0xD8, 0xA8, 0x4B, 0x2A, 0x14, 0xB3, 0x14, 0x4E, 0x0E, 0xF1, 0x28, 0x47, 0x54,
    0xFD, 0x17, 0xED, 0x95, 0x0D, 0x59, 0x65, 0xB4, 0xB9, 0xDD, 0x46, 0x58, 0x2D, 0xB1, 0x17, 0x8D,
    0x16, 0x9C, 0x6B, 0xC4, 0x65, 0xB0, 0xD6, 0xFF, 0x9C, 0xA3, 0x92, 0x8F, 0xEF, 0x5B, 0x9A, 0xE4,
    0xE4, 0x18, 0xFC, 0x15, 0xE8, 0x3E, 0xBE, 0xA0, 0xF8, 0x7F, 0xA9, 0xFF, 0x5E, 0xED, 0x70, 0x05,
    0x0D, 0xED, 0x28, 0x49, 0xF4, 0x7B, 0xF9, 0x59, 0xD9, 0x56, 0x85, 0x0C, 0xE9, 0x29, 0x85, 0x1F,
    0x0D, 0x81, 0x15, 0xF6, 0x35, 0xB1, 0x05, 0xEE, 0x2E, 0x4E, 0x15, 0xD0, 0x4B, 0x24, 0x54, 0xBF,
    0x6F, 0x4F, 0xAD, 0xF0, 0x34, 0xB1, 0x04, 0x03, 0x11, 0x9C, 0xD8, 0xE3, 0xB9, 0x2F, 0xCC, 0x5B,
];

/// Small primes for fast trial division before Miller-Rabin.
const SMALL_PRIMES: &[u32] = &[
    3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163, 167, 173, 179, 181, 191, 193,
    197, 199, 211, 223, 227, 229, 233, 239, 241, 251, 257, 263, 269, 271, 277, 281, 283, 293, 307,
    311, 313, 317, 331, 337, 347, 349, 353, 359, 367, 373, 379, 383, 389, 397, 401, 409, 419, 421,
    431, 433, 439, 443, 449, 457, 461, 463, 467, 479, 487, 491, 499, 503, 509, 521, 523, 541,
];

/// Result of [`create_mod_exp`] (`MTP::ModExpFirst` in `mtproto/mtproto_dh_utils.h`).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ModExpFirst {
    /// Big-endian `g^b mod p` bytes.
    pub modexp: Vec<u8>,
    /// The 256-byte secret exponent `b`.
    pub random_power: [u8; 256],
}

impl std::fmt::Debug for ModExpFirst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModExpFirst")
            .field("modexp_len", &self.modexp.len())
            .finish_non_exhaustive()
    }
}

/// Factorize a 64-bit integer `pq` into `(p, q)` with `p <= q` and `p * q == pq`
/// using Brent's variant of Pollard's rho algorithm (port of `FactorizeSmallPQ` in
/// `mtproto/details/mtproto_dc_key_creator.cpp`).
///
/// If `pq < 2` or `pq` is prime, returns `(1, pq)`.
pub fn factorize(pq: u64) -> (u64, u64) {
    if pq < 2 {
        return (1, pq);
    }
    if pq.is_multiple_of(2) {
        return (2, pq / 2);
    }
    for &sp in SMALL_PRIMES {
        let d = u64::from(sp);
        if pq == d {
            return (1, pq);
        }
        if pq.is_multiple_of(d) {
            let other = pq / d;
            return if d <= other { (d, other) } else { (other, d) };
        }
    }
    if is_prime_u64(pq) {
        return (1, pq);
    }

    let n = u128::from(pq);
    let mut attempt: u64 = 0;
    loop {
        let c = u128::from(17 + (attempt % 1024));
        let mut y = u128::from((random_u64() % (pq - 1)) + 1);
        let mut r: u64 = 1;
        let mut q: u128 = 1;
        let mut g: u64 = 1;
        let mut x = y;
        let mut ys = y;
        const M: u64 = 128;

        while g == 1 {
            x = y;
            for _ in 0..r {
                y = (y * y + c) % n;
            }
            let mut k = 0;
            while k < r && g == 1 {
                ys = y;
                let lim = M.min(r - k);
                for _ in 0..lim {
                    y = (y * y + c) % n;
                    let diff = x.abs_diff(y);
                    q = (q * diff) % n;
                }
                g = gcd_u64(q as u64, pq);
                k += lim;
            }
            r = r.saturating_mul(2);
            if r > (1 << 24) {
                break;
            }
        }

        if g == pq {
            loop {
                ys = (ys * ys + c) % n;
                let diff = x.abs_diff(ys);
                g = gcd_u64(diff as u64, pq);
                if g > 1 {
                    break;
                }
            }
        }

        if g > 1 && g < pq {
            let other = pq / g;
            return if g <= other { (g, other) } else { (other, g) };
        }
        attempt = attempt.wrapping_add(1);
    }
}

/// Factorize an arbitrary-size big-endian unsigned integer `pq` into `(p, q)` with `p <= q`
/// (port of `FactorizePQ` / `FactorizeBigPQ` in `mtproto/details/mtproto_dc_key_creator.cpp`).
pub fn factorize_bytes(pq_bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let pq = BigUint::from_bytes_be(pq_bytes);
    if pq < BigUint::from(4u32) {
        return Err(CryptoError::FactorizationFailed);
    }

    if let Some(small) = pq.to_u64() {
        let (p, q) = factorize(small);
        if p <= 1 || p.checked_mul(q) != Some(small) {
            return Err(CryptoError::FactorizationFailed);
        }
        return Ok((
            BigUint::from(p).to_bytes_be(),
            BigUint::from(q).to_bytes_be(),
        ));
    }

    // BigUint Brent-Pollard rho (for pq >= 2^64).
    let two = BigUint::from(2u32);
    if (&pq & BigUint::one()).is_zero() {
        let q: BigUint = &pq >> 1usize;
        return Ok((two.to_bytes_be(), q.to_bytes_be()));
    }
    for &sp in SMALL_PRIMES {
        let d = BigUint::from(sp);
        if (&pq % &d).is_zero() {
            let q = &pq / &d;
            return if d <= q {
                Ok((d.to_bytes_be(), q.to_bytes_be()))
            } else {
                Ok((q.to_bytes_be(), d.to_bytes_be()))
            };
        }
    }

    for i in 0..16u32 {
        let t = BigUint::from(17u32 + (random_u64() % 16) as u32);
        let mut a = BigUint::from(random_u64());
        let mut b = a.clone();
        let lim = 1u32 << (i.min(4) + 20);
        for j in 1..lim {
            a = (&a * &a + &t) % &pq;
            let diff = if a >= b { &a - &b } else { &b - &a };
            let g = gcd_biguint(diff, pq.clone());
            if g > BigUint::one() && g < pq {
                let other = &pq / &g;
                return if g <= other {
                    Ok((g.to_bytes_be(), other.to_bytes_be()))
                } else {
                    Ok((other.to_bytes_be(), g.to_bytes_be()))
                };
            }
            if (j & (j - 1)) == 0 {
                b = a.clone();
            }
        }
    }

    Err(CryptoError::FactorizationFailed)
}

/// Validate that `prime_bytes` is a 2048-bit safe prime (`p` and `(p - 1) / 2` both prime)
/// and that generator `g` is valid for `p` (port of `MTP::IsPrimeAndGood` in
/// `mtproto/mtproto_dh_utils.cpp`).
pub fn is_prime_and_good(prime_bytes: &[u8], g: i32) -> bool {
    if prime_bytes == GOOD_PRIME && matches!(g, 3 | 4 | 5 | 7) {
        return true;
    }

    let prime = BigUint::from_bytes_be(prime_bytes);
    if prime.bits() != 2048 {
        return false;
    }

    // Check generator g quadratic residue constraints before expensive primality tests.
    let g_ok = match g {
        2 => mod_u32(&prime, 8) == 7,
        3 => mod_u32(&prime, 3) == 2,
        4 => true,
        5 => matches!(mod_u32(&prime, 5), 1 | 4),
        6 => matches!(mod_u32(&prime, 24), 19 | 23),
        7 => matches!(mod_u32(&prime, 7), 3 | 5 | 6),
        _ => false,
    };
    if !g_ok {
        return false;
    }

    let half = (&prime - BigUint::one()) >> 1;
    // Run fast trial division on both p and (p - 1)/2 first.
    if !passes_small_prime_trial(&prime) || !passes_small_prime_trial(&half) {
        return false;
    }

    // 64 rounds of Miller-Rabin matching OpenSSL `kMillerRabinIterationCount = 64`.
    is_probable_prime_mr(&prime, 64) && is_probable_prime_mr(&half, 64)
}

/// Check that a DH public value `g_a` or `g_b` satisfies MTProto range bounds:
/// `2^{2048 - 64} <= modexp <= prime - 2^{2048 - 64}` (more precisely,
/// `modexp.bits() >= 1984`, `(prime - modexp).bits() >= 1984`, and `modexp <= 256 bytes`).
///
/// Port of `MTP::IsGoodModExpFirst` in `mtproto/mtproto_dh_utils.cpp`.
pub fn is_good_mod_exp_first(modexp: &BigUint, prime: &BigUint) -> bool {
    const MIN_DIFF_BITS: u64 = 2048 - 64;
    const MAX_MODEXP_BITS: u64 = 256 * 8;

    if modexp >= prime {
        return false;
    }
    let diff = prime - modexp;
    diff.bits() >= MIN_DIFF_BITS
        && modexp.bits() >= MIN_DIFF_BITS
        && modexp.bits() <= MAX_MODEXP_BITS
}

/// Byte-slice convenience wrapper for [`is_good_mod_exp_first`].
pub fn is_good_mod_exp_first_bytes(modexp_bytes: &[u8], prime_bytes: &[u8]) -> bool {
    let modexp = BigUint::from_bytes_be(modexp_bytes);
    let prime = BigUint::from_bytes_be(prime_bytes);
    is_good_mod_exp_first(&modexp, &prime)
}

/// Generate a random 256-byte exponent `b` (XORed with `random_seed`) and compute
/// `g^b mod p`, retrying until `g^b mod p` satisfies [`is_good_mod_exp_first`].
///
/// Port of `MTP::CreateModExp` in `mtproto/mtproto_dh_utils.cpp`.
pub fn create_mod_exp(
    g: i32,
    prime_bytes: &[u8],
    random_seed: &[u8; 256],
) -> Result<ModExpFirst, CryptoError> {
    if g <= 1 {
        return Err(CryptoError::InvalidDhParams("generator g must be > 1"));
    }
    let prime = BigUint::from_bytes_be(prime_bytes);
    if prime.bits() != 2048 {
        return Err(CryptoError::InvalidDhParams("prime must be 2048 bits"));
    }
    let g_bn = BigUint::from(g as u32);
    let mut random_power = [0u8; 256];

    for _ in 0..1024 {
        random_bytes(&mut random_power);
        for (dst, &seed) in random_power.iter_mut().zip(random_seed.iter()) {
            *dst ^= seed;
        }
        let exp = BigUint::from_bytes_be(&random_power);
        let modexp = g_bn.modpow(&exp, &prime);
        if is_good_mod_exp_first(&modexp, &prime) {
            return Ok(ModExpFirst {
                modexp: modexp.to_bytes_be(),
                random_power,
            });
        }
    }

    random_power.zeroize();
    Err(CryptoError::InvalidDhParams(
        "failed to generate valid DH modexp",
    ))
}

/// Compute the shared DH authorization key bytes `first^random mod prime` after verifying
/// [`is_good_mod_exp_first`] on `first` (port of `MTP::CreateAuthKey` in
/// `mtproto/mtproto_dh_utils.cpp`).
pub fn create_auth_key(
    first_bytes: &[u8],
    random_bytes_in: &[u8],
    prime_bytes: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let first = BigUint::from_bytes_be(first_bytes);
    let prime = BigUint::from_bytes_be(prime_bytes);
    if !is_good_mod_exp_first(&first, &prime) {
        return Err(CryptoError::InvalidDhParams(
            "g_a failed IsGoodModExpFirst safety check",
        ));
    }
    let exp = BigUint::from_bytes_be(random_bytes_in);
    Ok(first.modpow(&exp, &prime).to_bytes_be())
}

/// Compute big-endian `(base ^ exp) mod modulus`.
pub fn mod_exp(base: &[u8], exp: &[u8], modulus: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let m = BigUint::from_bytes_be(modulus);
    if m.is_zero() {
        return Err(CryptoError::InvalidDhParams("modulus cannot be zero"));
    }
    let b = BigUint::from_bytes_be(base);
    let e = BigUint::from_bytes_be(exp);
    Ok(b.modpow(&e, &m).to_bytes_be())
}

/// Derive temporary `(aes_key, aes_iv)` for encrypting/decrypting `server_DH_inner_data`
/// and `client_DH_inner_data` during auth key creation
/// (`mtproto/details/mtproto_dc_key_creator.cpp:611-633`).
pub fn dh_server_aes_key_iv(new_nonce: &[u8; 32], server_nonce: &[u8; 16]) -> ([u8; 32], [u8; 32]) {
    let sha1_ns = sha1(&[new_nonce, server_nonce]);
    let sha1_sn = sha1(&[server_nonce, new_nonce]);
    let sha1_nn = sha1(&[new_nonce, new_nonce]);

    let mut aes_key = [0u8; 32];
    aes_key[0..20].copy_from_slice(&sha1_ns);
    aes_key[20..32].copy_from_slice(&sha1_sn[0..12]);

    let mut aes_iv = [0u8; 32];
    aes_iv[0..8].copy_from_slice(&sha1_sn[12..20]);
    aes_iv[8..28].copy_from_slice(&sha1_nn);
    aes_iv[28..32].copy_from_slice(&new_nonce[0..4]);

    (aes_key, aes_iv)
}

/// Compute `new_nonce_hash1` / `new_nonce_hash2` / `new_nonce_hash3` (128 lower bits of
/// `SHA1(new_nonce || [number] || auth_key_aux_hash)`).
///
/// Port of `NonceDigest` in `mtproto/details/mtproto_dc_key_creator.cpp:361, 762-808`.
pub fn dh_new_nonce_hash(
    new_nonce: &[u8; 32],
    number: u8,
    auth_key_aux_hash: &[u8; 8],
) -> [u8; 16] {
    let digest = sha1(&[new_nonce, &[number], auth_key_aux_hash]);
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[4..20]);
    out
}

fn mod_u32(n: &BigUint, d: u32) -> u32 {
    (n % d).to_u32().unwrap_or(0)
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    if a == 0 {
        return b;
    }
    if b == 0 {
        return a;
    }
    let shift = (a | b).trailing_zeros();
    a >>= a.trailing_zeros();
    while b != 0 {
        b >>= b.trailing_zeros();
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        b -= a;
    }
    a << shift
}

fn gcd_biguint(mut a: BigUint, mut b: BigUint) -> BigUint {
    while !b.is_zero() {
        let r = &a % &b;
        a = b;
        b = r;
    }
    a
}

/// Deterministic Miller-Rabin primality test for all 64-bit unsigned integers.
fn is_prime_u64(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    if n == 2 || n == 3 {
        return true;
    }
    if n.is_multiple_of(2) {
        return false;
    }
    let d_shift = (n - 1).trailing_zeros();
    let d = (n - 1) >> d_shift;

    // These 12 witnesses are proven deterministic for all n < 2^64.
    const WITNESSES: [u64; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];
    'witness: for &a in &WITNESSES {
        if a >= n {
            continue;
        }
        let mut x = mod_pow_u64(a, d, n);
        if x == 1 || x == n - 1 {
            continue;
        }
        for _ in 1..d_shift {
            x = ((u128::from(x) * u128::from(x)) % u128::from(n)) as u64;
            if x == n - 1 {
                continue 'witness;
            }
        }
        return false;
    }
    true
}

fn mod_pow_u64(mut base: u64, mut exp: u64, modulus: u64) -> u64 {
    let m = u128::from(modulus);
    let mut acc: u128 = 1;
    let mut b = u128::from(base) % m;
    while exp > 0 {
        if exp & 1 == 1 {
            acc = (acc * b) % m;
        }
        b = (b * b) % m;
        exp >>= 1;
    }
    base = acc as u64;
    base
}

fn passes_small_prime_trial(n: &BigUint) -> bool {
    if n < &BigUint::from(2u32) {
        return false;
    }
    if (&(n.clone()) & BigUint::one()).is_zero() {
        return n == &BigUint::from(2u32);
    }
    for &sp in SMALL_PRIMES {
        let rem = mod_u32(n, sp);
        if rem == 0 {
            return n == &BigUint::from(sp);
        }
    }
    true
}

/// Miller-Rabin probabilistic primality test for `BigUint` with `rounds` random bases.
fn is_probable_prime_mr(n: &BigUint, rounds: usize) -> bool {
    let two = BigUint::from(2u32);
    let three = BigUint::from(3u32);
    if *n < two {
        return false;
    }
    if *n == two || *n == three {
        return true;
    }
    if (n & BigUint::one()).is_zero() {
        return false;
    }

    let n_minus_one = n - BigUint::one();
    let s = n_minus_one.trailing_zeros().unwrap_or(0);
    let d = &n_minus_one >> s;

    let byte_len = n.bits().div_ceil(8) as usize;
    let mut buf = vec![0u8; byte_len];
    let range = n - &three; // random in [0, n - 4], then + 2 -> [2, n - 2]

    'round: for _ in 0..rounds {
        random_bytes(&mut buf);
        let a = (BigUint::from_bytes_be(&buf) % &range) + &two;
        let mut x = a.modpow(&d, n);
        if x.is_one() || x == n_minus_one {
            continue;
        }
        for _ in 1..s {
            x = (&x * &x) % n;
            if x == n_minus_one {
                continue 'round;
            }
            if x.is_one() {
                return false;
            }
        }
        return false;
    }

    true
}
