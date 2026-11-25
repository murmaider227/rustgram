//! Security review #1 (SEC-B) regression tests for `tg-wallet`:
//! - B7: pre-approval emulation/fee-estimate BOCs never verify under the wallet's real public key
//! - B8: TON Proof domain allowlist (`telegram.org` / subdomains blocked) and manifest host binding
//! - B9: BOC deserializer caps untrusted counts and rejects size overflows without aborting

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tg_wallet::address::TonAddress;
use tg_wallet::boc::{
    BOC_MAGIC, BOC_MAGIC_INDEXED, BOC_MAGIC_INDEXED_CRC32C, deserialize_boc, deserialize_boc_base64,
};
use tg_wallet::cell::CellBuilder;
use tg_wallet::contract::{TransferMessage, WalletContract, WalletVersion};
use tg_wallet::mnemonic::TonKeypair;
use tg_wallet::tonconnect::{
    DappManifest, TonConnectKeypair, TonConnectSession, ton_connect_manifest_hosts_match,
    ton_connect_proof_domain_allowed,
};

#[test]
fn b7_unsigned_emulation_boc_never_verifies_under_real_public_key() {
    let real_kp = TonKeypair::from_seed([0x42_u8; 32]);
    let dest = TonAddress::new(0, [0x99_u8; 32]);
    let msg = TransferMessage::simple(dest, 500_000_000, true, Some("emulation preview")).unwrap();
    let seqno = 7_u32;
    let valid_until = 1_900_000_000_u32;

    for version in [
        WalletVersion::V3R2,
        WalletVersion::V4R2,
        WalletVersion::V5R1,
    ] {
        let contract = WalletContract::new(version, 0, real_kp.public_key_bytes(), false);

        let unsigned = contract
            .create_unsigned_emulation_transfer(seqno, valid_until, std::slice::from_ref(&msg))
            .expect("build unsigned emulation BOC");

        // The BOC must parse cleanly as a valid external-in message addressed to the wallet.
        let root = deserialize_boc_base64(&unsigned.boc_base64).expect("valid BOC");
        assert!(
            !root.refs().is_empty(),
            "external message must carry body cell ref"
        );

        // Security invariant (B7): the emulation BOC MUST NOT verify under the real public key.
        assert!(
            contract
                .verify_signed_transfer(&unsigned, seqno, valid_until, std::slice::from_ref(&msg))
                .is_err(),
            "{version:?}: emulation BOC must NOT verify under real wallet public key"
        );
        let signing_hash = contract
            .build_signing_payload(seqno, valid_until, std::slice::from_ref(&msg))
            .unwrap()
            .end_cell()
            .unwrap()
            .hash();
        let vk = VerifyingKey::from_bytes(&real_kp.public_key_bytes()).unwrap();
        let sig = Signature::from_bytes(&unsigned.signature);
        assert!(
            vk.verify_strict(&signing_hash, &sig).is_err()
                && vk.verify(&signing_hash, &sig).is_err(),
            "{version:?}: emulation signature must be rejected by Ed25519 verify"
        );

        // Contrast: explicit user-confirmed `create_signed_transfer` DOES verify.
        let signed = contract
            .create_signed_transfer(&real_kp, seqno, valid_until, std::slice::from_ref(&msg))
            .expect("build signed transfer");
        contract
            .verify_signed_transfer(&signed, seqno, valid_until, std::slice::from_ref(&msg))
            .expect("confirmed transfer must verify");
    }
}

#[test]
fn b8_ton_connect_proof_domain_rejects_telegram_org_and_host_mismatch() {
    assert!(!ton_connect_proof_domain_allowed("telegram.org", ""));
    assert!(!ton_connect_proof_domain_allowed("TELEGRAM.ORG.", ""));
    assert!(!ton_connect_proof_domain_allowed("wallet.telegram.org", ""));
    assert!(!ton_connect_proof_domain_allowed(
        "fragment.sub.telegram.org.",
        ""
    ));
    assert!(!ton_connect_proof_domain_allowed("", ""));
    assert!(ton_connect_proof_domain_allowed("demoswap.example", ""));
    assert!(!ton_connect_proof_domain_allowed(
        "owned.example",
        "owned.example."
    ));

    // Manifest host binding: manifest.url host must match manifestUrl host.
    let good_manifest = DappManifest {
        url: "https://demoswap.example/app".to_owned(),
        name: "Demo Swap".to_owned(),
        icon_url: String::new(),
    };
    assert!(ton_connect_manifest_hosts_match(
        "https://demoswap.example/tonconnect-manifest.json",
        &good_manifest
    ));
    let spoofed_manifest = DappManifest {
        url: "https://victim.example".to_owned(),
        name: "Victim".to_owned(),
        icon_url: String::new(),
    };
    assert!(!ton_connect_manifest_hosts_match(
        "https://evil.example/tonconnect-manifest.json",
        &spoofed_manifest
    ));
    assert!(!ton_connect_manifest_hosts_match(
        "http://demoswap.example/tonconnect-manifest.json",
        &good_manifest
    ));

    // Defense-in-depth inside `TonConnectSession::build_connect_event`: refuses telegram.org proof.
    let real_kp = TonKeypair::from_seed([0x11_u8; 32]);
    let contract = WalletContract::new(WalletVersion::V5R1, 0, real_kp.public_key_bytes(), false);
    let wallet_tc = TonConnectKeypair::generate();
    let dapp_tc = TonConnectKeypair::generate();
    let spoof_tg = DappManifest {
        url: "https://wallet.telegram.org".to_owned(),
        name: "Spoof".to_owned(),
        icon_url: String::new(),
    };
    let sess = TonConnectSession::new(wallet_tc, &dapp_tc.client_id(), spoof_tg).unwrap();
    assert!(
        sess.build_connect_event(
            1,
            &contract,
            &real_kp,
            Some("challenge-nonce"),
            1_760_000_000
        )
        .is_err(),
        "build_connect_event must reject ton_proof for telegram.org / *.telegram.org"
    );
}

