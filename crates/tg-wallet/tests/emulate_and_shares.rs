//! WP-43 integration tests covering:
//! 1. TON Connect `signData` (`text`, `binary`, `cell`) Ed25519 signing, verification, and tamper
//!    rejection, plus encrypted bridge round-trip and claim-store persistence.
//! 2. Transaction emulation (`EmulateMessageRequest`, `parse_emulation_response`,
//!    `HttpToncenterClient::emulate_message` over local fake HTTP server, TON/Jetton/NFT balance
//!    deltas, aborted/bounced/failed-phase warnings, and tdesktop `gram_emulate_tests` +
//!    `wallet_ton_connect_emulation` edge cases).
//! 3. GF(256) Shamir secret sharing for 24-word recovery phrases (`(k=2, n=3)` all subsets,
//!    `(k=3, n=5)` subsets, insufficient share rejection, corrupted share checksum detection,
//!    mismatched-split detection, and tdesktop XOR/holder-encryption round-trips).
//!
//! Strictly uses throwaway generated keys and local loopback fakes; never touches real TON
//! networks, bridges, or funds.

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use tg_wallet::address::TonAddress;
use tg_wallet::boc::serialize_boc_base64;
use tg_wallet::cell::CellBuilder;
use tg_wallet::contract::{WalletContract, WalletVersion};
use tg_wallet::emulate::{
    EMULATE_TRACE_ENDPOINT, EmulateMessageRequest, EmulationWarning, SendEmulation,
    SendEmulationAction, TonConnectActionKind, TonConnectActionSide, TonConnectEmulationStatus,
    parse_emulation_response, parse_ton_connect_emulation,
};
use tg_wallet::jettons::{JettonTransferParams, build_jetton_transfer_body};
use tg_wallet::mnemonic::{TonKeypair, generate_mnemonic, mnemonic_to_keypair, validate_mnemonic};
use tg_wallet::phrase_shares::{
    PHRASE_SEED_LENGTH, PhraseShare, combine_shares_xor, decrypt_share_for_holder,
    encrypt_share_for_holder, parse_decrypted_key_part, reconstruct_phrase, reconstruct_secret,
    seed_from_words, serialize_decrypted_key_part, split_phrase, split_secret, split_seed_xor,
    words_from_seed,
};
use tg_wallet::toncenter::{
    HttpToncenterClient, ToncenterApi, ToncenterConfig, parse_emulated_trace,
};
use tg_wallet::tonconnect::{
    DappManifest, SignDataPayload, TonConnectAppRequest, TonConnectClaimDecision,
    TonConnectClaimRecord, TonConnectClaimStore, TonConnectKeypair, TonConnectSession, crc32_ieee,
    encode_dns_name, parse_app_request_json, sign_data_hash, sign_ton_connect_data,
    ton_connect_proof_domain_allowed, ton_connect_request_id_valid, verify_ton_connect_data,
    verify_ton_connect_data_against,
};

const ACC_WALLET: &str = "0:1111111111111111111111111111111111111111111111111111111111111111";
const ACC_RECIPIENT: &str = "0:2222222222222222222222222222222222222222222222222222222222222222";
const ACC_JETTON_MASTER: &str =
    "0:3333333333333333333333333333333333333333333333333333333333333333";
const ACC_JETTON_WALLET_A: &str =
    "0:4444444444444444444444444444444444444444444444444444444444444444";
const ACC_JETTON_WALLET_B: &str =
    "0:5555555555555555555555555555555555555555555555555555555555555555";
const ACC_NFT_ITEM: &str = "0:6666666666666666666666666666666666666666666666666666666666666666";
const ACC_NFT_COLLECTION: &str =
    "0:7777777777777777777777777777777777777777777777777777777777777777";

fn throwaway_keypair_and_address() -> (TonKeypair, TonAddress) {
    let words = generate_mnemonic("").unwrap();
    let kp = mnemonic_to_keypair(&words, "").unwrap();
    let contract = WalletContract::new(WalletVersion::V5R1, 0, kp.public_key_bytes(), false);
    let addr = contract.address().unwrap();
    (kp, addr)
}

// ============================================================================
// Acceptance Criterion 1: TON Connect `signData` (`text`, `binary`, `cell`)
// ============================================================================

