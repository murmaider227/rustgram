//! Pure-Rust cryptographic primitives for Telegram Desktop (`rustgram`).
//!
//! Ports all cryptographic building blocks used by Telegram Desktop without OpenSSL:
//! - [`aes`]: AES-256-IGE, stateful AES-256-CTR (obfuscated transports), AES-256-CBC
//! - [`hash`]: SHA-1, SHA-256, SHA-512, MD5, HMAC-SHA256/SHA512, PBKDF2-HMAC-SHA512/SHA1
//! - [`auth_key`]: 256-byte [`AuthKey`], MTProto 2.0 and legacy 1.0 key derivation & message crypto
//! - [`rsa`]: built-in Telegram server RSA public keys, PEM parsing, fingerprints, `RSA_PAD`, OAEP
//! - [`dh`]: PQ factorization, safe-prime & generator validation, DH key agreement helpers
//! - [`srp`]: 2FA cloud password SRP-6a and Telegram Passport secret cryptography
//! - [`local`]: `tdata` passcode KDFs (PBKDF2-SHA512, legacy PBKDF2-SHA1, Argon2id, Scrypt),
//!   local AES-IGE block encryption, TDF MD5 checksums, and offset-based storage CTR
//! - [`random`]: OS CSPRNG helpers (`rand_core::OsRng`)

pub mod aes;
pub mod auth_key;
pub mod dh;
pub mod e2e;
pub mod error;
pub mod hash;
pub mod local;
pub mod random;
pub mod rsa;
pub mod srp;

pub use aes::{
    AES_BLOCK_SIZE, CtrState, aes_cbc_decrypt, aes_cbc_encrypt, aes_ctr_encrypt, aes_ige_decrypt,
    aes_ige_decrypt_in_place, aes_ige_encrypt, aes_ige_encrypt_in_place, try_aes_ige_decrypt,
    try_aes_ige_encrypt,
};
pub use auth_key::{
    AUTH_KEY_SIZE, AuthKey, DecryptedMessage, MAX_PADDING_V2, MIN_PADDING_V2, Side,
    compute_msg_key_v2, decrypt_data_v1, decrypt_data_v2, decrypt_message_v2, encrypt_data_v1,
    encrypt_data_v2, encrypt_data_v2_with_padding,
};
pub use dh::{
    GOOD_PRIME, ModExpFirst, create_auth_key, create_mod_exp, dh_new_nonce_hash,
    dh_server_aes_key_iv, factorize, factorize_bytes, is_good_mod_exp_first,
    is_good_mod_exp_first_bytes, is_prime_and_good, mod_exp,
};
pub use e2e::{
    E2E_ENVELOPE_NONCE_SIZE, E2E_ENVELOPE_TAG_SIZE, E2E_KEY_SIZE, E2E_SIGNATURE_SIZE, E2eKeyPair,
    open_with_shared_secret, seal_with_shared_secret, validate_ed25519_public_key, verify_ed25519,
};
pub use error::CryptoError;
pub use hash::{
    Md5Hasher, Sha256Hasher, const_time_eq, hmac_sha256, hmac_sha256_parts, hmac_sha512,
    hmac_sha512_parts, md5, md5_bytes, md5_hex, pbkdf2_hmac_sha1, pbkdf2_hmac_sha512,
    pbkdf2_hmac_sha512_64, sha1, sha1_bytes, sha256, sha256_bytes, sha512, sha512_bytes,
};
pub use local::{
    LOCAL_ENCRYPT_ITER_COUNT, LOCAL_ENCRYPT_NO_PWD_ITER_COUNT, LOCAL_ENCRYPT_SALT_SIZE,
    LOCAL_ENCRYPT_STRONG_ITER_COUNT, PASSCODE_SALT_MIN_SIZE, PasscodeKdf, PasscodeKdfKind,
    STORAGE_CTR_SALT_SIZE, StorageCtrState, StorageEncryptionKey, TDF_MAGIC, aes_decrypt_local,
    aes_encrypt_local, create_legacy_local_key, create_local_key, create_passcode_key,
    decrypt_local_data, encrypt_local_data, tdf_checksum, verify_tdf_checksum,
};
pub use random::{random_array, random_bytes, random_i64, random_u32, random_u64};
