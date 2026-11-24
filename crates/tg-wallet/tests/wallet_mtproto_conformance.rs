//! Byte-level tdesktop conformance tests for Layer-233 MTProto TON Wallet RPCs
//! (`26 wallet.*`, `2 toncenter.*`, and `3 payments.*` methods) against an in-process
//! scripted fake Telegram MTProto 2.0 server.
//!
//! Reference client: Telegram Desktop (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Money & hygiene guarantees:
//! - 100% offline and deterministic: every MTProto connection runs over an in-process
//!   `TransportFactory` (`FakeConnection`), and HTTP transport override tests run only
//!   against a loopback `127.0.0.1:0` fake TCP listener.
//! - Uses throwaway keys/mnemonics generated solely inside tests and obviously fake
//!   addresses/credentials. Never contacts Telegram, toncenter, or the TON network.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use crypto_box::SecretKey as BoxSecretKey;
use rand_core::OsRng;
use serde_json::json;
use tg_api::{InvocationError, RpcClient, RpcError};
use tg_crypto::dh::{GOOD_PRIME, dh_new_nonce_hash, dh_server_aes_key_iv, mod_exp};
use tg_crypto::rsa::PublicKey;
use tg_crypto::{AuthKey, Side, aes_ige_decrypt, aes_ige_encrypt, sha1, sha256};
use tg_mtproto::dc_options::{DcId, Environment};
use tg_mtproto::message::{
    build_encrypted_packet, encode_unencrypted_packet, parse_encrypted_packet, unpack_container,
    unwrap_gzip_layers,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tg_mtproto::{
    Client, ClientConfig, FakeConnection, FakeServerPeer, InvokeOptions, PacketConnection,
    TransportFactory,
};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_functions,
    types as mtp_types,
};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Boxed, Cursor, Deserializable, Identifiable, RawObject,
    Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tg_wallet::WalletAppConfig;
use tg_wallet::address::TonAddress;
use tg_wallet::boc::serialize_boc;
use tg_wallet::jettons::{NftTransferParams, build_nft_transfer_message};
use tg_wallet::mnemonic::generate_mnemonic;
use tg_wallet::mtproto_api::{
    CardStickerCoordinates, CardStickerInput, DepositStarGiftRequest, DisableBackupRequest,
    EMULATED_TRACES_NOT_FOUND, FiatRateCache, GiftMoveTarget, OwnershipProofInput,
    SendTransferRequest, ServerTransactionKind, ServerTransactionPeer, ServerWalletState,
    TonConnectClaimRequest, TonConnectPendingQuery, TonConnectSubmitConnectResultRequest,
    TonConnectSubmitOutcome, TonConnectSubmitResponseRequest, TransactionDirectionFilter,
    TransferRefusalKind, TransferSubmissionError, WalletMtprotoClient, WalletMtprotoError,
    WalletReplacement, WalletServerUpdate, default_gasless_terms, extract_routed_send_boc,
    extract_submitted_lookup_tx_id, is_empty_pending_traces_error, is_send_boc_json_rpc,
    parse_wallet_update, select_toncenter_api, select_toncenter_transport, toncenter_shifted_dc_id,
    wallet_share_shifted_dc_id,
};
use tg_wallet::phrase_shares::{decrypt_share_for_holder, encrypt_share_for_holder};
use tg_wallet::toncenter::{
    AccountState, GramHttpRequest, NftKind, ToncenterApi, ToncenterTransportKind,
    ToncenterTransportPolicy,
};
use tg_wallet::tonconnect::TonConnectSessionStatus;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ============================================================================
// Ephemeral test-only RSA keypair & byte helpers (identical to tg-session suite)
// ============================================================================

const TEST_RSA_N_HEX: &str = "\
9000000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000000\
000c000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000535\
8000000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000000\
40d0000000000000000000000000000000000000000000000000000000000000\
00000000000000000000000000000000000000000000000000000000000bb9a1";

const TEST_RSA_D_HEX: &str = "\
6a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec\
0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec0a13f5ec\
0a1ccceb3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb\
3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb3314cceb3314d0c0\
6bb794486bb794486bb794486bb794486bb794486bb794486bb794486bb79448\
6bb794486bb794486bb794486bb794486bb794486bb794486bb794486bb79448\
9b6a5515aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515\
aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515aaea5515aaf2f321";

fn hex_decode(hex: &str) -> Vec<u8> {
    hex::decode(hex).expect("valid hex")
}

fn test_rsa_public_key() -> PublicKey {
    let n = hex_decode(TEST_RSA_N_HEX);
    let e = vec![0x01, 0x00, 0x01];
    PublicKey::from_components(&n, &e).expect("valid test RSA public key")
}

fn pad_be_256(raw: &[u8]) -> [u8; 256] {
    let mut out = [0u8; 256];
    let len = raw.len().min(256);
    out[256 - len..].copy_from_slice(&raw[raw.len() - len..]);
    out
}

fn now_unix_secs() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i32
}

/// Manual TL byte writer for independent wire-level byte assertions (does not call `tg_tl`'s
/// generated serializer on the function under test, catching flag-bit or field-order regressions).
#[derive(Default)]
struct TlWireBuilder {
    buf: Vec<u8>,
}