#[test]
fn sign_data_text_binary_cell_verify_and_tamper_rejection() {
    let (kp, addr) = throwaway_keypair_and_address();
    let pubkey = kp.public_key_bytes();
    let domain = "demo.app.example";
    let timestamp = 1_750_000_123_u64;

    // Standard IEEE CRC-32 check value for "123456789" is 0xCBF43926.
    assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
    // TEP-81 DNS label reversal encoding.
    assert_eq!(
        encode_dns_name("demo.app.example").unwrap(),
        b"example\0app\0demo\0"
    );
    assert_eq!(
        encode_dns_name("demo.app.example.").unwrap(),
        b"example\0app\0demo\0"
    );
    assert!(encode_dns_name("").is_err());
    assert!(encode_dns_name("bad..domain").is_err());

    // Build a throwaway TVM cell for `cell` signing.
    let mut cb = CellBuilder::new();
    cb.store_uint(0x1234_5678, 32).unwrap();
    cb.store_coins(1_500_000_000).unwrap();
    let cell_boc = serialize_boc_base64(&cb.end_cell().unwrap()).unwrap();
    let schema = "transfer#12345678 amount:Coins = Transfer;";

    let payloads = [
        SignDataPayload::Text {
            text: "Confirm order #9918 on Demo App".to_owned(),
            network: Some("-239".to_owned()),
            from: None,
        },
        SignDataPayload::Binary {
            bytes: STANDARD.encode(b"\x00\x01\x02hello-binary\xfe\xff"),
            network: Some("-239".to_owned()),
            from: None,
        },
        SignDataPayload::Cell {
            schema: schema.to_owned(),
            cell: cell_boc.clone(),
            network: Some("-239".to_owned()),
            from: None,
        },
    ];

    for payload in &payloads {
        let signed = sign_ton_connect_data(&kp, &addr, domain, timestamp, payload).unwrap();
        assert_eq!(signed.address, addr.to_raw());
        assert_eq!(signed.domain, domain);
        assert_eq!(signed.timestamp, timestamp);
        assert_eq!(STANDARD.decode(&signed.signature).unwrap().len(), 64);

        // Valid verification succeeds.
        verify_ton_connect_data(&pubkey, &signed).unwrap();
        verify_ton_connect_data_against(
            &pubkey,
            &addr,
            domain,
            timestamp,
            payload,
            &signed.signature,
        )
        .unwrap();

        // Tampered domain fails verification.
        assert!(
            verify_ton_connect_data_against(
                &pubkey,
                &addr,
                "evil.app.example",
                timestamp,
                payload,
                &signed.signature,
            )
            .is_err()
        );

        // Tampered timestamp fails verification.
        assert!(
            verify_ton_connect_data_against(
                &pubkey,
                &addr,
                domain,
                timestamp + 1,
                payload,
                &signed.signature,
            )
            .is_err()
        );

        // Wrong wallet public key fails verification.
        let other_kp = TonKeypair::from_seed([0x99; 32]);
        assert!(verify_ton_connect_data(&other_kp.public_key_bytes(), &signed).is_err());

        // Wrong wallet address fails verification.
        let other_addr = TonAddress::new(0, [0xAB; 32]);
        assert!(
            verify_ton_connect_data_against(
                &pubkey,
                &other_addr,
                domain,
                timestamp,
                payload,
                &signed.signature,
            )
            .is_err()
        );

        // Tampered payload fails verification.
        let tampered_payload = match payload {
            SignDataPayload::Text { .. } => SignDataPayload::Text {
                text: "Confirm order #9919 on Demo App".to_owned(),
                network: Some("-239".to_owned()),
                from: None,
            },
            SignDataPayload::Binary { .. } => SignDataPayload::Binary {
                bytes: STANDARD.encode(b"\x00\x01\x02hello-binary\xfe\x00"),
                network: Some("-239".to_owned()),
                from: None,
            },
            SignDataPayload::Cell { .. } => SignDataPayload::Cell {
                schema: "transfer#12345679 amount:Coins = Transfer;".to_owned(),
                cell: cell_boc.clone(),
                network: Some("-239".to_owned()),
                from: None,
            },
        };
        assert!(
            verify_ton_connect_data_against(
                &pubkey,
                &addr,
                domain,
                timestamp,
                &tampered_payload,
                &signed.signature,
            )
            .is_err()
        );
    }

    // Domain separation: identical bytes signed as `text` vs `binary` must produce distinct hashes.
    let same_as_text = SignDataPayload::Text {
        text: "same-content".to_owned(),
        network: None,
        from: None,
    };
    let same_as_bin = SignDataPayload::Binary {
        bytes: STANDARD.encode(b"same-content"),
        network: None,
        from: None,
    };
    let h_text = sign_data_hash(&addr, domain, timestamp, &same_as_text).unwrap();
    let h_bin = sign_data_hash(&addr, domain, timestamp, &same_as_bin).unwrap();
    assert_ne!(h_text, h_bin);
}

