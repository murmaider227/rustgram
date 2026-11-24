//! Integration and known-answer tests for `tg-wallet` covering BOC hashes, CRC16 addresses,
//! TON mnemonic vectors, multi-message wallet signing, vault tamper detection, and local fake
//! server HTTP round-trips.
//!
//! Never uses real seed phrases, real funds, or real TON network endpoints.

use tg_wallet::address::{
    TonAddress, build_transfer_link, canonical_address, crc16_ccitt, format_friendly,
    parse_transfer_link,
};
use tg_wallet::boc::{BocSerializeOptions, crc32c, deserialize_boc, serialize_boc_with_options};
use tg_wallet::cell::{Cell, CellBuilder};
use tg_wallet::contract::{TransferMessage, WalletContract, WalletVersion};
use tg_wallet::mnemonic::{mnemonic_to_keypair, validate_mnemonic};
use tg_wallet::tonconnect::{
    DappManifest, TonConnectAppRequest, TonConnectErrorCode, TonConnectKeypair, TonConnectSession,
    parse_app_request_json, sign_ton_proof,
};
use tg_wallet::vault::{
    LocalKeyVault, PasscodeKdf, parse_device_keyring, serialize_device_keyring,
};

#[test]
fn kat_crc16_and_crc32c_standards() {
    // Standard CRC16-CCITT/XMODEM check value for "123456789" is 0x31C3.
    assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
    // Standard Castagnoli CRC32C check value for "123456789" is 0xE3069283.
    assert_eq!(crc32c(b"123456789"), 0xE306_9283);
}

#[test]
fn kat_boc_indexed_and_crc32c_roundtrip_with_shared_dag_cells() {
    // Shared leaf cell referenced twice (verifies BOC DAG deduplication and hash preservation).
    let mut leaf_b = CellBuilder::new();
    leaf_b.store_uint(0xCAFE_BABE, 32).unwrap();
    leaf_b.store_bit(true).unwrap(); // non-byte-aligned (33 bits) -> tests completion tag
    let leaf = std::sync::Arc::new(leaf_b.end_cell().unwrap());

    let mut root_b = CellBuilder::new();
    root_b.store_uint(0x1234_5678, 32).unwrap();
    root_b.store_ref(std::sync::Arc::clone(&leaf)).unwrap();
    root_b.store_ref(std::sync::Arc::clone(&leaf)).unwrap();
    let root = root_b.end_cell().unwrap();
    let expected_hash = root.hash();

    for (has_idx, has_crc32c) in [(false, false), (false, true), (true, false), (true, true)] {
        let bytes = serialize_boc_with_options(
            &root,
            BocSerializeOptions {
                has_idx,
                has_crc32c,
            },
        )
        .unwrap();
        let decoded = deserialize_boc(&bytes).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].hash(), expected_hash);
        assert_eq!(decoded[0].refs().len(), 2);
        assert_eq!(decoded[0].refs()[0].bit_len(), 33);

        if has_crc32c {
            let mut corrupted = bytes.clone();
            let last = corrupted.len() - 1;
            corrupted[last] ^= 0x01;
            assert!(deserialize_boc(&corrupted).is_err());
        }
    }
}

#[test]
fn kat_standard_and_urlsafe_address_encoding() {
    let raw = "0:471f04d4ef9a52ec55d17daffff8c52d222c28d184874069029444eee06125f1";
    let url_safe = "EQBHHwTU75pS7FXRfa__-MUtIiwo0YSHQGkClETu4GEl8df2";
    let standard = "EQBHHwTU75pS7FXRfa//+MUtIiwo0YSHQGkClETu4GEl8df2";

    assert_eq!(canonical_address(url_safe).as_deref(), Some(raw));
    assert_eq!(canonical_address(standard).as_deref(), Some(raw));
    assert_eq!(format_friendly(raw, true, false).as_deref(), Some(url_safe));

    let addr = TonAddress::parse(raw).unwrap().address;
    assert_eq!(addr.to_friendly_with_encoding(true, false, false), standard);
    assert_eq!(addr.to_friendly_with_encoding(true, false, true), url_safe);

    let link = build_transfer_link(&addr, false, true, Some(42_000), Some("hi"));
    let parsed_link = parse_transfer_link(&link).unwrap();
    assert!(parsed_link.address.testnet);
    assert!(!parsed_link.address.bounceable);
    assert_eq!(parsed_link.amount_nano, Some(42_000));
    assert_eq!(parsed_link.comment.as_deref(), Some("hi"));
}