impl TlWireBuilder {
    fn ctor(id: u32) -> Self {
        let mut s = Self::default();
        s.u32(id);
        s
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn f64(&mut self, v: f64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn bytes(&mut self, data: &[u8]) -> &mut Self {
        let len = data.len();
        let header_len = if len < 254 {
            self.buf.push(len as u8);
            1
        } else {
            self.buf.push(254);
            self.buf.push((len & 0xFF) as u8);
            self.buf.push(((len >> 8) & 0xFF) as u8);
            self.buf.push(((len >> 16) & 0xFF) as u8);
            4
        };
        self.buf.extend_from_slice(data);
        let pad = (4 - ((header_len + len) % 4)) % 4;
        self.buf.resize(self.buf.len() + pad, 0);
        self
    }

    fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    fn raw(&mut self, data: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(data);
        self
    }

    fn vec_header(&mut self, count: usize) -> &mut Self {
        self.u32(0x1cb5_c415);
        self.i32(count as i32)
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

// ============================================================================
// In-Process Byte-Level Scripted Fake MTProto 2.0 Server
// ============================================================================

#[derive(Debug, Clone)]
struct RpcRecord {
    dc_id: DcId,
    session_id: u64,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct IncomingRpc {
    dc_id: DcId,
    ctor: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
enum ServerAction {
    Reply(Vec<u8>),
    RpcError(i32, String),
}

impl ServerAction {
    fn ok<T: Serializable>(val: &T) -> Self {
        Self::Reply(val.to_bytes())
    }

    fn ok_bool(v: bool) -> Self {
        let id = if v { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        Self::Reply(id.to_le_bytes().to_vec())
    }

    fn err(code: i32, msg: impl Into<String>) -> Self {
        Self::RpcError(code, msg.into())
    }
}

type RpcHandler = Arc<dyn Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync>;

#[derive(Debug, Clone)]
struct ServerKeyEntry {
    key: AuthKey,
    server_salt: u64,
}

struct ScriptedServerInner {
    rsa_pub: PublicKey,
    rsa_n: Vec<u8>,
    rsa_d: Vec<u8>,
    next_conn_id: AtomicU64,
    dh_counter: AtomicU64,
    keys: Mutex<HashMap<u64, ServerKeyEntry>>,
    rpcs: Mutex<Vec<RpcRecord>>,
    handler: Mutex<RpcHandler>,
}

#[derive(Clone)]
struct ScriptedServer {
    inner: Arc<ScriptedServerInner>,
}

impl ScriptedServer {
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(ScriptedServerInner {
                rsa_pub: test_rsa_public_key(),
                rsa_n: hex_decode(TEST_RSA_N_HEX),
                rsa_d: hex_decode(TEST_RSA_D_HEX),
                next_conn_id: AtomicU64::new(1),
                dh_counter: AtomicU64::new(1),
                keys: Mutex::new(HashMap::new()),
                rpcs: Mutex::new(Vec::new()),
                handler: Mutex::new(Arc::new(handler)),
            }),
        }
    }

    fn set_handler<F>(&self, handler: F)
    where
        F: Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync + 'static,
    {
        *self.inner.handler.lock().unwrap() = Arc::new(handler);
    }

    fn rpcs(&self) -> Vec<RpcRecord> {
        self.inner.rpcs.lock().unwrap().clone()
    }

    fn clear_rpcs(&self) {
        self.inner.rpcs.lock().unwrap().clear();
    }

    fn last_rpc(&self) -> RpcRecord {
        self.inner
            .rpcs
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("at least one RPC recorded")
    }

    fn register_rsa_key_on(&self, client: &Client) {
        client
            .dc_options()
            .add_public_key(self.inner.rsa_pub.clone());
    }

    fn decrypt_rsa_pad(&self, encrypted_data: &[u8]) -> Vec<u8> {
        let dec_raw = mod_exp(encrypted_data, &self.inner.rsa_d, &self.inner.rsa_n)
            .expect("RSA private modexp");
        let block = pad_be_256(&dec_raw);
        let temp_key_xor = &block[0..32];
        let aes_encrypted = &block[32..256];
        let aes_hash = sha256(&[aes_encrypted]);
        let mut temp_key = [0u8; 32];
        for i in 0..32 {
            temp_key[i] = temp_key_xor[i] ^ aes_hash[i];
        }
        let data_with_hash = aes_ige_decrypt(aes_encrypted, &temp_key, &[0u8; 32]);
        assert_eq!(data_with_hash.len(), 224);
        let mut data_with_padding = data_with_hash[0..192].to_vec();
        data_with_padding.reverse();
        let expected_hash = sha256(&[&temp_key, &data_with_padding]);
        assert_eq!(&data_with_hash[192..224], &expected_hash);
        data_with_padding
    }

    fn spawn_connection(&self, conn_id: u64, protocol_dc_id: i16, mut peer: FakeServerPeer) {
        let server = self.clone();
        let bare_dc = protocol_dc_id.unsigned_abs() as i32;
        let dc_id = if bare_dc >= 10_000 {
            bare_dc - 10_000
        } else {
            bare_dc
        };

        tokio::spawn(async move {
            struct DhPending {
                nonce: [u8; 16],
                server_nonce: [u8; 16],
                new_nonce: [u8; 32],
                a_srv: [u8; 256],
                aes_key: [u8; 32],
                aes_iv: [u8; 32],
            }

            let mut dh_pending: Option<DhPending> = None;
            let mut server_msg_counter: u64 = 1;
            let mut server_seq_counter: u32 = 0;
            let mut sent_new_session = false;

            let next_server_msg_id = |counter: &mut u64, is_reply: bool| -> u64 {
                let secs = now_unix_secs().max(1_700_000_000) as u64;
                let low_bits = if is_reply { 1 } else { 3 };
                let id = (secs << 32) | ((*counter << 2) | low_bits);
                *counter += 1;
                id
            };

            let send_encrypted = |peer: &FakeServerPeer,
                                  key: &AuthKey,
                                  salt: u64,
                                  session_id: u64,
                                  msg_id: u64,
                                  content_related: bool,
                                  seq_counter: &mut u32,
                                  body: &[u8]| {
                let seq_no = if content_related {
                    let s = *seq_counter * 2 + 1;
                    *seq_counter += 1;
                    s
                } else {
                    *seq_counter * 2
                };
                let pkt = build_encrypted_packet(
                    salt,
                    session_id,
                    msg_id,
                    seq_no,
                    body,
                    key,
                    Side::Server,
                );
                let _ = peer.tx.send(Ok(pkt));
            };

            while let Some(raw) = peer.rx.recv().await {
                if raw.len() < 8 {
                    break;
                }
                let auth_key_id = u64::from_le_bytes(raw[0..8].try_into().unwrap());
                if auth_key_id == 0 {
                    let msg_len = u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
                    let body = &raw[20..20 + msg_len];
                    let ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    if ctor == mtp_functions::ReqPqMulti::CONSTRUCTOR_ID {
                        let req = mtp_functions::ReqPqMulti::from_bytes(body).unwrap();
                        let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                        let mut server_nonce = [0u8; 16];
                        server_nonce[0..8].copy_from_slice(&seq.to_le_bytes());
                        server_nonce[8..16]
                            .copy_from_slice(&0xA55A_C33C_1234_5678u64.to_le_bytes());
                        let res_pq = mtp_enums::ResPQ::ResPQ(mtp_types::ResPQ {
                            nonce: req.nonce,
                            server_nonce,
                            pq: vec![0x01, 0x87],
                            server_public_key_fingerprints: vec![
                                server.inner.rsa_pub.fingerprint() as i64,
                            ],
                        });
                        let out_id = next_server_msg_id(&mut server_msg_counter, true);
                        let _ = peer
                            .tx
                            .send(Ok(encode_unencrypted_packet(out_id, &res_pq.to_bytes())));
                    } else if ctor == mtp_functions::ReqDHParams::CONSTRUCTOR_ID {
                        let req = mtp_functions::ReqDHParams::from_bytes(body).unwrap();
                        let plain = server.decrypt_rsa_pad(&req.encrypted_data);
                        let mut cur = Cursor::new(&plain);
                        let pq_inner = mtp_enums::PQInnerData::deserialize(&mut cur).unwrap();
                        let new_nonce = match pq_inner {
                            mtp_enums::PQInnerData::Dc(d) => d.new_nonce,
                            mtp_enums::PQInnerData::TempDc(d) => d.new_nonce,
                            other => panic!("unexpected PQInnerData: {other:?}"),
                        };
                        let seq = server.inner.dh_counter.fetch_add(1, Ordering::Relaxed);
                        let mut a_srv = [0x42u8; 256];
                        a_srv[0..8].copy_from_slice(&seq.to_be_bytes());
                        a_srv[248..256].copy_from_slice(&(seq ^ 0x55AA_55AA).to_be_bytes());
                        let g_a =
                            pad_be_256(&mod_exp(&[3], &a_srv, &GOOD_PRIME).expect("g^a mod p"));
                        let inner = mtp_enums::ServerDHInnerData::ServerDHInnerData(
                            mtp_types::ServerDHInnerData {
                                nonce: req.nonce,
                                server_nonce: req.server_nonce,
                                g: 3,
                                dh_prime: GOOD_PRIME.to_vec(),
                                g_a: g_a.to_vec(),
                                server_time: now_unix_secs(),
                            },
                        )
                        .to_bytes();
                        let digest = sha1(&[&inner]);
                        let mut answer_with_hash = Vec::with_capacity(20 + inner.len() + 16);
                        answer_with_hash.extend_from_slice(&digest);
                        answer_with_hash.extend_from_slice(&inner);
                        while answer_with_hash.len() % 16 != 0 {
                            answer_with_hash.push(0);
                        }
                        let (aes_key, aes_iv) = dh_server_aes_key_iv(&new_nonce, &req.server_nonce);
                        let encrypted_answer =
                            aes_ige_encrypt(&answer_with_hash, &aes_key, &aes_iv);
                        let resp = mtp_enums::ServerDHParams::Ok(mtp_types::ServerDHParamsOk {
                            nonce: req.nonce,
                            server_nonce: req.server_nonce,
                            encrypted_answer,
                        });
                        dh_pending = Some(DhPending {
                            nonce: req.nonce,
                            server_nonce: req.server_nonce,
                            new_nonce,
                            a_srv,
                            aes_key,
                            aes_iv,
                        });
                        let out_id = next_server_msg_id(&mut server_msg_counter, true);
                        let _ = peer
                            .tx
                            .send(Ok(encode_unencrypted_packet(out_id, &resp.to_bytes())));
                    } else if ctor == mtp_functions::SetClientDHParams::CONSTRUCTOR_ID {
                        let req = mtp_functions::SetClientDHParams::from_bytes(body).unwrap();
                        let pending = dh_pending.take().unwrap();
                        let dec =
                            aes_ige_decrypt(&req.encrypted_data, &pending.aes_key, &pending.aes_iv);
                        let mut dec_cur = Cursor::new(&dec[20..]);
                        let mtp_enums::ClientDHInnerData::ClientDHInnerData(cdh) =
                            mtp_enums::ClientDHInnerData::deserialize(&mut dec_cur).unwrap();
                        let shared = pad_be_256(
                            &mod_exp(&cdh.g_b, &pending.a_srv, &GOOD_PRIME).expect("g_b^a mod p"),
                        );
                        let auth_key = AuthKey::new(shared);
                        let nn_lo = u64::from_le_bytes(pending.new_nonce[0..8].try_into().unwrap());
                        let sn_lo =
                            u64::from_le_bytes(pending.server_nonce[0..8].try_into().unwrap());
                        let server_salt = nn_lo ^ sn_lo;
                        let nh1 = dh_new_nonce_hash(&pending.new_nonce, 1, &auth_key.aux_hash());
                        server.inner.keys.lock().unwrap().insert(
                            auth_key.key_id_u64(),
                            ServerKeyEntry {
                                key: auth_key,
                                server_salt,
                            },
                        );
                        let ok = mtp_enums::SetClientDHParamsAnswer::DhGenOk(mtp_types::DhGenOk {
                            nonce: pending.nonce,
                            server_nonce: pending.server_nonce,
                            new_nonce_hash1: nh1,
                        });
                        let out_id = next_server_msg_id(&mut server_msg_counter, true);
                        let _ = peer
                            .tx
                            .send(Ok(encode_unencrypted_packet(out_id, &ok.to_bytes())));
                    } else {
                        panic!("unexpected unencrypted constructor: 0x{ctor:08x}");
                    }
                    continue;
                }

                let key_entry = server.inner.keys.lock().unwrap().get(&auth_key_id).cloned();
                let Some(entry) = key_entry else {
                    let _ = peer.tx.send(Err(TransportError::ServerError(
                        TransportErrorCode::AuthKeyNotFound,
                    )));
                    break;
                };
                let dec = parse_encrypted_packet(&raw, &entry.key, Side::Client)
                    .expect("decrypt MTProto 2.0 packet");
                let session_id = dec.session_id;

                let inner_msgs: Vec<(u64, Vec<u8>)> = if dec.body.len() >= 4
                    && u32::from_le_bytes(dec.body[0..4].try_into().unwrap())
                        == MsgContainer::CONSTRUCTOR_ID
                {
                    unpack_container(&dec.body)
                        .unwrap()
                        .into_iter()
                        .map(|m| (m.msg_id as u64, m.body.0))
                        .collect()
                } else {
                    vec![(dec.msg_id, dec.body.clone())]
                };

                for (msg_id, raw_msg) in inner_msgs {
                    let mut budget = 1024 * 1024;
                    let body = if raw_msg.len() >= 4
                        && u32::from_le_bytes(raw_msg[0..4].try_into().unwrap())
                            == GzipPacked::CONSTRUCTOR_ID
                    {
                        unwrap_gzip_layers(&raw_msg, &mut budget, 0).unwrap()
                    } else {
                        raw_msg
                    };
                    if body.len() < 4 {
                        continue;
                    }
                    let outer_ctor = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    if outer_ctor == mtp_functions::Ping::CONSTRUCTOR_ID {
                        let ping = mtp_functions::Ping::from_bytes(&body).unwrap();
                        if !sent_new_session {
                            sent_new_session = true;
                            let ns = mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
                                first_msg_id: msg_id as i64,
                                unique_id: conn_id as i64,
                                server_salt: entry.server_salt as i64,
                            });
                            let ns_id = next_server_msg_id(&mut server_msg_counter, false);
                            send_encrypted(
                                &peer,
                                &entry.key,
                                entry.server_salt,
                                session_id,
                                ns_id,
                                true,
                                &mut server_seq_counter,
                                &ns.to_bytes(),
                            );
                        }
                        let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                            msg_id: msg_id as i64,
                            ping_id: ping.ping_id,
                        });
                        let p_id = next_server_msg_id(&mut server_msg_counter, true);
                        send_encrypted(
                            &peer,
                            &entry.key,
                            entry.server_salt,
                            session_id,
                            p_id,
                            false,
                            &mut server_seq_counter,
                            &pong.to_bytes(),
                        );
                        continue;
                    }
                    if outer_ctor == mtp_functions::PingDelayDisconnect::CONSTRUCTOR_ID {
                        let ping = mtp_functions::PingDelayDisconnect::from_bytes(&body).unwrap();
                        let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
                            msg_id: msg_id as i64,
                            ping_id: ping.ping_id,
                        });
                        let p_id = next_server_msg_id(&mut server_msg_counter, true);
                        send_encrypted(
                            &peer,
                            &entry.key,
                            entry.server_salt,
                            session_id,
                            p_id,
                            false,
                            &mut server_seq_counter,
                            &pong.to_bytes(),
                        );
                        continue;
                    }
                    if outer_ctor == mtp_types::MsgsAck::CONSTRUCTOR_ID
                        || outer_ctor == mtp_functions::GetFutureSalts::CONSTRUCTOR_ID
                    {
                        if outer_ctor == mtp_functions::GetFutureSalts::CONSTRUCTOR_ID {
                            let now = now_unix_secs();
                            let salts =
                                mtp_enums::FutureSalts::FutureSalts(mtp_types::FutureSalts {
                                    req_msg_id: msg_id as i64,
                                    now,
                                    salts: vec![mtp_types::FutureSalt {
                                        valid_since: now - 60,
                                        valid_until: now + 3600,
                                        salt: entry.server_salt as i64,
                                    }],
                                });
                            let s_id = next_server_msg_id(&mut server_msg_counter, true);
                            send_encrypted(
                                &peer,
                                &entry.key,
                                entry.server_salt,
                                session_id,
                                s_id,
                                false,
                                &mut server_seq_counter,
                                &salts.to_bytes(),
                            );
                        }
                        continue;
                    }

                    let inner_body = unwrap_init_connection(&body);
                    let inner_ctor = u32::from_le_bytes(inner_body[0..4].try_into().unwrap());
                    server.inner.rpcs.lock().unwrap().push(RpcRecord {
                        dc_id,
                        session_id,
                        ctor: inner_ctor,
                        body: inner_body.clone(),
                    });

                    let handler = server.inner.handler.lock().unwrap().clone();
                    let action = handler(
                        &server,
                        IncomingRpc {
                            dc_id,
                            ctor: inner_ctor,
                            body: inner_body,
                        },
                    );
                    let payload = match action {
                        ServerAction::Reply(p) => p,
                        ServerAction::RpcError(code, message) => {
                            mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                error_code: code,
                                error_message: message,
                            })
                            .to_bytes()
                        }
                    };
                    let rpc_res = Boxed(RpcResult {
                        req_msg_id: msg_id as i64,
                        result: RawObject(payload),
                    })
                    .to_bytes();
                    let r_id = next_server_msg_id(&mut server_msg_counter, true);
                    send_encrypted(
                        &peer,
                        &entry.key,
                        entry.server_salt,
                        session_id,
                        r_id,
                        true,
                        &mut server_seq_counter,
                        &rpc_res,
                    );
                }
            }
        });
    }
}

fn unwrap_init_connection(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 4 {
        return raw.to_vec();
    }
    let ctor = u32::from_le_bytes(raw[0..4].try_into().unwrap());
    if ctor != tl_fns::InvokeWithLayer::<RawObject>::CONSTRUCTOR_ID {
        return raw.to_vec();
    }
    let mut cur = Cursor::new(&raw[4..]);
    let _layer = i32::deserialize(&mut cur).expect("invokeWithLayer.layer");
    let rest = &raw[raw.len() - cur.remaining()..];
    let inner_ctor = u32::from_le_bytes(rest[0..4].try_into().unwrap());
    assert_eq!(
        inner_ctor,
        tl_fns::InitConnection::<RawObject>::CONSTRUCTOR_ID
    );
    let mut icur = Cursor::new(&rest[4..]);
    let flags = i32::deserialize(&mut icur).unwrap() as u32;
    let _api_id = i32::deserialize(&mut icur).unwrap();
    let _device_model = String::deserialize(&mut icur).unwrap();
    let _system_version = String::deserialize(&mut icur).unwrap();
    let _app_version = String::deserialize(&mut icur).unwrap();
    let _system_lang_code = String::deserialize(&mut icur).unwrap();
    let _lang_pack = String::deserialize(&mut icur).unwrap();
    let _lang_code = String::deserialize(&mut icur).unwrap();
    if (flags & (1 << 0)) != 0 {
        let _ = tl_enums::InputClientProxy::deserialize(&mut icur).unwrap();
    }
    if (flags & (1 << 1)) != 0 {
        let _ = tl_enums::JSONValue::deserialize(&mut icur).unwrap();
    }
    rest[rest.len() - icur.remaining()..].to_vec()
}

impl TransportFactory for ScriptedServer {
    fn connect<'a>(
        &'a self,
        _ip: &'a str,
        _port: u16,
        protocol_dc_id: i16,
        _secret: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn PacketConnection>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let conn_id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
            let (conn, peer) = FakeConnection::pair();
            self.spawn_connection(conn_id, protocol_dc_id, peer);
            Ok(Box::new(conn) as Box<dyn PacketConnection>)
        })
    }
}

#[derive(Clone)]
struct MtprotoRpcAdapter(Client);

fn map_mtproto_err(err: tg_mtproto::InvocationError) -> InvocationError {
    match err {
        tg_mtproto::InvocationError::Rpc(rpc) => {
            InvocationError::Rpc(RpcError::new(rpc.code, rpc.name))
        }
        tg_mtproto::InvocationError::Dropped => InvocationError::Dropped,
        tg_mtproto::InvocationError::Transport(s) => InvocationError::Transport(s.to_string()),
        tg_mtproto::InvocationError::Deserialize(s) => InvocationError::Deserialize(s.to_string()),
        other => InvocationError::Transport(other.to_string()),
    }
}

