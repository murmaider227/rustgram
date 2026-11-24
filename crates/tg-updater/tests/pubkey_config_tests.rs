//! Tests for build-time `RUSTGRAM_UPDATE_PUBKEY` configuration and fail-closed
//! rejection of the all-zero placeholder verifying key.

use std::path::PathBuf;

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;

use tg_updater::checker::{UpdateChecker, UpdateConfig};
use tg_updater::github::{AvailableUpdate, HttpClient};
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{
    RELEASE_SIGNING_PUBLIC_KEY, RELEASE_SIGNING_PUBLIC_KEY_CONFIGURED, UPDATE_PUBKEY_ENV_VAR,
    UpdateError, decode_public_key_hex, has_configured_release_key, is_placeholder_public_key,
    release_verifying_key, verifying_key_from_bytes, verifying_key_from_hex,
};

struct PanicHttpClient;

impl HttpClient for PanicHttpClient {
    fn get_bytes(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        _max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError> {
        panic!("HTTP client must not be contacted when verifying key is placeholder: {url}");
    }
}

#[test]
fn env_var_name_matches_contract() {
    assert_eq!(UPDATE_PUBKEY_ENV_VAR, "RUSTGRAM_UPDATE_PUBKEY");
}

#[test]
fn default_placeholder_key_fails_closed_everywhere() {
    // When built without `RUSTGRAM_UPDATE_PUBKEY`, the compiled-in key is all zeros
    // and must fail closed.
    if !RELEASE_SIGNING_PUBLIC_KEY_CONFIGURED {
        assert!(is_placeholder_public_key(&RELEASE_SIGNING_PUBLIC_KEY));
        assert!(!has_configured_release_key());
        assert!(matches!(
            release_verifying_key(),
            Err(UpdateError::UnconfiguredPublicKey)
        ));
    }

    // Explicit all-zero hex and bytes must always fail closed.
    let zero_hex = "00".repeat(32);
    assert_eq!(decode_public_key_hex(&zero_hex).unwrap(), [0u8; 32]);
    assert!(matches!(
        verifying_key_from_hex(&zero_hex),
        Err(UpdateError::UnconfiguredPublicKey)
    ));
    assert!(matches!(
        verifying_key_from_bytes(&[0u8; 32]),
        Err(UpdateError::UnconfiguredPublicKey)
    ));
}

#[test]
fn valid_hex_roundtrips_and_invalid_hex_is_rejected() {
    let signing = SigningKey::generate(&mut OsRng);
    let expected = signing.verifying_key();
    let hex_str = hex::encode(expected.to_bytes());

    let parsed = verifying_key_from_hex(&format!("  {hex_str}\n")).unwrap();
    assert_eq!(parsed.to_bytes(), expected.to_bytes());

    assert!(matches!(
        verifying_key_from_hex("not-hex"),
        Err(UpdateError::InvalidPublicKey(_))
    ));
    assert!(matches!(
        verifying_key_from_hex("abcd"),
        Err(UpdateError::InvalidPublicKey(_))
    ));
}

#[test]
fn update_checker_fails_closed_before_network_on_placeholder_key() {
    // Curve25519 accepts 32 zero bytes as the low-order point; `UpdateChecker` must still
    // fail closed before touching HTTP or disk.
    let zero_vk = VerifyingKey::from_bytes(&[0u8; 32]).unwrap();
    let checker = UpdateChecker::new(
        PanicHttpClient,
        UpdateConfig {
            github_repo: "rustgram/rustgram".into(),
            api_base_url: Some("http://127.0.0.1:1".into()),
            current_version: Version::new(0, 1, 0),
            allow_beta: false,
            target: Target::WIN_X64,
            working_dir: PathBuf::from("."),
            install_dir: PathBuf::from("."),
            verifying_key: zero_vk,
        },
    );

    assert!(matches!(
        checker.check(),
        Err(UpdateError::UnconfiguredPublicKey)
    ));

    let fake_avail = AvailableUpdate {
        version: Version::new(0, 2, 0),
        channel: UpdateChannel::Stable,
        title: "v0.2.0".into(),
        asset_name: "rustgram-0.2.0-win-x64.tupdate".into(),
        download_url: "http://127.0.0.1:1/update.tupdate".into(),
        size: 128,
        expected_sha256: None,
        sha256_sums_url: None,
        release_notes: None,
    };
    assert!(matches!(
        checker.download_and_stage(&fake_avail),
        Err(UpdateError::UnconfiguredPublicKey)
    ));
}