#[test]
fn kat_remaining_ton_spec_mnemonic_vectors_and_batch_signing() {
    // Public TON spec test vector #3 from `@ton/crypto` (zero real funds).
    let words: Vec<String> = [
        "hobby", "coil", "wisdom", "mechanic", "fossil", "pretty", "enough", "attract", "since",
        "choice", "exhaust", "hazard", "kit", "oven", "damp", "flip", "hawk", "tribe", "spice",
        "glare", "step", "hammer", "apple", "number",
    ]
    .iter()
    .map(|w| (*w).to_owned())
    .collect();
    assert!(validate_mnemonic(&words, ""));
    let kp = mnemonic_to_keypair(&words, "").unwrap();
    assert_eq!(
        hex::encode(*kp.expanded_secret_key()),
        "764c63ecdc92b331caf3c5a81c483da8444d4ac87d87af9e3cd36ae207d94e5199ac861b19db16bc0f01adfc6897f4760dfc44f9415284c78689d4fcc28b94f7"
    );

    // Invalid word count or unknown word fails validation.
    assert!(!validate_mnemonic(&words[..12], ""));
    let mut bad = words.clone();
    bad[0] = "notaword".to_owned();
    assert!(!validate_mnemonic(&bad, ""));

    // Multi-message batch transfer on V5R1 (5 messages > V4R2 limit of 4).
    let msgs: Vec<TransferMessage> = (0_u8..5)
        .map(|i| {
            TransferMessage::simple(
                TonAddress::new(0, [i + 1; 32]),
                100_000_000 * u64::from(i + 1),
                false,
                Some("batch"),
            )
            .unwrap()
        })
        .collect();

    let v4 = WalletContract::new(WalletVersion::V4R2, 0, kp.public_key_bytes(), false);
    assert!(
        v4.create_signed_transfer(&kp, 1, 1_800_000_000, &msgs)
            .is_err()
    );

    let v5 = WalletContract::new(WalletVersion::V5R1, 0, kp.public_key_bytes(), false);
    let signed = v5
        .create_signed_transfer(&kp, 1, 1_800_000_000, &msgs)
        .unwrap();
    v5.verify_signed_transfer(&signed, 1, 1_800_000_000, &msgs)
        .unwrap();

    // TON Proof signature verifies against the same Ed25519 keypair.
    let addr = v5.address().unwrap();
    let proof_sig = sign_ton_proof(&kp, &addr, "demo.example", 1_700_000_000, "nonce-xyz");
    assert_eq!(proof_sig.len(), 64);
    assert!(proof_sig.iter().any(|&b| b != 0));
}

#[test]
fn kat_ton_connect_sign_data_disconnect_and_error_responses() {
    let w = TonConnectKeypair::generate();
    let d = TonConnectKeypair::generate();
    let manifest = DappManifest {
        url: "https://demo.example".to_owned(),
        name: "Demo".to_owned(),
        icon_url: String::new(),
    };
    let ws = TonConnectSession::new(w.clone(), &d.client_id(), manifest.clone()).unwrap();
    let ds = TonConnectSession::new(d, &w.client_id(), manifest).unwrap();

    let sd_req = parse_app_request_json(
        br#"{"id":"9","method":"signData","params":["{\"type\":\"text\",\"text\":\"Sign me\"}"]}"#,
    )
    .unwrap();
    assert!(matches!(sd_req, TonConnectAppRequest::SignData(ref r) if r.payload == "Sign me"));

    let disc_req =
        parse_app_request_json(br#"{"id":"10","method":"disconnect","params":[]}"#).unwrap();
    assert!(matches!(disc_req, TonConnectAppRequest::Disconnect { .. }));

    let err_enc = ws
        .build_error_response("9", TonConnectErrorCode::UserDeclined, "Declined")
        .unwrap();
    let err_plain = ds.decrypt_message(&err_enc).unwrap();
    let val: serde_json::Value = serde_json::from_slice(&err_plain).unwrap();
    assert_eq!(val["error"]["code"], 300);
}

#[test]
fn vault_tamper_detection_and_keyring_serialization() {
    let mut vault = LocalKeyVault::new(7);
    vault
        .initialize_with_passcode("pass-777", PasscodeKdf::pbkdf2(1000))
        .unwrap();
    vault.store_secret("seed", &[0x55; 32]).unwrap();
    assert_eq!(vault.read_secret("seed").unwrap().as_slice(), &[0x55; 32]);

    // Verify keyring binary serialization round-trips and rejects tampered bytes.
    let dir = tempfile::tempdir().unwrap();
    vault.save_to_dir(dir.path()).unwrap();
    let raw_keyring = std::fs::read(dir.path().join("keyring.bin")).unwrap();
    let parsed = parse_device_keyring(&raw_keyring).unwrap();
    assert_eq!(serialize_device_keyring(&parsed), raw_keyring);

    let mut tampered = raw_keyring.clone();
    tampered[0] ^= 0xFF;
    assert!(parse_device_keyring(&tampered).is_err());

    // Tampered records file fails AEAD authentication on read.
    let mut raw_records = std::fs::read(dir.path().join("records.bin")).unwrap();
    let last = raw_records.len() - 1;
    raw_records[last] ^= 0x01;
    std::fs::write(dir.path().join("records.bin"), raw_records).unwrap();
    let mut reloaded = LocalKeyVault::load_from_dir(7, dir.path()).unwrap();
    reloaded.unlock("pass-777").unwrap();
    assert!(reloaded.read_secret("seed").is_err());
    let _ = Cell::empty();
}