impl RpcClient for MtprotoRpcAdapter {
    type Error = InvocationError;

    async fn invoke<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.0.invoke(req).await.map_err(map_mtproto_err)
    }

    async fn invoke_in_dc<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.0
            .invoke_in_dc(dc_id, req)
            .await
            .map_err(map_mtproto_err)
    }

    async fn invoke_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.0
            .invoke_with_options(
                req,
                InvokeOptions {
                    handle_flood_errors: true,
                    ..InvokeOptions::default()
                },
            )
            .await
            .map_err(map_mtproto_err)
    }

    async fn invoke_in_dc_handle_flood<R: tg_tl::RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.0
            .invoke_with_options(
                req,
                InvokeOptions {
                    dc_id,
                    handle_flood_errors: true,
                    ..InvokeOptions::default()
                },
            )
            .await
            .map_err(map_mtproto_err)
    }
}

fn make_wallet_client<F>(
    webfile_dc_id: DcId,
    handler: F,
) -> (ScriptedServer, WalletMtprotoClient<MtprotoRpcAdapter>)
where
    F: Fn(&ScriptedServer, IncomingRpc) -> ServerAction + Send + Sync + 'static,
{
    let server = ScriptedServer::new(handler);
    let cfg = ClientConfig {
        api_id: 1,
        device_model: "Desktop".to_owned(),
        system_version: "Linux".to_owned(),
        app_version: "6.0.0".to_owned(),
        system_lang_code: "en".to_owned(),
        lang_pack: "tdesktop".to_owned(),
        lang_code: "en".to_owned(),
        environment: Environment::Production,
        main_dc_id: 2,
        proxy: ProxyData::none(),
        use_pfs: false,
        ping_interval: Duration::from_secs(600),
        ..ClientConfig::default()
    };
    let (mtproto_client, _rx) = Client::with_transport(cfg, Arc::new(server.clone()));
    server.register_rsa_key_on(&mtproto_client);
    let rpc = MtprotoRpcAdapter(mtproto_client);
    let wallet = WalletMtprotoClient::new(rpc, webfile_dc_id);
    (server, wallet)
}

// ============================================================================
// Deterministic Synthetic TON Fixtures (zero real addresses or keys)
// ============================================================================

fn synth_raw_addr(fill: u8) -> String {
    TonAddress::new(0, [fill; 32]).to_raw()
}

fn synth_friendly_addr(fill: u8, bounceable: bool, testnet: bool) -> String {
    TonAddress::new(0, [fill; 32]).to_friendly(bounceable, testnet)
}

fn synth_web_doc(url: &str, access_hash: i64) -> tl_enums::WebDocument {
    tl_enums::WebDocument::WebDocument(tl_types::WebDocument {
        url: url.to_owned(),
        access_hash,
        size: 1024,
        mime_type: "image/png".to_owned(),
        attributes: Vec::new(),
    })
}

fn synth_web_doc_no_proxy(url: &str) -> tl_enums::WebDocument {
    tl_enums::WebDocument::NoProxy(tl_types::WebDocumentNoProxy {
        url: url.to_owned(),
        size: 1024,
        mime_type: "image/png".to_owned(),
        attributes: Vec::new(),
    })
}

fn synth_unique_gift(id: i64, slug: &str, title: &str) -> tl_enums::StarGift {
    tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
        require_premium: false,
        resale_ton_only: false,
        theme_available: false,
        burned: false,
        crafted: false,
        id,
        gift_id: id + 1000,
        title: title.to_owned(),
        slug: slug.to_owned(),
        num: 42,
        owner_id: None,
        owner_name: None,
        owner_address: None,
        attributes: Vec::new(),
        availability_issued: 100,
        availability_total: 1000,
        gift_address: None,
        resell_amount: None,
        released_by: None,
        value_amount: None,
        value_currency: None,
        value_usd_amount: None,
        theme_peer: None,
        peer_color: None,
        host_id: None,
        offer_min_stars: None,
        craft_chance_permille: None,
    })
}

fn synth_srp_password() -> tl_enums::InputCheckPasswordSRP {
    tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(tl_types::InputCheckPasswordSRP {
        srp_id: 9_876_543,
        a: vec![0xAA; 32],
        m1: vec![0xBB; 32],
    })
}

// ============================================================================
// Test 1: toncenter.* (2 methods) + ToncenterTransport MTProto vs HTTP selection
// ============================================================================

#[tokio::test]
async fn toncenter_mtproto_proxy_and_transport_selection() {
    let addr = synth_friendly_addr(0x11, false, false);
    let raw_addr = synth_raw_addr(0x11);
    let addr_for_srv = addr.clone();
    let raw_for_srv = raw_addr.clone();

    let (server, wallet) = make_wallet_client(4, move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::toncenter::PerformApiRequest::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 4, "toncenter proxy must route to webfile DC 4");
            let req = tl_fns::toncenter::PerformApiRequest::from_bytes(&rpc.body).unwrap();
            let json_body = if req.endpoint == "/api/v2/getAddressInformation" {
                json!({
                    "ok": true,
                    "result": {
                        "balance": "4200000000",
                        "state": "active",
                        "last_transaction_id": { "lt": "777", "hash": "h777" }
                    }
                })
                .to_string()
            } else if req.endpoint == "/api/emulate/v1/emulateTrace" {
                json!({
                    "mc_block_seqno": 99,
                    "trace": { "tx_hash": "root1" },
                    "transactions": {
                        "root1": {
                            "account": raw_for_srv,
                            "total_fees": "3100000",
                            "description": {
                                "type": "ord",
                                "aborted": false,
                                "compute_ph": { "success": true, "exit_code": 0 },
                                "action": { "success": true, "result_code": 0 }
                            }
                        }
                    }
                })
                .to_string()
            } else if req.endpoint == "/api/v3/pendingTransactions" {
                json!({ "error": EMULATED_TRACES_NOT_FOUND }).to_string()
            } else {
                json!({ "ok": true, "address": addr_for_srv }).to_string()
            };
            ServerAction::ok(&tl_enums::toncenter::ApiResponse::ApiResponse(
                tl_types::toncenter::ApiResponse {
                    response: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: json_body }),
                },
            ))
        }
        x if x == tl_fns::toncenter::GetStreamingUrl::CONSTRUCTOR_ID => {
            assert_eq!(rpc.dc_id, 4);
            ServerAction::ok(&tl_enums::toncenter::StreamingUrl::StreamingUrl(
                tl_types::toncenter::StreamingUrl {
                    url: "wss://stream.example.invalid:8443/v2/ws?tok=xyz".to_owned(),
                    expires: 1_750_000_999,
                },
            ))
        }
        other => panic!("unexpected ctor 0x{other:08x}"),
    });

    // Verify shifted DC helpers match tdesktop (`kToncenterDcShift = 0x08`, `kWalletShareDcShift = 0x09`).
    assert_eq!(toncenter_shifted_dc_id(4), Some(80_004));
    assert_eq!(toncenter_shifted_dc_id(0), None);
    assert_eq!(toncenter_shifted_dc_id(10_000), None);
    assert_eq!(wallet_share_shifted_dc_id(5), Some(90_005));
    assert_eq!(wallet_share_shifted_dc_id(-1), None);

    // 1a. GET with query (`flags = 1<<1 = 0x02`, no post, no payload).
    let tc_client = wallet.toncenter_client();
    let info = tc_client.get_address_information(&addr).await.unwrap();
    assert_eq!(info.balance_nano, 4_200_000_000);
    assert_eq!(info.state, AccountState::Active);
    assert_eq!(info.last_tx_lt, 777);
    let expected_query = format!(
        "address={}",
        url::form_urlencoded::byte_serialize(addr.as_bytes()).collect::<String>()
    );
    let mut expected_get = TlWireBuilder::ctor(0x8d7b_dd61);
    expected_get
        .u32(1 << 1)
        .str("/api/v2/getAddressInformation")
        .str(&expected_query);
    assert_eq!(server.last_rpc().body, expected_get.finish());

    // 1b. POST with payload (`flags = (1<<0)|(1<<2) = 0x05`, no query).
    let emul = tc_client
        .emulate_trace("te6cckEBAQEAAgAAAA==")
        .await
        .unwrap();
    assert_eq!(emul.fee_nano, 3_100_000);
    assert_eq!(emul.account, raw_addr);
    let expected_payload = serde_json::to_string(&json!({
        "boc": "te6cckEBAQEAAgAAAA==",
        "ignore_chksig": true,
        "include_code_data": false,
        "include_address_book": false,
        "include_metadata": false,
        "with_actions": true,
        "mc_block_seqno": serde_json::Value::Null,
    }))
    .unwrap();
    let mut expected_post = TlWireBuilder::ctor(0x8d7b_dd61);
    expected_post
        .u32((1 << 0) | (1 << 2))
        .str("/api/emulate/v1/emulateTrace")
        .str(&expected_payload);
    assert_eq!(server.last_rpc().body, expected_post.finish());

    // 1c. Bare GET with neither query nor payload (`flags = 0`) + JSON error handling.
    let err = wallet
        .toncenter_perform_api_request(GramHttpRequest {
            post: false,
            endpoint: "/api/v3/pendingTransactions".to_owned(),
            query: String::new(),
            payload: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, WalletMtprotoError::ToncenterApi { .. }));
    assert!(is_empty_pending_traces_error(err.error_type().unwrap()));
    let mut expected_bare = TlWireBuilder::ctor(0x8d7b_dd61);
    expected_bare.u32(0).str("/api/v3/pendingTransactions");
    assert_eq!(server.last_rpc().body, expected_bare.finish());

    // 1d. toncenter.getStreamingUrl#cdaf63c7 exact 4 bytes.
    let stream = tc_client.get_streaming_url().await.unwrap();
    assert_eq!(
        stream.url,
        "wss://stream.example.invalid:8443/v2/ws?tok=xyz"
    );
    assert_eq!(stream.expires, 1_750_000_999);
    let ep = stream.endpoint.expect("parsed stream endpoint");
    assert_eq!(ep.host, "stream.example.invalid");
    assert_eq!(ep.port, 8443);
    assert_eq!(ep.request_target, "/v2/ws?tok=xyz");
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0xcdaf_63c7).finish()
    );

    // 1e. Invalid webfile_dc_id fails locally with WEBFILE_DC_INVALID (no RPC sent).
    let rpc_count_before = server.rpcs().len();
    wallet.set_webfile_dc_id(0);
    let dc_err = wallet.toncenter_get_streaming_url().await.unwrap_err();
    assert_eq!(dc_err.error_type(), Some("WEBFILE_DC_INVALID"));
    assert_eq!(server.rpcs().len(), rpc_count_before);
    wallet.set_webfile_dc_id(4);

    // 1f. Routed sendBoc helper (`wallet_engine.cpp:107-126`).
    let send_boc_req = GramHttpRequest {
        post: true,
        endpoint: "/api/v2/jsonRPC".to_owned(),
        query: String::new(),
        payload: serde_json::to_vec(&json!({
            "id": "1",
            "jsonrpc": "2.0",
            "method": "sendBoc",
            "params": { "boc": STANDARD.encode(b"fake-boc-bytes") }
        }))
        .unwrap(),
    };
    assert!(is_send_boc_json_rpc(&send_boc_req));
    assert_eq!(
        extract_routed_send_boc(&send_boc_req).as_deref(),
        Some(b"fake-boc-bytes".as_slice())
    );

    // 1g. Transport selection: default AutoPreferMtproto chooses MTProto when base_url is empty,
    //     and switches to HTTP when an explicit base_url override is configured.
    let default_cfg = WalletAppConfig::default();
    let (kind_mtproto, api_mtproto) = select_toncenter_api(
        ToncenterTransportPolicy::AutoPreferMtproto,
        Some(wallet.rpc().clone()),
        4,
        &default_cfg,
    )
    .unwrap();
    assert_eq!(kind_mtproto, ToncenterTransportKind::Mtproto);
    assert_eq!(
        api_mtproto
            .get_address_information(&addr)
            .await
            .unwrap()
            .balance_nano,
        4_200_000_000
    );

    // Spawn local fake HTTP server for HTTP override check.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = json!({
                "ok": true,
                "result": {
                    "balance": "999000000",
                    "state": "active",
                    "last_transaction_id": { "lt": "12", "hash": "h12" }
                }
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    let http_cfg = WalletAppConfig {
        toncenter_base_url: format!("http://{http_addr}"),
        ..WalletAppConfig::default()
    };
    let (kind_http, api_http) = select_toncenter_api(
        ToncenterTransportPolicy::AutoPreferMtproto,
        Some(wallet.rpc().clone()),
        4,
        &http_cfg,
    )
    .unwrap();
    assert_eq!(kind_http, ToncenterTransportKind::Http);
    assert_eq!(
        api_http
            .get_address_information(&addr)
            .await
            .unwrap()
            .balance_nano,
        999_000_000
    );

    // ForceMtproto ignores HTTP URL; ForceHttp fails if URL is empty.
    let forced_mtproto = select_toncenter_transport(
        ToncenterTransportPolicy::ForceMtproto,
        Some(wallet.rpc().clone()),
        4,
        &http_cfg,
    )
    .unwrap();
    assert_eq!(forced_mtproto.kind, ToncenterTransportKind::Mtproto);
    assert!(
        select_toncenter_transport(
            ToncenterTransportPolicy::ForceHttp,
            Some(wallet.rpc().clone()),
            4,
            &default_cfg,
        )
        .is_err()
    );
}