#[test]
fn sign_data_encrypted_ton_connect_session_and_claims_roundtrip() {
    let (kp, addr) = throwaway_keypair_and_address();
    let wallet_box = TonConnectKeypair::generate();
    let dapp_box = TonConnectKeypair::generate();
    let manifest = DappManifest {
        url: "https://vault.example".to_owned(),
        name: "Vault Demo".to_owned(),
        icon_url: String::new(),
    };
    let wallet_sess =
        TonConnectSession::new(wallet_box.clone(), &dapp_box.client_id(), manifest.clone())
            .unwrap();
    let dapp_sess = TonConnectSession::new(dapp_box, &wallet_box.client_id(), manifest).unwrap();

    // Parse incoming encrypted `signData` request from dApp.
    let req_json = json!({
        "id": "sd-77",
        "method": "signData",
        "params": [
            json!({
                "type": "text",
                "text": "Authorize session"
            }).to_string()
        ]
    });
    let enc_req = dapp_sess
        .encrypt_message(&serde_json::to_vec(&req_json).unwrap())
        .unwrap();
    let app_req = wallet_sess.decrypt_app_request(&enc_req).unwrap();
    let TonConnectAppRequest::SignData(sd_req) = app_req else {
        panic!("expected SignData request");
    };
    assert!(ton_connect_request_id_valid(&sd_req.request_id));
    assert!(ton_connect_proof_domain_allowed(
        &wallet_sess.manifest.host(),
        "telegram.org"
    ));
    assert!(!ton_connect_proof_domain_allowed("telegram.org.", ""));

    let typed_payload = sd_req.to_sign_data_payload().unwrap();
    let result = sign_ton_connect_data(
        &kp,
        &addr,
        &wallet_sess.manifest.host(),
        1_760_000_000,
        &typed_payload,
    )
    .unwrap();
    let enc_resp = wallet_sess
        .build_sign_data_success_response(&sd_req.request_id, &result)
        .unwrap();

    // dApp decrypts response and verifies Ed25519 signature.
    let dec_bytes = dapp_sess.decrypt_message(&enc_resp).unwrap();
    let resp_val: Value = serde_json::from_slice(&dec_bytes).unwrap();
    assert_eq!(resp_val["id"], "sd-77");
    let decoded_result: tg_wallet::tonconnect::SignDataResult =
        serde_json::from_value(resp_val["result"].clone()).unwrap();
    verify_ton_connect_data(&kp.public_key_bytes(), &decoded_result).unwrap();

    // Verify claim store serialization, validation, and expiry pruning (`wallet_ton_connect_claims.cpp`).
    let mut store = TonConnectClaimStore::default();
    store.records.push(TonConnectClaimRecord {
        session_id: 42,
        msg_id: 1001,
        request_id: "sd-77".to_owned(),
        trace_id: "trace-1".to_owned(),
        expires: 1_760_000_300,
        created: 1_760_000_000,
        address: addr.to_raw(),
        public_key: kp.public_key_bytes().to_vec(),
        decision: TonConnectClaimDecision::Answer,
        answer: enc_resp.into_bytes(),
        not_sent: Vec::new(),
        operation_id: String::new(),
        signed_boc: String::new(),
    });
    assert!(store.is_valid());
    let serialized = store.to_bytes().unwrap();
    let reloaded = TonConnectClaimStore::from_bytes(&serialized).unwrap();
    assert_eq!(reloaded, store);
    assert!(reloaded.find(42, 1001).is_some());

    // Prune after expires + 24h.
    let mut pruned = reloaded;
    pruned.prune_expired(1_760_000_300 + 24 * 3600 + 1);
    assert!(pruned.records.is_empty());

    // Parse directly from JSON as well.
    let raw_req = parse_app_request_json(
        br#"{"id":"sd-bin","method":"signData","params":["{\"type\":\"binary\",\"bytes\":\"AQID\"}"]}"#,
    )
    .unwrap();
    assert!(
        matches!(raw_req, TonConnectAppRequest::SignData(r) if r.to_sign_data_payload().is_ok())
    );
}

// ============================================================================
// Acceptance Criterion 2: Transaction Emulation over mock HTTP transport
// ============================================================================