#[test]
fn b9_boc_deserializer_rejects_huge_counts_and_size_overflow_without_abort() {
    // 1. BOC_MAGIC with huge 32-bit `cells` and `roots` counts (would OOM `Vec::with_capacity`).
    let mut huge_roots = Vec::new();
    huge_roots.extend_from_slice(&BOC_MAGIC.to_be_bytes());
    huge_roots.push(0x04); // has_idx=0, has_crc=0, ref_size=4
    huge_roots.push(0x04); // offset_size=4
    huge_roots.extend_from_slice(&0xFFFF_FFFF_u32.to_be_bytes()); // cells
    huge_roots.extend_from_slice(&0xFFFF_FFFF_u32.to_be_bytes()); // roots
    huge_roots.extend_from_slice(&0_u32.to_be_bytes()); // absent
    huge_roots.extend_from_slice(&0_u32.to_be_bytes()); // total_cells_size
    assert!(
        deserialize_boc(&huge_roots).is_err(),
        "huge roots/cells must return Err, not abort on allocation"
    );

    // 2. BOC_MAGIC_INDEXED and BOC_MAGIC_INDEXED_CRC32C with huge `roots` count
    //    (old code ran `(0..roots).collect()` before checking remaining input).
    for magic in [BOC_MAGIC_INDEXED, BOC_MAGIC_INDEXED_CRC32C] {
        let mut indexed = Vec::new();
        indexed.extend_from_slice(&magic.to_be_bytes());
        indexed.push(0x04); // ref_size=4
        indexed.push(0x04); // offset_size=4
        indexed.extend_from_slice(&0xFFFF_FFFF_u32.to_be_bytes()); // cells
        indexed.extend_from_slice(&0xFFFF_FFFF_u32.to_be_bytes()); // roots
        indexed.extend_from_slice(&0_u32.to_be_bytes()); // absent
        indexed.extend_from_slice(&0_u32.to_be_bytes()); // total_cells_size
        assert!(
            deserialize_boc(&indexed).is_err(),
            "indexed BOC with huge roots must return Err, not OOM"
        );
    }

    // 3. `total_cells_size + crc_tail` overflow when `has_crc=1`, `offset_size=8`, `total_size=u64::MAX`.
    let mut overflow_crc = Vec::new();
    overflow_crc.extend_from_slice(&BOC_MAGIC.to_be_bytes());
    overflow_crc.push(0x41); // has_crc=1, ref_size=1
    overflow_crc.push(0x08); // offset_size=8
    overflow_crc.push(1); // cells=1
    overflow_crc.push(1); // roots=1
    overflow_crc.push(0); // absent=0
    overflow_crc.extend_from_slice(&u64::MAX.to_be_bytes()); // total_cells_size = u64::MAX
    overflow_crc.push(0); // root[0] = 0
    overflow_crc.extend_from_slice(&[0_u8; 8]); // padding bytes
    assert!(
        deserialize_boc(&overflow_crc).is_err(),
        "total_cells_size + crc_tail overflow must return Err cleanly"
    );

    // 4. `cells_count` larger than `total_cells_size / 2` (old code did Vec::with_capacity(cells_count)).
    let mut inflated_cells = Vec::new();
    inflated_cells.extend_from_slice(&BOC_MAGIC.to_be_bytes());
    inflated_cells.push(0x04); // has_idx=0, has_crc=0, ref_size=4
    inflated_cells.push(0x01); // offset_size=1
    inflated_cells.extend_from_slice(&0x1000_0000_u32.to_be_bytes()); // cells = 268M
    inflated_cells.extend_from_slice(&1_u32.to_be_bytes()); // roots = 1
    inflated_cells.extend_from_slice(&0_u32.to_be_bytes()); // absent = 0
    inflated_cells.push(2); // total_cells_size = 2
    inflated_cells.extend_from_slice(&0_u32.to_be_bytes()); // root[0] = 0
    inflated_cells.extend_from_slice(&[0x00, 0x00]); // 2 bytes payload
    assert!(
        deserialize_boc(&inflated_cells).is_err(),
        "cells_count exceeding payload capacity must be rejected before Vec::with_capacity"
    );

    // 5. Sanity check: a genuine small BOC still deserializes properly.
    let mut b = CellBuilder::new();
    b.store_uint(0xDEAD_BEEF, 32).unwrap();
    let cell = b.end_cell().unwrap();
    let encoded = tg_wallet::boc::serialize_boc(&cell).unwrap();
    let decoded = deserialize_boc(&encoded).expect("valid BOC must still decode");
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].hash(), cell.hash());
}