// ============================================================================
// Test 2: wallet.getState, getUserAddresses, getTransactions*, getNfts, getExistingWaltBalance
// ============================================================================

#[tokio::test]
async fn wallet_state_addresses_transactions_nfts_and_walt_balance_conformance() {
    let wallet_raw = synth_raw_addr(0x21);
    let wallet_friendly = synth_friendly_addr(0x21, false, false);
    let peer_friendly_bounce = synth_friendly_addr(0x22, true, false);
    let peer_raw = synth_raw_addr(0x22);
    let nft_raw = synth_raw_addr(0x33);
    let nft_friendly = synth_friendly_addr(0x33, true, false);
    let enc_comment_bytes = vec![0x5Au8; 64]; // 64 = 48 header + 16 block
    let enc_comment_b64 = STANDARD.encode(&enc_comment_bytes);
    let tx_hash_bytes = [0xABu8; 32];
    let tx_hash_b64url = URL_SAFE_NO_PAD.encode(tx_hash_bytes);

    let state_step = Arc::new(AtomicU64::new(0));
    let state_step_srv = Arc::clone(&state_step);
    let walt_step = Arc::new(AtomicU64::new(0));
    let walt_step_srv = Arc::clone(&walt_step);
    let wallet_friendly_srv = wallet_friendly.clone();
    let peer_friendly_srv = peer_friendly_bounce.clone();
    let nft_friendly_srv = nft_friendly.clone();
    let enc_comment_srv = enc_comment_b64.clone();
    let tx_hash_srv = tx_hash_b64url.clone();

    let (server, wallet) = make_wallet_client(4, move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::wallet::GetState::CONSTRUCTOR_ID => {
            match state_step_srv.fetch_add(1, Ordering::Relaxed) {
                0 => ServerAction::ok(&tl_enums::WalletState::Empty(tl_types::WalletStateEmpty {
                    creating: true,
                })),
                1 => ServerAction::ok(&tl_enums::WalletState::WalletState(tl_types::WalletState {
                    backup_enabled: true,
                    can_export_phrase: true,
                    can_enable_backup: false,
                    address: wallet_friendly_srv.clone(),
                    public_key: vec![0x31; 32],
                    balance: 7_500_000_000,
                })),
                2 => ServerAction::ok(&tl_enums::WalletState::WalletState(tl_types::WalletState {
                    backup_enabled: false,
                    can_export_phrase: false,
                    can_enable_backup: true,
                    address: "not-a-valid-ton-address".to_owned(),
                    public_key: vec![0x31; 32],
                    balance: 0,
                })),
                _ => ServerAction::err(400, "WALLET_UNAVAILABLE"),
            }
        }
        x if x == tl_fns::wallet::GetUserAddresses::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::GetUserAddresses::from_bytes(&rpc.body).unwrap();
            let mut out = Vec::new();
            for u in &req.id {
                if let tl_enums::InputUser::InputUser(iu) = u {
                    out.push(tl_enums::WalletUserAddress::WalletUserAddress(
                        tl_types::WalletUserAddress {
                            user_id: Some(iu.user_id),
                            address: synth_friendly_addr((iu.user_id & 0xFF) as u8, false, false),
                            public_key: vec![0x44; 32],
                        },
                    ));
                }
            }
            for a in &req.addresses {
                out.push(tl_enums::WalletUserAddress::WalletUserAddress(
                    tl_types::WalletUserAddress {
                        user_id: Some(777),
                        address: a.clone(),
                        public_key: vec![0x55; 32],
                    },
                ));
            }
            ServerAction::ok(&tl_enums::wallet::UserAddresses::UserAddresses(
                tl_types::wallet::UserAddresses {
                    addresses: out,
                    users: Vec::new(),
                },
            ))
        }
        x if x == tl_fns::wallet::GetTransactions::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::GetTransactions::from_bytes(&rpc.body).unwrap();
            let next_offset = if req.offset.is_empty() {
                Some("cursor_page_2".to_owned())
            } else {
                // Deliberately repeat requested offset to test tdesktop loop guard.
                Some(req.offset.clone())
            };
            let nft = tl_enums::wallet::NftItem::NftItem(tl_types::wallet::NftItem {
                collection_address: None,
                address: nft_friendly_srv.clone(),
                owner_address: wallet_friendly_srv.clone(),
                index: "9".to_owned(),
                name: Some("Pepe #9".to_owned()),
                description: Some("Collectible".to_owned()),
                image: Some(synth_web_doc("https://cdn.example.invalid/img.png", 101)),
                image_small: Some(synth_web_doc_no_proxy(
                    "https://direct.example.invalid/leak.png",
                )),
                content_url: None,
                lottie: None,
                attributes: Some(vec![tl_enums::wallet::NftAttribute::NftAttribute(
                    tl_types::wallet::NftAttribute {
                        trait_type: "Model".to_owned(),
                        value: "Gold".to_owned(),
                    },
                )]),
                extra: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: r#"{"k":"v"}"#.to_owned(),
                })),
                gift: Some(synth_unique_gift(501, "PlushPepe-9", "Plush Pepe")),
            });
            let txs = vec![
                // 1. Incoming user transfer with encrypted comment & base64url tx_hash.
                tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                    incoming: true,
                    gasless: false,
                    failed: false,
                    key_change: false,
                    comment_encrypted: true,
                    id: "100:aaa".to_owned(),
                    amount: 1_500_000_000,
                    fee: 5_000_000,
                    date: 1_750_000_001,
                    peer: tl_enums::WalletTransactionPeer::User(
                        tl_types::WalletTransactionPeerUser {
                            user_id: 9001,
                            address: peer_friendly_srv.clone(),
                            domain: Some("alice.ton".to_owned()),
                        },
                    ),
                    comment: Some(enc_comment_srv.clone()),
                    tx_hash: Some(tx_hash_srv.clone()),
                    nft: None,
                }),
                // 2. Negative amount + key_change (must fold sign and drop peer, force incoming=false).
                tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                    incoming: true, // intentionally contradictory; key_change + negative sign wins
                    gasless: false,
                    failed: false,
                    key_change: true,
                    comment_encrypted: false,
                    id: "101:bbb".to_owned(),
                    amount: -300_000_000,
                    fee: 3_000_000,
                    date: 1_750_000_002,
                    peer: tl_enums::WalletTransactionPeer::Address(
                        tl_types::WalletTransactionPeerAddress {
                            address: wallet_friendly_srv.clone(),
                            domain: None,
                        },
                    ),
                    comment: None,
                    tx_hash: Some(hex::encode(tx_hash_bytes)),
                    nft: None,
                }),
                // 3. Onramp peer with valid provider + NFT collectible override.
                tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                    incoming: true,
                    gasless: true,
                    failed: false,
                    key_change: false,
                    comment_encrypted: false,
                    id: "102:ccc".to_owned(),
                    amount: 500_000_000,
                    fee: 0,
                    date: 1_750_000_003,
                    peer: tl_enums::WalletTransactionPeer::Onramp(
                        tl_types::WalletTransactionPeerOnramp {
                            address: peer_friendly_srv.clone(),
                            domain: None,
                            provider_name: "  MoonPay  ".to_owned(),
                        },
                    ),
                    comment: Some("gift move".to_owned()),
                    tx_hash: Some("not-a-hash".to_owned()),
                    nft: Some(nft),
                }),
                // 4. TelegramGift peer + Unsupported peer check.
                tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                    incoming: false,
                    gasless: false,
                    failed: true,
                    key_change: false,
                    comment_encrypted: false,
                    id: "103:ddd".to_owned(),
                    amount: 100_000_000,
                    fee: 1_000_000,
                    date: 1_750_000_004,
                    peer: tl_enums::WalletTransactionPeer::TelegramGift(
                        tl_types::WalletTransactionPeerTelegramGift { own_profile: true },
                    ),
                    comment: None,
                    tx_hash: None,
                    nft: None,
                }),
            ];
            ServerAction::ok(&tl_enums::wallet::Transactions::Transactions(
                tl_types::wallet::Transactions {
                    balance: 7_500_000_000,
                    transactions: txs,
                    next_offset,
                    chats: Vec::new(),
                    users: Vec::new(),
                },
            ))
        }
        x if x == tl_fns::wallet::GetTransactionsByIDs::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::wallet::Transactions::Transactions(tl_types::wallet::Transactions {
                balance: 7_500_000_000,
                transactions: vec![tl_enums::WalletTransaction::WalletTransaction(
                    tl_types::WalletTransaction {
                        incoming: false,
                        gasless: false,
                        failed: false,
                        key_change: false,
                        comment_encrypted: false,
                        id: "trace-id-1".to_owned(),
                        amount: 250_000_000,
                        fee: 2_000_000,
                        date: 1_750_000_010,
                        peer: tl_enums::WalletTransactionPeer::Channel(
                            tl_types::WalletTransactionPeerChannel {
                                channel_id: 555,
                                address: peer_friendly_srv.clone(),
                                domain: None,
                            },
                        ),
                        comment: Some("hello".to_owned()),
                        tx_hash: None,
                        nft: None,
                    },
                )],
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            }),
        ),
        x if x == tl_fns::wallet::GetTransactionsByMsgHash::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::GetTransactionsByMsgHash::from_bytes(&rpc.body).unwrap();
            let txs = if req.msg_hash == vec!["ambiguous_hash".to_owned()] {
                vec![
                    tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                        incoming: false,
                        gasless: false,
                        failed: false,
                        key_change: false,
                        comment_encrypted: false,
                        id: "1:first".to_owned(),
                        amount: 100,
                        fee: 1,
                        date: 10,
                        peer: tl_enums::WalletTransactionPeer::Unsupported,
                        comment: None,
                        tx_hash: None,
                        nft: None,
                    }),
                    tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                        incoming: false,
                        gasless: false,
                        failed: false,
                        key_change: false,
                        comment_encrypted: false,
                        id: "2:second".to_owned(),
                        amount: 100,
                        fee: 1,
                        date: 11,
                        peer: tl_enums::WalletTransactionPeer::Unsupported,
                        comment: None,
                        tx_hash: None,
                        nft: None,
                    }),
                ]
            } else {
                // Self-transfer returns both incoming and outgoing; lookup picks the outgoing one.
                vec![
                    tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                        incoming: true,
                        gasless: false,
                        failed: false,
                        key_change: false,
                        comment_encrypted: false,
                        id: "99:in".to_owned(),
                        amount: 100,
                        fee: 0,
                        date: 10,
                        peer: tl_enums::WalletTransactionPeer::Unsupported,
                        comment: None,
                        tx_hash: None,
                        nft: None,
                    }),
                    tl_enums::WalletTransaction::WalletTransaction(tl_types::WalletTransaction {
                        incoming: false,
                        gasless: true,
                        failed: false,
                        key_change: false,
                        comment_encrypted: false,
                        id: "99:out".to_owned(),
                        amount: 100,
                        fee: 0,
                        date: 10,
                        peer: tl_enums::WalletTransactionPeer::Unsupported,
                        comment: None,
                        tx_hash: None,
                        nft: None,
                    }),
                ]
            };
            ServerAction::ok(&tl_enums::wallet::Transactions::Transactions(
                tl_types::wallet::Transactions {
                    balance: 7_500_000_000,
                    transactions: txs,
                    next_offset: None,
                    chats: Vec::new(),
                    users: Vec::new(),
                },
            ))
        }
        x if x == tl_fns::wallet::GetNfts::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::GetNfts::from_bytes(&rpc.body).unwrap();
            let valid_nft = tl_enums::wallet::NftItem::NftItem(tl_types::wallet::NftItem {
                collection_address: None,
                address: nft_friendly_srv.clone(),
                owner_address: wallet_friendly_srv.clone(),
                index: "1".to_owned(),
                name: Some("Gift One".to_owned()),
                description: None,
                image: Some(synth_web_doc("https://cdn.example.invalid/a.png", 5)),
                image_small: Some(synth_web_doc_no_proxy("https://leak.example.invalid/b.png")),
                content_url: None,
                lottie: None,
                attributes: None,
                extra: None,
                gift: Some(synth_unique_gift(77, "StarHat-77", "Star Hat")),
            });
            let bad_nft = tl_enums::wallet::NftItem::NftItem(tl_types::wallet::NftItem {
                collection_address: None,
                address: "bad-address".to_owned(),
                owner_address: wallet_friendly_srv.clone(),
                index: "2".to_owned(),
                name: None,
                description: None,
                image: None,
                image_small: None,
                content_url: None,
                lottie: None,
                attributes: None,
                extra: None,
                gift: None,
            });
            ServerAction::ok(&tl_enums::wallet::NftItems::NftItems(
                tl_types::wallet::NftItems {
                    // Duplicate valid_nft + unparseable bad_nft: parser must dedupe & drop bad.
                    items: vec![valid_nft.clone(), valid_nft, bad_nft],
                    next_offset: if req.offset.is_empty() {
                        Some("nft_next".to_owned())
                    } else {
                        Some(req.offset)
                    },
                },
            ))
        }
        x if x == tl_fns::wallet::GetExistingWaltBalance::CONSTRUCTOR_ID => {
            let has = walt_step_srv.fetch_add(1, Ordering::Relaxed) == 0;
            ServerAction::ok(&tl_enums::wallet::ExistingBalance::ExistingBalance(
                tl_types::wallet::ExistingBalance {
                    has_balance: has,
                    url: "https://t.me/wallet?start=migrate".to_owned(),
                },
            ))
        }
        other => panic!("unexpected ctor 0x{other:08x}"),
    });

    // 2a. wallet.getState#ab83b33a (4 states: Empty, Ready, AddressUnreadable, WALLET_UNAVAILABLE).
    assert_eq!(
        wallet.get_state().await.unwrap(),
        ServerWalletState::Empty { creating: true }
    );
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0xab83_b33a).finish()
    );
    match wallet.get_state().await.unwrap() {
        ServerWalletState::Ready {
            backup_enabled,
            can_export_phrase,
            can_enable_backup,
            address,
            balance_nano,
            ..
        } => {
            assert!(backup_enabled);
            assert!(can_export_phrase);
            assert!(!can_enable_backup);
            assert_eq!(address, wallet_raw);
            assert_eq!(balance_nano, 7_500_000_000);
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert!(matches!(
        wallet.get_state().await.unwrap(),
        ServerWalletState::AddressUnreadable { .. }
    ));
    let unavail = wallet.get_state().await.unwrap_err();
    assert!(unavail.is_wallet_unavailable());

    // 2b. wallet.getUserAddresses#5275dfdd (force=true, reverse lookup, chunking 105 -> 100 + 5).
    let forced = wallet
        .force_resolve_user_address(
            42,
            tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: 42,
                access_hash: 999,
            }),
        )
        .await
        .unwrap();
    assert_eq!(forced.user_id, Some(42));
    assert_eq!(forced.address, synth_raw_addr(42));
    let mut expected_force = TlWireBuilder::ctor(0x5275_dfdd);
    expected_force
        .u32(1 << 0) // flags.0 = force
        .vec_header(1)
        .u32(0xf211_58c6) // inputUser#f21158c6
        .i64(42)
        .i64(999)
        .vec_header(0);
    assert_eq!(server.last_rpc().body, expected_force.finish());

    let owner = wallet
        .resolve_address_owner(&wallet_friendly)
        .await
        .unwrap()
        .expect("owner found");
    assert_eq!(owner.user_id, Some(777));
    assert_eq!(owner.address, wallet_raw);
    let mut expected_rev = TlWireBuilder::ctor(0x5275_dfdd);
    expected_rev
        .u32(0)
        .vec_header(0)
        .vec_header(1)
        .str(&wallet_raw);
    assert_eq!(server.last_rpc().body, expected_rev.finish());

    server.clear_rpcs();
    let mut batch = Vec::new();
    for i in 1..=105_i64 {
        batch.push((
            i,
            tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: i,
                access_hash: i * 10,
            }),
        ));
    }
    // Add duplicate user 1 to verify deduplication.
    batch.push((
        1,
        tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: 1,
            access_hash: 10,
        }),
    ));
    let resolved_map = wallet.resolve_user_addresses_chunked(batch).await.unwrap();
    assert_eq!(resolved_map.len(), 105);
    assert_eq!(
        server.rpcs().len(),
        2,
        "105 unique users must split into chunks of 100 and 5"
    );

    // 2c. wallet.getTransactions#a8830a83 (flags, parsing, repeated-offset guard).
    let page1 = wallet
        .get_transactions(TransactionDirectionFilter::InboundOnly, "", 50)
        .await
        .unwrap();
    let mut expected_tx = TlWireBuilder::ctor(0xa883_0a83);
    expected_tx.u32(1 << 0).str("").i32(50);
    assert_eq!(server.last_rpc().body, expected_tx.finish());
    assert_eq!(page1.next_offset.as_deref(), Some("cursor_page_2"));
    assert_eq!(page1.transactions.len(), 4);

    assert_eq!(
        page1.transactions[0].kind,
        ServerTransactionKind::PeerTransfer
    );
    assert!(page1.transactions[0].incoming);
    assert_eq!(
        page1.transactions[0].encrypted_comment_payload.as_deref(),
        Some(enc_comment_bytes.as_slice())
    );
    assert_eq!(page1.transactions[0].tx_hash, Some(tx_hash_bytes));
    assert_eq!(
        page1.transactions[0].peer,
        Some(ServerTransactionPeer::User {
            user_id: 9001,
            address: peer_raw.clone(),
            bounceable: true,
            domain: Some("alice.ton".to_owned()),
        })
    );

    // Key change folds negative amount, forces incoming=false, suppresses peer.
    assert_eq!(page1.transactions[1].kind, ServerTransactionKind::KeyChange);
    assert!(!page1.transactions[1].incoming);
    assert_eq!(page1.transactions[1].amount_nano, 300_000_000);
    assert!(page1.transactions[1].peer.is_none());
    assert_eq!(page1.transactions[1].tx_hash, Some(tx_hash_bytes));

    // NFT overrides kind to Collectible and drops WebDocumentNoProxy.
    assert_eq!(
        page1.transactions[2].kind,
        ServerTransactionKind::Collectible
    );
    assert!(page1.transactions[2].gasless);
    assert!(page1.transactions[2].tx_hash.is_none());
    let tx_nft = page1.transactions[2].nft.as_ref().unwrap();
    assert_eq!(tx_nft.item.address, nft_raw);
    assert_eq!(tx_nft.item.kind, NftKind::TelegramGift);
    assert!(tx_nft.image.is_some());
    assert!(
        tx_nft.image_small.is_none(),
        "webDocumentNoProxy must be dropped"
    );

    // TelegramGift peer.
    assert_eq!(
        page1.transactions[3].peer,
        Some(ServerTransactionPeer::TelegramGift {
            target: GiftMoveTarget::OwnProfile
        })
    );
    assert!(page1.transactions[3].failed);

    // Second page with OutboundOnly (`flags = 1<<1`) repeating cursor -> next_offset becomes None!
    let page2 = wallet
        .get_transactions(
            TransactionDirectionFilter::OutboundOnly,
            "cursor_page_2",
            25,
        )
        .await
        .unwrap();
    let mut expected_tx2 = TlWireBuilder::ctor(0xa883_0a83);
    expected_tx2.u32(1 << 1).str("cursor_page_2").i32(25);
    assert_eq!(server.last_rpc().body, expected_tx2.finish());
    assert!(
        page2.next_offset.is_none(),
        "repeated cursor must terminate paging"
    );

    // 2d. wallet.getTransactionsByIDs#811ceab6.
    let by_id = wallet
        .get_transactions_by_ids(vec!["trace-id-1".to_owned()])
        .await
        .unwrap();
    let mut expected_by_id = TlWireBuilder::ctor(0x811c_eab6);
    expected_by_id.vec_header(1).str("trace-id-1");
    assert_eq!(server.last_rpc().body, expected_by_id.finish());
    assert_eq!(by_id.transactions.len(), 1);
    assert!(matches!(
        by_id.transactions[0].peer,
        Some(ServerTransactionPeer::Channel {
            channel_id: 555,
            ..
        })
    ));

    // 2e. wallet.getTransactionsByMsgHash#a6bb795d + submitted lookup attribution.
    let by_hash = wallet
        .get_transactions_by_msg_hash(vec!["opaque_msg_hash_1".to_owned()])
        .await
        .unwrap();
    let mut expected_by_hash = TlWireBuilder::ctor(0xa6bb_795d);
    expected_by_hash.vec_header(1).str("opaque_msg_hash_1");
    assert_eq!(server.last_rpc().body, expected_by_hash.finish());
    assert_eq!(
        extract_submitted_lookup_tx_id(&by_hash).unwrap().as_deref(),
        Some("99:out")
    );
    let ambig = wallet
        .get_transactions_by_msg_hash(vec!["ambiguous_hash".to_owned()])
        .await
        .unwrap();
    assert_eq!(
        extract_submitted_lookup_tx_id(&ambig)
            .unwrap_err()
            .error_type(),
        Some("WALLET_LOOKUP_AMBIGUOUS")
    );

    // 2f. wallet.getNfts#8c12f3a4 (deduplication, WebDocumentNoProxy drop, repeated offset guard).
    let nfts1 = wallet.get_nfts("", 20).await.unwrap();
    let mut expected_nfts = TlWireBuilder::ctor(0x8c12_f3a4);
    expected_nfts.str("").i32(20);
    assert_eq!(server.last_rpc().body, expected_nfts.finish());
    assert_eq!(
        nfts1.items.len(),
        1,
        "duplicates and invalid addresses dropped"
    );
    assert_eq!(nfts1.items[0].item.address, nft_raw);
    assert_eq!(nfts1.items[0].item.key, "StarHat-77");
    assert!(nfts1.items[0].image.is_some());
    assert!(nfts1.items[0].image_small.is_none());
    assert_eq!(nfts1.next_offset.as_deref(), Some("nft_next"));

    let nfts2 = wallet.get_nfts("nft_next", 20).await.unwrap();
    assert!(nfts2.next_offset.is_none());

    // 2g. wallet.getExistingWaltBalance#691f4360.
    let wb1 = wallet.get_existing_walt_balance().await.unwrap();
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0x691f_4360).finish()
    );
    assert!(wb1.has_balance);
    assert_eq!(
        wb1.url.as_deref(),
        Some("https://t.me/wallet?start=migrate")
    );
    let wb2 = wallet.get_existing_walt_balance().await.unwrap();
    assert!(!wb2.has_balance);
    assert!(wb2.url.is_none());
}