async fn spawn_mock_emulate_server(
    canned_body: String,
    captured_requests: Arc<Mutex<Vec<(String, String)>>>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let canned = canned_body.clone();
            let captured = Arc::clone(&captured_requests);
            tokio::spawn(async move {
                let mut buf = vec![0_u8; 32768];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("").to_owned();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
                captured.lock().unwrap().push((first_line, body));

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    canned.len(),
                    canned
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn emulate_message_over_mock_http_parses_fees_ton_jetton_nft_deltas() {
    let canned = json!({
        "mc_block_seqno": 88888,
        "is_incomplete": false,
        "trace": {
            "tx_hash": "tx_root",
            "children": [
                {
                    "tx_hash": "tx_jetton",
                    "children": [{ "tx_hash": "tx_notify", "children": [] }]
                },
                {
                    "tx_hash": "tx_nft",
                    "children": []
                }
            ]
        },
        "transactions_order": ["tx_root", "tx_jetton", "tx_notify", "tx_nft"],
        "transactions": {
            "tx_root": {
                "account": ACC_WALLET,
                "total_fees": "4100000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "storage_ph": { "storage_fees_collected": "100000" },
                    "compute_ph": { "success": true, "exit_code": 0, "gas_fees": "2800000" },
                    "action": { "success": true, "result_code": 0, "total_fwd_fees": "1200000" }
                },
                "account_state_before": { "balance": "10000000000" },
                "account_state_after": { "balance": "8895900000" },
                "out_msgs": []
            },
            "tx_jetton": {
                "account": ACC_JETTON_WALLET_A,
                "total_fees": "2500000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "storage_ph": { "storage_fees_collected": "50000" },
                    "compute_ph": { "success": true, "exit_code": 0, "gas_fees": "2000000" },
                    "action": { "success": true, "result_code": 0, "total_fwd_fees": "450000" }
                },
                "account_state_before": { "balance": "100000000" },
                "account_state_after": { "balance": "100000000" },
                "out_msgs": []
            },
            "tx_notify": {
                "account": ACC_RECIPIENT,
                "total_fees": "900000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "storage_ph": { "storage_fees_collected": "20000" },
                    "compute_ph": { "success": true, "exit_code": 1, "gas_fees": "880000" },
                    "action": { "success": true, "result_code": 0, "total_fwd_fees": "0" }
                },
                "account_state_before": { "balance": "5000000000" },
                "account_state_after": { "balance": "5999100000" },
                "out_msgs": []
            },
            "tx_nft": {
                "account": ACC_NFT_ITEM,
                "total_fees": "1500000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "storage_ph": { "storage_fees_collected": "30000" },
                    "compute_ph": { "success": true, "exit_code": 0, "gas_fees": "1470000" },
                    "action": { "success": true, "result_code": 0, "total_fwd_fees": "0" }
                },
                "account_state_before": { "balance": "50000000" },
                "account_state_after": { "balance": "50000000" },
                "out_msgs": []
            }
        },
        "actions": [
            {
                "type": "ton_transfer",
                "success": true,
                "details": {
                    "source": ACC_WALLET,
                    "destination": ACC_RECIPIENT,
                    "value": "1000000000"
                }
            },
            {
                "type": "jetton_transfer",
                "success": true,
                "details": {
                    "sender": ACC_WALLET,
                    "receiver": ACC_RECIPIENT,
                    "sender_jetton_wallet": ACC_JETTON_WALLET_A,
                    "receiver_jetton_wallet": ACC_JETTON_WALLET_B,
                    "asset": ACC_JETTON_MASTER,
                    "amount": "12500000"
                }
            },
            {
                "type": "nft_transfer",
                "success": true,
                "details": {
                    "old_owner": ACC_WALLET,
                    "new_owner": ACC_RECIPIENT,
                    "nft_item": ACC_NFT_ITEM,
                    "nft_collection": ACC_NFT_COLLECTION
                }
            }
        ],
        "metadata": {
            "0:3333333333333333333333333333333333333333333333333333333333333333": {
                "token_info": [{ "symbol": "USDt", "extra": { "decimals": "6" } }]
            }
        }
    })
    .to_string();

    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock_emulate_server(canned, Arc::clone(&captured)).await;
    let client = HttpToncenterClient::new(ToncenterConfig::new(base_url)).unwrap();

    let req =
        EmulateMessageRequest::new("te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo").with_metadata(true);
    let res = client.emulate_message(&req).await.unwrap();

    // Verify HTTP endpoint and request JSON payload.
    let recorded = captured.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0]
            .0
            .starts_with(&format!("POST {EMULATE_TRACE_ENDPOINT}"))
    );
    let req_json: Value = serde_json::from_str(&recorded[0].1).unwrap();
    assert_eq!(req_json["boc"], "te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo");
    assert_eq!(req_json["ignore_chksig"], true);
    assert_eq!(req_json["include_metadata"], true);
    assert_eq!(req_json["with_actions"], true);

    // Verify aggregate fee totals.
    assert!(res.trace_succeeded);
    assert!(!res.has_warnings());
    assert_eq!(res.mc_block_seqno, Some(88888));
    assert_eq!(res.root_fee_nano, 4_100_000);
    assert_eq!(res.total_fees_nano, 9_000_000);
    assert_eq!(res.total_gas_fees_nano, 7_150_000);
    assert_eq!(res.total_storage_fees_nano, 200_000);
    assert_eq!(res.total_fwd_fees_nano, 1_650_000);

    // Verify per-account TON, Jetton, and NFT deltas (deterministic order).
    let wallet_delta = res.delta_for(ACC_WALLET).unwrap();
    assert_eq!(wallet_delta.ton_delta_nano, -1_104_100_000);
    assert_eq!(wallet_delta.jetton_deltas.len(), 1);
    assert_eq!(wallet_delta.jetton_deltas[0].delta, -12_500_000);
    assert_eq!(
        wallet_delta.jetton_deltas[0].symbol.as_deref(),
        Some("USDt")
    );
    assert_eq!(wallet_delta.jetton_deltas[0].decimals, Some(6));
    assert_eq!(wallet_delta.nft_deltas.len(), 1);
    assert_eq!(wallet_delta.nft_deltas[0].nft_address, ACC_NFT_ITEM);
    assert_eq!(
        wallet_delta.nft_deltas[0].collection_address.as_deref(),
        Some(ACC_NFT_COLLECTION)
    );
    assert_eq!(wallet_delta.nft_deltas[0].delta, -1);

    let recip_delta = res.delta_for(ACC_RECIPIENT).unwrap();
    assert_eq!(recip_delta.ton_delta_nano, 999_100_000);
    assert_eq!(recip_delta.jetton_deltas[0].delta, 12_500_000);
    assert_eq!(recip_delta.nft_deltas[0].delta, 1);

    // Deterministic re-parse yields byte-for-byte identical result.
    let res2 = client.emulate_message(&req).await.unwrap();
    assert_eq!(res, res2);
}