// ============================================================================
// Test 3: Cloud backup (5 methods) + getProofChallenge + replaceWallet (7 methods total)
// ============================================================================

#[tokio::test]
async fn wallet_cloud_backup_proof_and_replace_conformance() {
    // Generate throwaway 24-word mnemonic and throwaway holder X25519 keys for DC 2, DC 4, DC 5.
    let throwaway_words = generate_mnemonic("").expect("throwaway mnemonic");
    let holder_dcs: Vec<DcId> = vec![2, 4, 5];
    let holder_sks: Vec<[u8; 32]> = (0..3)
        .map(|_| BoxSecretKey::generate(&mut OsRng).to_bytes())
        .collect();
    let holder_pks: Vec<[u8; 32]> = holder_sks
        .iter()
        .map(|sk| *BoxSecretKey::from(*sk).public_key().as_bytes())
        .collect();

    // Server-side storage of decrypted XOR shares per holder DC (simulating Telegram's holder DCs).
    let stored_shares: Arc<Mutex<HashMap<DcId, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
    let stored_shares_srv = Arc::clone(&stored_shares);
    let holder_dcs_srv = holder_dcs.clone();
    let holder_sks_srv = holder_sks.clone();
    let holder_pks_srv = holder_pks.clone();
    let ready_addr = synth_friendly_addr(0x41, false, false);
    let ready_addr_srv = ready_addr.clone();

    let (server, wallet) = make_wallet_client(4, move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::wallet::GetBackupHolderDcs::CONSTRUCTOR_ID => {
            let list: Vec<tl_enums::wallet::HolderDc> = holder_dcs_srv
                .iter()
                .zip(holder_pks_srv.iter())
                .map(|(&dc, pk)| {
                    tl_enums::wallet::HolderDc::HolderDc(tl_types::wallet::HolderDc {
                        dc,
                        public_key: pk.to_vec(),
                    })
                })
                .collect();
            ServerAction::ok(&list)
        }
        x if x == tl_fns::wallet::GetProofChallenge::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::wallet::ProofChallenge::ProofChallenge(tl_types::wallet::ProofChallenge {
                payload: "challenge-payload-001".to_owned(),
                expires: 1_750_001_000,
                domain: "telegram.org".to_owned(),
            }),
        ),
        x if x == tl_fns::wallet::EnableBackup::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::EnableBackup::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.parts.len(), holder_dcs_srv.len());
            let mut map = stored_shares_srv.lock().unwrap();
            for ((&dc, sk), sealed) in holder_dcs_srv
                .iter()
                .zip(holder_sks_srv.iter())
                .zip(req.parts.iter())
            {
                let opened = decrypt_share_for_holder(sk, sealed)
                    .expect("holder DC must decrypt client's sealed share");
                map.insert(dc, opened);
            }
            ServerAction::ok(&tl_enums::WalletState::WalletState(tl_types::WalletState {
                backup_enabled: true,
                can_export_phrase: true,
                can_enable_backup: false,
                address: ready_addr_srv.clone(),
                public_key: vec![0x61; 32],
                balance: 1_000_000_000,
            }))
        }
        x if x == tl_fns::wallet::ExportSecretPhrase::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::ExportSecretPhrase::from_bytes(&rpc.body).unwrap();
            if req.password.is_none() {
                ServerAction::err(400, "PASSWORD_MISSING")
            } else {
                ServerAction::ok(&tl_enums::wallet::SecretPhraseParts::SecretPhraseParts(
                    tl_types::wallet::SecretPhraseParts {
                        token: "export-tok-xyz".to_owned(),
                        dcs: holder_dcs_srv.clone(),
                    },
                ))
            }
        }
        x if x == tl_fns::wallet::FetchEncryptedSecretPhrasePart::CONSTRUCTOR_ID => {
            let req =
                tl_fns::wallet::FetchEncryptedSecretPhrasePart::from_bytes(&rpc.body).unwrap();
            assert_eq!(req.token, "export-tok-xyz");
            let eph_pk: [u8; 32] = req.public_key.try_into().expect("32-byte eph key");
            let share = stored_shares_srv
                .lock()
                .unwrap()
                .get(&rpc.dc_id)
                .cloned()
                .expect("stored share for holder DC");
            let re_encrypted = encrypt_share_for_holder(&eph_pk, &share).unwrap();
            ServerAction::ok(
                &tl_enums::wallet::EncryptedSecretPhrasePart::EncryptedSecretPhrasePart(
                    tl_types::wallet::EncryptedSecretPhrasePart { data: re_encrypted },
                ),
            )
        }
        x if x == tl_fns::wallet::DisableBackup::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::WalletState::WalletState(tl_types::WalletState {
                backup_enabled: false,
                can_export_phrase: false,
                can_enable_backup: true,
                address: ready_addr_srv.clone(),
                public_key: vec![0x61; 32],
                balance: 1_000_000_000,
            }))
        }
        x if x == tl_fns::wallet::ReplaceWallet::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::WalletState::WalletState(tl_types::WalletState {
                backup_enabled: false,
                can_export_phrase: false,
                can_enable_backup: true,
                address: ready_addr_srv.clone(),
                public_key: vec![0x77; 32],
                balance: 0,
            }))
        }
        other => panic!("unexpected ctor 0x{other:08x}"),
    });

    // 3a. wallet.getProofChallenge#2025e697 exact 4 bytes.
    let challenge = wallet.get_proof_challenge().await.unwrap();
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0x2025_e697).finish()
    );
    assert_eq!(challenge.payload, "challenge-payload-001");
    assert_eq!(challenge.domain, "telegram.org");

    // 3b. End-to-end cloud backup enable (`getBackupHolderDcs` -> seal XOR shares -> `enableBackup`).
    let proof_key = vec![0x61u8; 32];
    let proof = OwnershipProofInput {
        timestamp: 1_750_000_500,
        signature: vec![0xCD; 64],
    };
    server.clear_rpcs();
    let enabled_state = wallet
        .enable_cloud_backup_from_words(
            &throwaway_words,
            Some(proof_key.clone()),
            Some(proof.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(
        enabled_state,
        ServerWalletState::Ready {
            backup_enabled: true,
            ..
        }
    ));
    let rpcs = server.rpcs();
    assert_eq!(rpcs.len(), 2);
    assert_eq!(rpcs[0].body, TlWireBuilder::ctor(0xd179_d494).finish());
    // Assert exact TL wire bytes of wallet.enableBackup#4500c525 (`flags = (1<<1)|(1<<2)`, never bit 0!).
    let decoded_enable = tl_fns::wallet::EnableBackup::from_bytes(&rpcs[1].body).unwrap();
    let mut expected_enable = TlWireBuilder::ctor(0x4500_c525);
    expected_enable
        .u32((1 << 1) | (1 << 2))
        .vec_header(decoded_enable.parts.len());
    for part in &decoded_enable.parts {
        expected_enable.bytes(part);
    }
    expected_enable
        .bytes(&proof_key)
        .u32(0x60bc_cb0d) // walletOwnershipProof
        .i32(1_750_000_500)
        .bytes(&[0xCD; 64]);
    assert_eq!(rpcs[1].body, expected_enable.finish());

    // 3c. wallet.exportSecretPhrase#9077c9ac: first without password -> PASSWORD_MISSING!
    let err_no_pw = wallet.export_secret_phrase(None).await.unwrap_err();
    assert!(err_no_pw.is_password_missing());
    let mut expected_export_no_pw = TlWireBuilder::ctor(0x9077_c9ac);
    expected_export_no_pw.u32(0);
    assert_eq!(server.last_rpc().body, expected_export_no_pw.finish());

    // 3d. End-to-end cloud backup export (`exportSecretPhrase` + 3x `fetchEncryptedSecretPhrasePart`
    //     on shifted holder DCs 2, 4, 5 -> decrypt -> XOR combine -> exact 24 words!).
    server.clear_rpcs();
    let srp = synth_srp_password();
    let recovered = wallet
        .export_cloud_backup_words(Some(srp.clone()))
        .await
        .unwrap();
    assert_eq!(recovered, throwaway_words);
    let export_rpcs = server.rpcs();
    assert_eq!(export_rpcs.len(), 4);
    let mut expected_export_pw = TlWireBuilder::ctor(0x9077_c9ac);
    expected_export_pw.u32(1 << 0).raw(&srp.to_bytes());
    assert_eq!(export_rpcs[0].body, expected_export_pw.finish());
    // Verify each holder fetch targeted its exact holder DC (2, 4, 5) on distinct shifted sessions
    // (different from the main DC 2 session used by wallet.exportSecretPhrase) with exact TL wire bytes.
    let main_session_id = export_rpcs[0].session_id;
    let fetch_dcs: Vec<DcId> = export_rpcs[1..].iter().map(|r| r.dc_id).collect();
    assert_eq!(fetch_dcs, vec![2, 4, 5]);
    for r in &export_rpcs[1..] {
        assert_ne!(
            r.session_id, main_session_id,
            "wallet.fetchEncryptedSecretPhrasePart must run on shifted WALLET_SHARE_DC_SHIFT session"
        );
        assert_eq!(r.ctor, 0xf539_25cf);
        let parsed = tl_fns::wallet::FetchEncryptedSecretPhrasePart::from_bytes(&r.body).unwrap();
        let mut expected_fetch = TlWireBuilder::ctor(0xf539_25cf);
        expected_fetch
            .str("export-tok-xyz")
            .bytes(&parsed.public_key);
        assert_eq!(r.body, expected_fetch.finish());
    }

    // 3e. wallet.disableBackup#0b0bf0da (password mode `flags=1`, and proof mode `flags=6`).
    let disabled1 = wallet
        .disable_backup(DisableBackupRequest {
            password: Some(srp.clone()),
            new_public_key: None,
            proof: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        disabled1,
        ServerWalletState::Ready {
            backup_enabled: false,
            ..
        }
    ));
    let mut expected_dis_pw = TlWireBuilder::ctor(0x0b0b_f0da);
    expected_dis_pw.u32(1 << 0).raw(&srp.to_bytes());
    assert_eq!(server.last_rpc().body, expected_dis_pw.finish());

    let _ = wallet
        .disable_backup(DisableBackupRequest {
            password: None,
            new_public_key: Some(proof_key.clone()),
            proof: Some(proof.clone()),
        })
        .await
        .unwrap();
    let mut expected_dis_proof = TlWireBuilder::ctor(0x0b0b_f0da);
    expected_dis_proof
        .u32((1 << 1) | (1 << 2))
        .bytes(&proof_key)
        .u32(0x60bc_cb0d)
        .i32(1_750_000_500)
        .bytes(&[0xCD; 64]);
    assert_eq!(server.last_rpc().body, expected_dis_proof.finish());

    // 3f. wallet.replaceWallet#d8c72eec (InputWalletNew + InputWalletImported with anchor).
    let _ = wallet
        .replace_wallet(WalletReplacement::New, None)
        .await
        .unwrap();
    let mut expected_rep_new = TlWireBuilder::ctor(0xd8c7_2eec);
    expected_rep_new.u32(0).u32(0x63a4_40dc); // inputWalletNew
    assert_eq!(server.last_rpc().body, expected_rep_new.finish());

    let anchor_key = vec![0x88u8; 32];
    let _ = wallet
        .replace_wallet(
            WalletReplacement::Imported {
                public_key: proof_key.clone(),
                anchor_public_key: Some(anchor_key.clone()),
                proof: proof.clone(),
            },
            Some(srp.clone()),
        )
        .await
        .unwrap();
    let mut expected_rep_imp = TlWireBuilder::ctor(0xd8c7_2eec);
    expected_rep_imp
        .u32(1 << 0) // password present
        .u32(0x66a6_6a3b) // inputWalletImported
        .u32(1 << 0) // anchor_public_key present
        .bytes(&proof_key)
        .bytes(&anchor_key)
        .u32(0x60bc_cb0d)
        .i32(1_750_000_500)
        .bytes(&[0xCD; 64])
        .raw(&srp.to_bytes());
    assert_eq!(server.last_rpc().body, expected_rep_imp.finish());
}