#[tokio::test]
async fn emulate_message_detects_aborted_failed_phases_bounced_and_incomplete_warnings() {
    let canned = json!({
        "mc_block_seqno": 90001,
        "is_incomplete": true,
        "trace": {
            "tx_hash": "tx_root",
            "children": [
                {
                    "tx_hash": "tx_child_aborted",
                    "children": [{ "tx_hash": "tx_bounce_back", "children": [] }]
                }
            ]
        },
        "transactions_order": ["tx_root", "tx_child_aborted", "tx_bounce_back"],
        "transactions": {
            "tx_root": {
                "account": ACC_WALLET,
                "total_fees": "3000000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "compute_ph": { "success": true, "exit_code": 0, "gas_fees": "2000000" },
                    "action": { "success": true, "result_code": 0, "total_fwd_fees": "1000000" }
                },
                "account_state_before": { "balance": "5000000000" },
                "account_state_after": { "balance": "4497000000" },
                "out_msgs": []
            },
            "tx_child_aborted": {
                "account": ACC_RECIPIENT,
                "total_fees": "2000000",
                "description": {
                    "type": "ord",
                    "aborted": true,
                    "compute_ph": { "success": false, "exit_code": 33, "gas_fees": "2000000" },
                    "action": { "success": false, "result_code": 34, "total_fwd_fees": "0" }
                },
                "out_msgs": [{
                    "source": ACC_RECIPIENT,
                    "destination": ACC_WALLET,
                    "value": "495000000",
                    "bounced": true
                }]
            },
            "tx_bounce_back": {
                "account": ACC_WALLET,
                "total_fees": "500000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "compute_ph": { "skipped": true },
                    "action": { "success": true, "result_code": 0, "total_fwd_fees": "0" }
                },
                "in_msg": {
                    "source": ACC_RECIPIENT,
                    "destination": ACC_WALLET,
                    "value": "495000000",
                    "bounced": true
                },
                "account_state_before": { "balance": "4497000000" },
                "account_state_after": { "balance": "4991500000" },
                "out_msgs": []
            }
        },
        "actions": [
            {
                "type": "call_contract",
                "success": false,
                "action_id": "act-1",
                "details": {
                    "source": ACC_WALLET,
                    "destination": ACC_RECIPIENT,
                    "value": "500000000"
                }
            }
        ]
    })
    .to_string();

    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock_emulate_server(canned, captured).await;
    let client = HttpToncenterClient::new(ToncenterConfig::new(base_url)).unwrap();
    let res = client
        .emulate_message(&EmulateMessageRequest::new("AAAA"))
        .await
        .unwrap();

    assert!(!res.trace_succeeded);
    assert!(res.is_incomplete);
    assert_eq!(res.total_fees_nano, 5_500_000);
    // Wallet started at 5_000_000_000 and ended at 4_991_500_000 after bounce -> net -8_500_000.
    assert_eq!(
        res.delta_for(ACC_WALLET).unwrap().ton_delta_nano,
        -8_500_000
    );

    // Must contain Aborted, ComputePhaseFailed(33), ActionPhaseFailed(34), MessageBounced, ActionFailed, TraceIncomplete.
    assert!(
        res.warnings
            .contains(&EmulationWarning::TransactionAborted {
                tx_hash: "tx_child_aborted".to_owned(),
                account: ACC_RECIPIENT.to_owned(),
            })
    );
    assert!(
        res.warnings
            .contains(&EmulationWarning::ComputePhaseFailed {
                tx_hash: "tx_child_aborted".to_owned(),
                account: ACC_RECIPIENT.to_owned(),
                exit_code: 33,
            })
    );
    assert!(res.warnings.contains(&EmulationWarning::ActionPhaseFailed {
        tx_hash: "tx_child_aborted".to_owned(),
        account: ACC_RECIPIENT.to_owned(),
        result_code: 34,
    }));
    assert!(res.warnings.iter().any(|w| matches!(
        w,
        EmulationWarning::MessageBounced {
            value_nano: 495_000_000,
            ..
        }
    )));
    assert!(res.warnings.contains(&EmulationWarning::ActionFailed {
        action_type: "call_contract".to_owned(),
        trace_id: Some("act-1".to_owned()),
    }));
    assert!(res.warnings.contains(&EmulationWarning::TraceIncomplete));

    // TON Connect status maps failed non-skipped action to Failed.
    let tc_emul = res.to_ton_connect_emulation(ACC_WALLET, 500_000_000);
    assert_eq!(tc_emul.status, TonConnectEmulationStatus::Failed);
}

#[test]
fn emulate_fallback_parses_raw_jetton_transfer_boc_when_actions_omitted() {
    let dest = TonAddress::parse(ACC_RECIPIENT).unwrap().address;
    let resp = TonAddress::parse(ACC_WALLET).unwrap().address;
    let jw = TonAddress::parse(ACC_JETTON_WALLET_A).unwrap().address;
    let jetton_body = build_jetton_transfer_body(&JettonTransferParams {
        jetton_wallet: jw,
        destination: dest,
        response_destination: resp,
        jetton_amount: 7_500_000,
        query_id: 9,
        attached_ton_nano: 50_000_000,
        forward_ton_nano: 1,
        forward_comment: Some("pay".to_owned()),
    })
    .unwrap();
    let jetton_body_b64 = serialize_boc_base64(&jetton_body).unwrap();

    let body = json!({
        "trace": { "tx_hash": "root", "children": [] },
        "transactions": {
            "root": {
                "account": ACC_WALLET,
                "total_fees": "4000000",
                "description": {
                    "type": "ord",
                    "aborted": false,
                    "compute_ph": { "success": true, "exit_code": 0 },
                    "action": { "success": true, "result_code": 0 }
                },
                "out_msgs": [{
                    "source": ACC_WALLET,
                    "destination": ACC_JETTON_WALLET_A,
                    "value": "50000000",
                    "bounced": false,
                    "message_content": { "body": jetton_body_b64 }
                }]
            }
        }
    });

    let res = parse_emulation_response(&serde_json::to_vec(&body).unwrap()).unwrap();
    assert!(res.trace_succeeded);
    let w = res.delta_for(ACC_WALLET).unwrap();
    assert_eq!(w.ton_delta_nano, -54_000_000);
    assert_eq!(w.jetton_deltas.len(), 1);
    assert_eq!(w.jetton_deltas[0].delta, -7_500_000);
    let r = res.delta_for(ACC_RECIPIENT).unwrap();
    assert_eq!(r.jetton_deltas.len(), 1);
    assert_eq!(r.jetton_deltas[0].delta, 7_500_000);
}