// ============================================================================
// Test 4: sendTransfer, getGaslessInfo, setCardStickers, payments.* (6 methods) + Updates
// ============================================================================

#[tokio::test]
async fn wallet_transfer_gasless_card_and_payments_conformance() {
    let own_raw = synth_raw_addr(0x51);
    let other_raw = synth_raw_addr(0x52);
    let relayer_friendly = synth_friendly_addr(0x53, true, false);
    let relayer_raw = synth_raw_addr(0x53);
    let relayer_testnet = synth_friendly_addr(0x53, true, true);
    let sticker_nft_raw = synth_raw_addr(0x54);
    let sticker_nft_friendly = synth_friendly_addr(0x54, true, false);
    let escrow_addr = TonAddress::new(0, [0x55; 32]);
    let collectible_addr = TonAddress::new(0, [0x56; 32]);
    let self_addr = TonAddress::new(0, [0x51; 32]);

    // Build a real synthetic TEP-62 NFT transfer payload BOC whose new_owner is `escrow_addr`.
    let nft_msg = build_nft_transfer_message(&NftTransferParams {
        nft_item: collectible_addr,
        new_owner: escrow_addr,
        response_destination: self_addr,
        query_id: 12345,
        attached_ton_nano: 50_000_000,
        forward_ton_nano: 1,
        forward_comment: None,
    })
    .unwrap();
    let deposit_payload_bytes = serialize_boc(nft_msg.body.as_ref().unwrap()).unwrap();

    let send_step = Arc::new(AtomicU64::new(0));
    let send_step_srv = Arc::clone(&send_step);
    let gasless_step = Arc::new(AtomicU64::new(0));
    let gasless_step_srv = Arc::clone(&gasless_step);
    let relayer_friendly_srv = relayer_friendly.clone();
    let relayer_testnet_srv = relayer_testnet.clone();
    let sticker_nft_friendly_srv = sticker_nft_friendly.clone();
    let own_raw_srv = own_raw.clone();
    let collectible_raw = collectible_addr.to_raw();
    let collectible_raw_srv = collectible_raw.clone();
    let deposit_payload_srv = deposit_payload_bytes.clone();

    let (server, wallet) = make_wallet_client(4, move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::wallet::SendTransfer::CONSTRUCTOR_ID => {
            match send_step_srv.fetch_add(1, Ordering::Relaxed) {
                0 => {
                    let upd = tl_enums::Update::SentWalletTransaction(
                        tl_types::UpdateSentWalletTransaction {
                            gasless: true,
                            msg_hash: "srv-msg-hash-001".to_owned(),
                            transaction: None,
                        },
                    );
                    ServerAction::ok(&tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                        update: upd,
                        date: 1_750_000_100,
                    }))
                }
                1 => {
                    // Conflicting two updateSentWalletTransaction items -> must be rejected!
                    ServerAction::ok(&tl_enums::Updates::Updates(tl_types::Updates {
                        updates: vec![
                            tl_enums::Update::SentWalletTransaction(
                                tl_types::UpdateSentWalletTransaction {
                                    gasless: false,
                                    msg_hash: "hash-a".to_owned(),
                                    transaction: None,
                                },
                            ),
                            tl_enums::Update::SentWalletTransaction(
                                tl_types::UpdateSentWalletTransaction {
                                    gasless: false,
                                    msg_hash: "hash-b".to_owned(),
                                    transaction: None,
                                },
                            ),
                        ],
                        users: Vec::new(),
                        chats: Vec::new(),
                        date: 1_750_000_101,
                        seq: 1,
                    }))
                }
                _ => ServerAction::err(400, "WALLET_TRANSFER_SEND_FAILED"),
            }
        }
        x if x == tl_fns::wallet::GetGaslessInfo::CONSTRUCTOR_ID => {
            let relayer = if gasless_step_srv.fetch_add(1, Ordering::Relaxed) == 0 {
                relayer_friendly_srv.clone()
            } else {
                relayer_testnet_srv.clone()
            };
            let upd = tl_enums::Update::WalletGaslessInfo(tl_types::UpdateWalletGaslessInfo {
                available: true,
                left: 3,
                reset_at: 1_750_086_400,
                min_amount: 200_000_000,
                relayer_address: relayer,
            });
            ServerAction::ok(&tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: upd,
                date: 1_750_000_200,
            }))
        }
        x if x == tl_fns::wallet::SetCardStickers::CONSTRUCTOR_ID => {
            let req = tl_fns::wallet::SetCardStickers::from_bytes(&rpc.body).unwrap();
            let nft_item = tl_enums::wallet::NftItem::NftItem(tl_types::wallet::NftItem {
                collection_address: None,
                address: sticker_nft_friendly_srv.clone(),
                owner_address: own_raw_srv.clone(),
                index: "1".to_owned(),
                name: Some("Card Gift".to_owned()),
                description: None,
                image: None,
                image_small: None,
                content_url: None,
                lottie: None,
                attributes: None,
                extra: None,
                gift: Some(synth_unique_gift(88, "CardGift-88", "Card Gift")),
            });
            let bg_nft = req.bg_nft_address.map(|_| nft_item.clone());
            let stickers = vec![tl_enums::wallet::CardSticker::CardSticker(
                tl_types::wallet::CardSticker {
                    coordinates: tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(
                        tl_types::MediaAreaCoordinates {
                            x: 50.0,
                            y: 50.0,
                            w: 25.0,
                            h: 25.0,
                            rotation: 15.0,
                            radius: Some(4.0),
                        },
                    ),
                    nft: nft_item,
                },
            )];
            ServerAction::ok(&tl_enums::wallet::Card::Card(tl_types::wallet::Card {
                stickers: Some(stickers),
                bg_nft_item: bg_nft,
                address: own_raw_srv.clone(),
                balance: 5_000_000_000,
            }))
        }
        x if x == tl_fns::payments::GetCurrencyRates::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::payments::CurrencyRates::CurrencyRates(tl_types::payments::CurrencyRates {
                rates: vec![
                    tl_enums::CurrencyRate::CurrencyRate(tl_types::CurrencyRate {
                        currency: "usd".to_owned(),
                        rate: 1.0,
                    }),
                    tl_enums::CurrencyRate::CurrencyRate(tl_types::CurrencyRate {
                        currency: "eur".to_owned(),
                        rate: 0.92,
                    }),
                    tl_enums::CurrencyRate::CurrencyRate(tl_types::CurrencyRate {
                        currency: "bad".to_owned(),
                        rate: -5.0,
                    }),
                ],
            }),
        ),
        x if x == tl_fns::payments::DepositStarGift::CONSTRUCTOR_ID => ServerAction::ok(
            &tl_enums::payments::StarGiftDepositPayload::StarGiftDepositPayload(
                tl_types::payments::StarGiftDepositPayload {
                    nft_address: collectible_raw_srv.clone(),
                    payload: deposit_payload_srv.clone(),
                    amount: 150_000_000,
                },
            ),
        ),
        x if x == tl_fns::payments::DisplayStarGift::CONSTRUCTOR_ID => ServerAction::ok_bool(true),
        other => panic!("unexpected ctor 0x{other:08x}"),
    });

    // 4a. wallet.sendTransfer#d9e5d9e2 (dual normal+gasless `flags=1`).
    let sent = wallet
        .send_transfer(SendTransferRequest {
            data_normal: vec![0xDE, 0xAD, 0xBE, 0xEF],
            data_gasless: Some(vec![0xCA, 0xFE]),
            user_id: tl_enums::InputUser::Empty,
            random_id: 0x0102_0304_0506_0708,
        })
        .await
        .unwrap();
    assert_eq!(sent.receipt.msg_hash, "srv-msg-hash-001");
    assert!(sent.receipt.gasless);
    let mut expected_send = TlWireBuilder::ctor(0xd9e5_d9e2);
    expected_send
        .u32(1 << 0) // data_gasless present
        .bytes(&[0xDE, 0xAD, 0xBE, 0xEF])
        .bytes(&[0xCA, 0xFE])
        .u32(0xb988_86cf) // inputUserEmpty
        .i64(0x0102_0304_0506_0708);
    assert_eq!(server.last_rpc().body, expected_send.finish());

    // Conflicting receipt updates rejected; 400 refusal classified as Definite::Rejected.
    let conflict_err = wallet
        .send_transfer(SendTransferRequest {
            data_normal: vec![1, 2],
            data_gasless: None,
            user_id: tl_enums::InputUser::Empty,
            random_id: 99,
        })
        .await
        .unwrap_err();
    assert_eq!(
        conflict_err.error_type(),
        Some("WALLET_TRANSFER_RECEIPT_INVALID")
    );
    let refused_err = wallet
        .send_transfer(SendTransferRequest {
            data_normal: vec![1, 2],
            data_gasless: None,
            user_id: tl_enums::InputUser::Empty,
            random_id: 100,
        })
        .await
        .unwrap_err();
    assert_eq!(
        refused_err.classify_transfer_refusal(),
        TransferSubmissionError::Definite {
            kind: TransferRefusalKind::Rejected,
            token: "WALLET_TRANSFER_SEND_FAILED".to_owned(),
        }
    );
    // Also check Silent (406), KeyMismatch, DataInvalid, and 500 Uncertain.
    assert_eq!(
        WalletMtprotoError::Invocation(RpcError::new(406, "IGNORED").into())
            .classify_transfer_refusal(),
        TransferSubmissionError::Definite {
            kind: TransferRefusalKind::Silent,
            token: "IGNORED".to_owned(),
        }
    );
    assert_eq!(
        WalletMtprotoError::Invocation(RpcError::new(500, "INTERNAL").into())
            .classify_transfer_refusal(),
        TransferSubmissionError::Uncertain {
            diagnostic: "INTERNAL".to_owned(),
        }
    );

    // 4b. wallet.getGaslessInfo#742df0a1 + GaslessTerms eligibility (min amount & no self-transfer).
    let gasless = wallet.get_gasless_info().await.unwrap();
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0x742d_f0a1).finish()
    );
    assert!(gasless.available);
    assert_eq!(gasless.left, 3);
    assert_eq!(gasless.relayer_address, relayer_raw);
    let terms = default_gasless_terms(gasless, Some(own_raw.clone()));
    assert_eq!(terms.effective_min_nanos, 200_000_000);
    assert!(terms.is_eligible_for(200_000_000, &other_raw));
    assert!(!terms.is_eligible_for(199_999_999, &other_raw));
    assert!(
        !terms.is_eligible_for(500_000_000, &own_raw),
        "self-transfers must never be offered gasless"
    );
    // Testnet relayer address must be rejected -> empty relayer -> not usable.
    let testnet_gasless = wallet.get_gasless_info().await.unwrap();
    assert!(testnet_gasless.relayer_address.is_empty());
    assert!(!default_gasless_terms(testnet_gasless, Some(own_raw.clone())).is_usable());

    // 4c. wallet.setCardStickers#068509ac.
    let sticker_input = CardStickerInput {
        coordinates: CardStickerCoordinates {
            x: 50.0,
            y: 50.0,
            w: 25.0,
            h: 25.0,
            rotation: 15.0,
            radius: Some(4.0),
        },
        nft_address: sticker_nft_friendly.clone(),
    };
    let card = wallet
        .set_card_stickers(
            std::slice::from_ref(&sticker_input),
            Some(&sticker_nft_friendly),
        )
        .await
        .unwrap();
    let mut expected_card = TlWireBuilder::ctor(0x0685_09ac);
    expected_card
        .u32(1 << 0) // bg_nft_address present
        .vec_header(1)
        .u32(0x57df_45a7) // inputWalletCardSticker
        .u32(0xcfc9_e002) // mediaAreaCoordinates
        .u32(1 << 0) // radius present
        .f64(50.0)
        .f64(50.0)
        .f64(25.0)
        .f64(25.0)
        .f64(15.0)
        .f64(4.0)
        .str(&sticker_nft_friendly)
        .str(&sticker_nft_friendly);
    assert_eq!(server.last_rpc().body, expected_card.finish());
    assert!(card.stickers_complete);
    assert_eq!(
        card.background_address.as_deref(),
        Some(sticker_nft_raw.as_str())
    );
    // Unmatched bg_nft_address must be rejected client-side.
    assert!(
        wallet
            .set_card_stickers(&[sticker_input], Some(&other_raw))
            .await
            .is_err()
    );

    // 4d. payments.getCurrencyRates#d35983e8 + FiatRateCache.
    let rates = wallet.get_currency_rates().await.unwrap();
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0xd359_83e8).finish()
    );
    assert!(rates.values.contains_key("USD"));
    assert!(rates.values.contains_key("EUR"));
    assert!(!rates.values.contains_key("BAD"));
    let mut fx = FiatRateCache::new("EUR", 5.0);
    fx.apply_rates(rates);
    assert!((fx.current_per_ton().unwrap() - 4.6).abs() < 1e-9);
    assert_eq!(fx.available_currencies(), vec!["EUR", "USD"]);

    // 4e. payments.depositStarGift#c0a2ea9f + TEP-62 escrow extraction & verification.
    let dep = wallet
        .deposit_star_gift(DepositStarGiftRequest {
            hide_sender: true,
            peer: tl_enums::InputPeer::Self_,
            slug: "PlushPepe-42".to_owned(),
        })
        .await
        .unwrap();
    let mut expected_dep = TlWireBuilder::ctor(0xc0a2_ea9f);
    expected_dep
        .u32(1 << 0) // hide_sender
        .u32(0x7da0_7ec9) // inputPeerSelf
        .str("PlushPepe-42");
    assert_eq!(server.last_rpc().body, expected_dep.finish());
    let verified = dep.verify_for_collectible(&collectible_raw).unwrap();
    assert_eq!(verified.destination_escrow, escrow_addr.to_raw());
    assert_eq!(verified.collectible_nft, collectible_raw);
    assert_eq!(verified.amount_nano, 150_000_000);
    assert!(verified.bounce);
    assert_eq!(
        dep.verify_for_collectible(&other_raw)
            .unwrap_err()
            .error_type(),
        Some("GIFT_DEPOSIT_ADDRESS_MISMATCH")
    );

    // 4f. payments.displayStarGift#4aa5f0fe (show with InputPeerSelf `flags=1`, hide `flags=0`).
    assert!(
        wallet
            .display_star_gift("PlushPepe-42", Some(tl_enums::InputPeer::Self_))
            .await
            .unwrap()
    );
    let mut expected_show = TlWireBuilder::ctor(0x4aa5_f0fe);
    expected_show
        .u32(1 << 0)
        .u32(0x7da0_7ec9) // inputPeerSelf
        .str("PlushPepe-42");
    assert_eq!(server.last_rpc().body, expected_show.finish());

    assert!(
        wallet
            .display_star_gift("PlushPepe-42", None)
            .await
            .unwrap()
    );
    let mut expected_hide = TlWireBuilder::ctor(0x4aa5_f0fe);
    expected_hide.u32(0).str("PlushPepe-42");
    assert_eq!(server.last_rpc().body, expected_hide.finish());

    // 4g. Server-pushed update parser (all 5 Layer-233 wallet Update variants).
    assert!(matches!(
        parse_wallet_update(&tl_enums::Update::WalletState(
            tl_types::UpdateWalletState {
                state: tl_enums::WalletState::Empty(tl_types::WalletStateEmpty { creating: false })
            }
        )),
        Some(WalletServerUpdate::WalletState(ServerWalletState::Empty {
            creating: false
        }))
    ));
    assert!(matches!(
        parse_wallet_update(&tl_enums::Update::WalletTonConnectPendingDisconnect(
            tl_types::UpdateWalletTonConnectPendingDisconnect {
                session_ids: vec![10, 20]
            }
        )),
        Some(WalletServerUpdate::TonConnectPendingDisconnect(ids)) if ids == vec![10, 20]
    ));
}

// ============================================================================
// Test 5: All 9 server-side wallet.tonConnect* RPCs
// ============================================================================

#[tokio::test]
async fn wallet_server_ton_connect_all_9_methods_conformance() {
    let submit_resp_step = Arc::new(AtomicU64::new(0));
    let submit_resp_step_srv = Arc::clone(&submit_resp_step);

    let make_session =
        |id: i64, pending: bool, closing: bool, closed: bool, no_proxy_icon: bool| {
            tl_enums::TonConnectSession::TonConnectSession(tl_types::TonConnectSession {
                pending,
                closing,
                closed,
                id,
                dapp_client_id: "dapp-pubkey-hex".to_owned(),
                client_id: Some("wallet-pubkey-hex".to_owned()),
                nonce: vec![0x11; 16],
                manifest: Some(tl_enums::TonConnectManifest::TonConnectManifest(
                    tl_types::TonConnectManifest {
                        url: "https://dapp.example.invalid".to_owned(),
                        name: "Demo dApp".to_owned(),
                        icon: Some(if no_proxy_icon {
                            synth_web_doc_no_proxy("https://dapp.example.invalid/icon.png")
                        } else {
                            synth_web_doc("https://cdn.example.invalid/icon.png", 42)
                        }),
                    },
                )),
                manifest_error: None,
                date: 1_750_000_000,
            })
        };

    let (server, wallet) = make_wallet_client(4, move |_srv, rpc| match rpc.ctor {
        x if x == tl_fns::wallet::TonConnectCreateSession::CONSTRUCTOR_ID => {
            ServerAction::ok(&make_session(1001, true, false, false, false))
        }
        x if x == tl_fns::wallet::TonConnectRegisterKey::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::wallet::TonConnectChallenge::TonConnectChallenge(
                tl_types::wallet::TonConnectChallenge {
                    challenge: vec![0xAA, 0xBB, 0xCC, 0xDD],
                    event_id: 7,
                },
            ))
        }
        x if x == tl_fns::wallet::TonConnectSubmitConnectResult::CONSTRUCTOR_ID => {
            ServerAction::ok_bool(true)
        }
        x if x == tl_fns::wallet::TonConnectGetPending::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::wallet::TonConnectPending::TonConnectPending(
                tl_types::wallet::TonConnectPending {
                    session: make_session(1001, false, false, false, false),
                    requests: vec![tl_enums::TonConnectRequest::TonConnectRequest(
                        tl_types::TonConnectRequest {
                            session_id: 1001,
                            msg_id: 55,
                            body: vec![0x01, 0x02, 0x03],
                            expires: 1_750_000_300,
                            topic: Some("sendTransaction".to_owned()),
                            trace_id: Some("trace-uuid-1".to_owned()),
                        },
                    )],
                },
            ))
        }
        x if x == tl_fns::wallet::TonConnectClaimRequest::CONSTRUCTOR_ID => {
            ServerAction::ok_bool(true)
        }
        x if x == tl_fns::wallet::TonConnectSubmitResponse::CONSTRUCTOR_ID => {
            match submit_resp_step_srv.fetch_add(1, Ordering::Relaxed) {
                0 => ServerAction::ok_bool(true),
                1 => ServerAction::ok_bool(false),
                2 => ServerAction::err(400, "TONCONNECT_REQUEST_EXPIRED"),
                _ => ServerAction::err(400, "TONCONNECT_BRIDGE_BUSY"),
            }
        }
        x if x == tl_fns::wallet::TonConnectNextEventId::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::TonConnectNextEventId::TonConnectNextEventId(
                tl_types::TonConnectNextEventId { event_id: 88 },
            ))
        }
        x if x == tl_fns::wallet::TonConnectCloseSession::CONSTRUCTOR_ID => {
            ServerAction::ok_bool(true)
        }
        x if x == tl_fns::wallet::TonConnectGetSessions::CONSTRUCTOR_ID => {
            ServerAction::ok(&tl_enums::wallet::TonConnectSessions::TonConnectSessions(
                tl_types::wallet::TonConnectSessions {
                    sessions: vec![
                        make_session(1001, false, false, false, false), // Active
                        make_session(1002, true, true, false, true), // Closing beats Pending + NoProxy icon dropped
                        make_session(1003, true, true, true, false), // Closed beats Closing & Pending
                    ],
                },
            ))
        }
        other => panic!("unexpected ctor 0x{other:08x}"),
    });

    // 5.1 wallet.tonConnectCreateSession#cc931046.
    let created = wallet
        .ton_connect_create_session(
            "dapp-pubkey-hex",
            "https://dapp.example.invalid/manifest.json",
        )
        .await
        .unwrap();
    let mut expected_create = TlWireBuilder::ctor(0xcc93_1046);
    expected_create
        .str("dapp-pubkey-hex")
        .str("https://dapp.example.invalid/manifest.json");
    assert_eq!(server.last_rpc().body, expected_create.finish());
    assert_eq!(created.id, 1001);
    assert_eq!(created.status, TonConnectSessionStatus::Pending);
    assert!(created.manifest.as_ref().unwrap().icon.is_some());

    // 5.2 wallet.tonConnectRegisterKey#338bcf1c.
    let reg = wallet
        .ton_connect_register_key(1001, "wallet-pubkey-hex")
        .await
        .unwrap();
    let mut expected_reg = TlWireBuilder::ctor(0x338b_cf1c);
    expected_reg.i64(1001).str("wallet-pubkey-hex");
    assert_eq!(server.last_rpc().body, expected_reg.finish());
    assert_eq!(reg.challenge, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(reg.event_id, 7);

    // 5.3 wallet.tonConnectSubmitConnectResult#c2c00779 (accept+trace_id `flags=2`, error `flags=1`).
    assert!(
        wallet
            .ton_connect_submit_connect_result(TonConnectSubmitConnectResultRequest {
                error: false,
                session_id: 1001,
                challenge_answer: vec![0x10, 0x20],
                body: vec![0x30, 0x40],
                trace_id: Some("trace-1".to_owned()),
            })
            .await
            .unwrap()
    );
    let mut expected_submit_ok = TlWireBuilder::ctor(0xc2c0_0779);
    expected_submit_ok
        .u32(1 << 1)
        .i64(1001)
        .bytes(&[0x10, 0x20])
        .bytes(&[0x30, 0x40])
        .str("trace-1");
    assert_eq!(server.last_rpc().body, expected_submit_ok.finish());

    assert!(
        wallet
            .ton_connect_submit_connect_result(TonConnectSubmitConnectResultRequest {
                error: true,
                session_id: 1001,
                challenge_answer: vec![0x10],
                body: vec![0x30],
                trace_id: Some(String::new()), // empty trace_id must clear flags.1!
            })
            .await
            .unwrap()
    );
    let mut expected_submit_err = TlWireBuilder::ctor(0xc2c0_0779);
    expected_submit_err
        .u32(1 << 0)
        .i64(1001)
        .bytes(&[0x10])
        .bytes(&[0x30]);
    assert_eq!(server.last_rpc().body, expected_submit_err.finish());

    // 5.4 wallet.tonConnectGetPending#11aee065 (by dapp_client_id `flags=1`, by session_id `flags=2`).
    let pend1 = wallet
        .ton_connect_get_pending(TonConnectPendingQuery::ByDappClientId(
            "dapp-pubkey-hex".to_owned(),
        ))
        .await
        .unwrap();
    let mut expected_pend1 = TlWireBuilder::ctor(0x11ae_e065);
    expected_pend1.u32(1 << 0).str("dapp-pubkey-hex");
    assert_eq!(server.last_rpc().body, expected_pend1.finish());
    assert_eq!(pend1.requests.len(), 1);
    assert_eq!(pend1.requests[0].msg_id, 55);
    assert_eq!(pend1.requests[0].topic.as_deref(), Some("sendTransaction"));

    let _ = wallet
        .ton_connect_get_pending(TonConnectPendingQuery::BySessionId(1001))
        .await
        .unwrap();
    let mut expected_pend2 = TlWireBuilder::ctor(0x11ae_e065);
    expected_pend2.u32(1 << 1).i64(1001);
    assert_eq!(server.last_rpc().body, expected_pend2.finish());

    // 5.5 wallet.tonConnectClaimRequest#66b57b41 (accept with answer `flags=1`, late decline `flags=2`).
    assert!(
        wallet
            .ton_connect_claim_request(TonConnectClaimRequest {
                declined: false,
                session_id: 1001,
                msg_id: 55,
                app_request_id: "req-42".to_owned(),
                challenge_answer: Some(vec![0x99, 0x88]),
            })
            .await
            .unwrap()
    );
    let mut expected_claim1 = TlWireBuilder::ctor(0x66b5_7b41);
    expected_claim1
        .u32(1 << 0) // challenge_answer is flags.0
        .i64(1001)
        .i32(55)
        .str("req-42")
        .bytes(&[0x99, 0x88]);
    assert_eq!(server.last_rpc().body, expected_claim1.finish());

    assert!(
        wallet
            .ton_connect_claim_request(TonConnectClaimRequest {
                declined: true,
                session_id: 1001,
                msg_id: 55,
                app_request_id: "req-42".to_owned(),
                challenge_answer: None,
            })
            .await
            .unwrap()
    );
    let mut expected_claim2 = TlWireBuilder::ctor(0x66b5_7b41);
    expected_claim2
        .u32(1 << 1) // declined is flags.1
        .i64(1001)
        .i32(55)
        .str("req-42");
    assert_eq!(server.last_rpc().body, expected_claim2.finish());

    // 5.6 wallet.tonConnectSubmitResponse#fac67d3c + classified outcomes.
    let outcome_ok = wallet
        .ton_connect_submit_response_classified(TonConnectSubmitResponseRequest {
            session_id: 1001,
            msg_id: 55,
            body: vec![0xAB, 0xCD],
            trace_id: Some("trace-uuid-1".to_owned()),
        })
        .await;
    assert_eq!(outcome_ok, TonConnectSubmitOutcome::Delivered);
    let mut expected_sub_resp = TlWireBuilder::ctor(0xfac6_7d3c);
    expected_sub_resp
        .u32(1 << 0)
        .i64(1001)
        .i32(55)
        .bytes(&[0xAB, 0xCD])
        .str("trace-uuid-1");
    assert_eq!(server.last_rpc().body, expected_sub_resp.finish());

    // boolFalse -> Refused; TONCONNECT_REQUEST_EXPIRED -> Refused; other non-dropped 4xx -> RetryLater.
    assert_eq!(
        wallet
            .ton_connect_submit_response_classified(TonConnectSubmitResponseRequest {
                session_id: 1001,
                msg_id: 55,
                body: vec![0x01],
                trace_id: None,
            })
            .await,
        TonConnectSubmitOutcome::Refused
    );
    assert_eq!(
        wallet
            .ton_connect_submit_response_classified(TonConnectSubmitResponseRequest {
                session_id: 1001,
                msg_id: 55,
                body: vec![0x01],
                trace_id: None,
            })
            .await,
        TonConnectSubmitOutcome::Refused
    );
    assert_eq!(
        wallet
            .ton_connect_submit_response_classified(TonConnectSubmitResponseRequest {
                session_id: 1001,
                msg_id: 55,
                body: vec![0x01],
                trace_id: None,
            })
            .await,
        TonConnectSubmitOutcome::RetryLater
    );

    // 5.7 wallet.tonConnectNextEventId#775d7244.
    let next_ev = wallet.ton_connect_next_event_id(1001).await.unwrap();
    let mut expected_ev = TlWireBuilder::ctor(0x775d_7244);
    expected_ev.i64(1001);
    assert_eq!(server.last_rpc().body, expected_ev.finish());
    assert_eq!(next_ev, 88);

    // 5.8 wallet.tonConnectCloseSession#d63f61ce (with body `flags=1` and empty body `flags=0`).
    assert!(
        wallet
            .ton_connect_close_session(1001, Some(vec![0xFE, 0xED]))
            .await
            .unwrap()
    );
    let mut expected_close1 = TlWireBuilder::ctor(0xd63f_61ce);
    expected_close1.u32(1 << 0).i64(1001).bytes(&[0xFE, 0xED]);
    assert_eq!(server.last_rpc().body, expected_close1.finish());

    assert!(
        wallet
            .ton_connect_close_session(1001, Some(Vec::new()))
            .await
            .unwrap()
    );
    let mut expected_close2 = TlWireBuilder::ctor(0xd63f_61ce);
    expected_close2.u32(0).i64(1001);
    assert_eq!(server.last_rpc().body, expected_close2.finish());

    // 5.9 wallet.tonConnectGetSessions#a7ffb56e.
    let sessions = wallet.ton_connect_get_sessions().await.unwrap();
    assert_eq!(
        server.last_rpc().body,
        TlWireBuilder::ctor(0xa7ff_b56e).finish()
    );
    assert_eq!(sessions.len(), 3);
    assert_eq!(sessions[0].status, TonConnectSessionStatus::Active);
    assert_eq!(sessions[1].status, TonConnectSessionStatus::Closing);
    assert!(
        sessions[1].manifest.as_ref().unwrap().icon.is_none(),
        "webDocumentNoProxy icon must be dropped"
    );
    assert_eq!(sessions[2].status, TonConnectSessionStatus::Closed);

    // Verify ServerAction::err classification for SessionGone.
    server.set_handler(|_, _| ServerAction::err(400, "TONCONNECT_SESSION_NOT_FOUND"));
    let gone = wallet.ton_connect_next_event_id(9999).await.unwrap_err();
    assert!(gone.is_ton_connect_session_gone());
    assert!(gone.is_ton_connect_request_dropped());
}