#[test]
fn gram_emulate_and_ton_connect_emulation_edge_cases() {
    // Rejects Toncenter error body (`api-04-emulateTrace.json`).
    let err_body =
        br#"{"error":"internal server error: Cannot POST \/api\/emulate\/v1\/emulateTrace"}"#;
    assert!(parse_emulated_trace(err_body).is_none());
    assert!(parse_emulation_response(err_body).is_err());

    // Port of `wallet_ton_connect_emulation.cpp` status edge cases:
    // 1. Aborted when trace failed and only skipped/succeeded actions exist.
    let aborted = parse_ton_connect_emulation(
        &SendEmulation {
            trace_succeeded: false,
            is_incomplete: false,
            actions: vec![SendEmulationAction {
                kind: "unknown".to_owned(),
                succeeded: false,
                details_json: "{}".to_owned(),
            }],
        },
        ACC_WALLET,
        100,
    );
    assert_eq!(aborted.status, TonConnectEmulationStatus::Aborted);

    // 2. Incomplete when trace succeeded with `is_incomplete = true`.
    let incomplete = parse_ton_connect_emulation(
        &SendEmulation {
            trace_succeeded: true,
            is_incomplete: true,
            actions: vec![SendEmulationAction {
                kind: "ton_transfer".to_owned(),
                succeeded: true,
                details_json: json!({
                    "source": ACC_RECIPIENT,
                    "destination": ACC_WALLET,
                    "value": "250"
                })
                .to_string(),
            }],
        },
        ACC_WALLET,
        100,
    );
    assert_eq!(incomplete.status, TonConnectEmulationStatus::Incomplete);
    assert_eq!(incomplete.net_nano, 150);
    assert_eq!(incomplete.actions[0].kind, TonConnectActionKind::Deposit);
    assert_eq!(incomplete.actions[0].side, TonConnectActionSide::Incoming);

    // 3. Signed negative numeric opcode (-718113061 == 0xd53276db as i32) maps to Excess.
    let excess_numeric = parse_ton_connect_emulation(
        &SendEmulation {
            trace_succeeded: true,
            is_incomplete: false,
            actions: vec![SendEmulationAction {
                kind: "call_contract".to_owned(),
                succeeded: true,
                details_json: json!({
                    "source": ACC_RECIPIENT,
                    "destination": ACC_WALLET,
                    "value": "50",
                    "opcode": -718_113_061_i64
                })
                .to_string(),
            }],
        },
        ACC_WALLET,
        100,
    );
    assert_eq!(excess_numeric.status, TonConnectEmulationStatus::Shown);
    assert_eq!(excess_numeric.actions[0].kind, TonConnectActionKind::Excess);
    assert_eq!(excess_numeric.net_nano, -50);

    // 4. Overflow in credited - request_total_nano downgrades Shown -> Empty.
    let overflow = parse_ton_connect_emulation(
        &SendEmulation {
            trace_succeeded: true,
            is_incomplete: false,
            actions: vec![SendEmulationAction {
                kind: "ton_transfer".to_owned(),
                succeeded: true,
                details_json: json!({
                    "source": ACC_RECIPIENT,
                    "destination": ACC_WALLET,
                    "value": i64::MAX.to_string()
                })
                .to_string(),
            }],
        },
        ACC_WALLET,
        -10,
    );
    assert_eq!(overflow.status, TonConnectEmulationStatus::Empty);
}

// ============================================================================
// Acceptance Criterion 3: Shamir Phrase Shares (`(k=2, n=3)` + corruption checks)
// ============================================================================

#[test]
fn shamir_phrase_shares_2_of_3_reconstruct_all_subsets_and_detect_corruption() {
    let words = generate_mnemonic("").unwrap();
    assert_eq!(words.len(), 24);
    assert!(validate_mnemonic(&words, ""));

    let shares = split_phrase(&words, 2, 3).unwrap();
    assert_eq!(shares.len(), 3);

    // Every 2-of-3 subset reconstructs the exact 24-word mnemonic.
    let subsets = [
        [shares[0].clone(), shares[1].clone()],
        [shares[0].clone(), shares[2].clone()],
        [shares[1].clone(), shares[2].clone()],
        [shares[2].clone(), shares[0].clone()],
    ];
    for subset in &subsets {
        let recovered = reconstruct_phrase(subset).unwrap();
        assert_eq!(recovered, words);
        assert!(validate_mnemonic(&recovered, ""));
    }

    // Reconstructing from only 1 share must fail.
    for single in &shares {
        assert!(reconstruct_phrase(std::slice::from_ref(single)).is_err());
    }

    // Duplicate share indices must be rejected.
    assert!(reconstruct_phrase(&[shares[0].clone(), shares[0].clone()]).is_err());

    // Corrupted payload bit is caught by share checksum (both in-memory and encoded string).
    let mut bad_payload = shares[1].clone();
    bad_payload.data[0] ^= 0x80;
    assert!(!bad_payload.verify_checksum());
    assert!(reconstruct_phrase(&[shares[0].clone(), bad_payload]).is_err());

    // Corrupted serialized token string fails decode.
    let encoded = shares[2].encode_string().unwrap();
    let mut bad_bytes = shares[2].to_bytes().unwrap();
    let last = bad_bytes.len() - 1;
    bad_bytes[last] ^= 0xFF;
    assert!(PhraseShare::from_bytes(&bad_bytes).is_err());
    assert_eq!(PhraseShare::decode_string(&encoded).unwrap(), shares[2]);

    // Forged checksum (attacker recomputes outer checksum after corrupting data) is still caught
    // by the inner secret integrity checksum!
    let mut forged = shares[1].clone();
    forged.data[5] ^= 0x42;
    forged.checksum = PhraseShare::compute_checksum(
        forged.version,
        forged.index,
        forged.threshold,
        forged.total_shares,
        &forged.data,
    );
    assert!(forged.verify_checksum());
    assert!(reconstruct_phrase(&[shares[0].clone(), forged]).is_err());

    // Mixing shares from two independent splits is caught by inner secret checksum.
    let other_words = generate_mnemonic("").unwrap();
    let other_shares = split_phrase(&other_words, 2, 3).unwrap();
    assert!(reconstruct_phrase(&[shares[0].clone(), other_shares[1].clone()]).is_err());
}

#[test]
fn shamir_3_of_5_and_tdesktop_xor_holder_shares() {
    let words = generate_mnemonic("").unwrap();
    let shares = split_phrase(&words, 3, 5).unwrap();
    assert_eq!(shares.len(), 5);

    // 2 shares fail for k=3.
    assert!(reconstruct_phrase(&shares[..2]).is_err());

    // Several 3-of-5 subsets succeed.
    assert_eq!(
        reconstruct_phrase(&[shares[0].clone(), shares[2].clone(), shares[4].clone()]).unwrap(),
        words
    );
    assert_eq!(
        reconstruct_phrase(&[shares[1].clone(), shares[3].clone(), shares[4].clone()]).unwrap(),
        words
    );

    // Arbitrary byte secret split/reconstruct.
    let raw_secret = b"throwaway-secret-material-for-wp43-test";
    let raw_shares = split_secret(raw_secret, 3, 4).unwrap();
    assert_eq!(
        reconstruct_secret(&[
            raw_shares[0].clone(),
            raw_shares[2].clone(),
            raw_shares[3].clone()
        ])
        .unwrap()
        .as_slice(),
        raw_secret
    );

    // tdesktop 215-byte seed padding + XOR shares + TL decryptedKeyPart + NaCl holder sealing.
    let seed = seed_from_words(&words).unwrap();
    assert_eq!(seed.len(), PHRASE_SEED_LENGTH);
    assert_eq!(words_from_seed(&seed).unwrap(), words);

    let xor_parts = split_seed_xor(&seed, 3).unwrap();
    assert_eq!(combine_shares_xor(&xor_parts).unwrap().as_slice(), &*seed);

    let tl = serialize_decrypted_key_part(&xor_parts[1]);
    assert_eq!(parse_decrypted_key_part(&tl).unwrap(), xor_parts[1]);

    // Wrong TL constructor rejected.
    let mut bad_tl = tl.clone();
    bad_tl[0] ^= 0xFF;
    assert!(parse_decrypted_key_part(&bad_tl).is_err());

    // Holder sealing + tamper rejection.
    let holder = TonConnectKeypair::generate();
    let sealed = encrypt_share_for_holder(&holder.public_key_bytes(), &xor_parts[2]).unwrap();
    let opened = decrypt_share_for_holder(holder.secret_key_bytes(), &sealed).unwrap();
    assert_eq!(opened, xor_parts[2]);

    let mut tampered = sealed;
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert!(decrypt_share_for_holder(holder.secret_key_bytes(), &tampered).is_err());
}
